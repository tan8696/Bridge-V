# 16 · System mode: emulating a whole computer and booting Linux

## What you will learn

- what a real operating system needs from the machine it runs on
- the emulated machine's memory map and devices: timer (CLINT), interrupt controller (PLIC), serial port (UART), power-off device, virtual disk
- **MMIO**: how device registers look like memory
- the **SBI** firmware interface and why Bridge-V implements it in Rust
- the **devicetree**: how Linux learns what hardware exists
- the boot sequence and the **machine loop**
- idling on `WFI`, multiple CPUs (SMP), and real OpenSBI firmware

Files: [`src/system/`](../../src/system/), especially [`machine.rs`](../../src/system/machine.rs) and [`sbi.rs`](../../src/system/sbi.rs).

---

## 1. What an OS needs

To boot an unmodified Linux kernel, the emulated machine must provide:

| Need | Provided by |
|---|---|
| a CPU with M, S and U privilege levels, CSRs, traps and interrupts | `cpu/csr.rs`, `cpu/trap.rs` (file 12) |
| virtual memory hardware (Sv39 page tables) | `mem/mmu.rs`, `mem/tlb.rs` (file 11) |
| RAM | 512 MiB at physical `0x8000_0000` (`--ram`) |
| a timer that can interrupt | CLINT + SBI timer |
| an interrupt controller for devices | PLIC |
| a console | 16550 UART serial port |
| a way to power off | SiFive test finisher ("syscon") |
| firmware services (timer setup, console before drivers, starting other CPUs) | the built-in SBI, or real OpenSBI |
| a description of all the above | a generated devicetree |
| (optional) storage | virtio-blk disk |

The machine is compatible with QEMU's `virt` board, the standard RISC-V virtual machine, so a stock kernel configuration works without changes (decision D16).

---

## 2. The memory map

| Physical address | Size | Device |
|---|---|---|
| `0x0010_0000` | 4 KiB | syscon / test finisher: write `0x5555` = power off, `0x7777` = reset |
| `0x0200_0000` | 64 KiB | **CLINT**: `msip` at +0, `mtimecmp` at +0x4000, `mtime` at +0xBFF8 |
| `0x0C00_0000` | 6 MiB | **PLIC**: interrupt priorities, pending bits, enables, thresholds, claim/complete |
| `0x1000_0000` | 256 B | **UART 16550**: the serial console, interrupt source 10 |
| `0x1000_1000` | 4 KiB | virtio-blk disk (`--disk`), interrupt source 1 |
| `0x8000_0000` | `--ram` (512 MiB) | **RAM** |

### 2.1 MMIO: devices that look like memory

Devices are controlled through **memory-mapped I/O**: the CPU does ordinary loads and stores to special physical addresses, and the device reacts. For example, writing a byte to `0x1000_0000` sends a character out of the serial port.

In Bridge-V, each device implements a small trait ([`src/mem/phys.rs`](../../src/mem/phys.rs)):
```rust
pub trait Mmio: Send {
    fn name(&self) -> &str;
    fn read(&mut self, off: u64, size: u64) -> u64;           // off = offset from the device base
    fn write(&mut self, off: u64, size: u64, val: u64);       // size = 1, 2, 4 or 8 bytes
}
```
and is registered with `mem.add_device(base, size, Box::new(device))`. When the page walker finds that a physical page belongs to a device, the TLB entry gets the `TLB_MMIO` flag, so every access to it takes the slow path, which calls the device's `read`/`write` (file 11, §5.4).

---

## 3. The devices

### 3.1 CLINT: timer and software interrupts ([`clint.rs`](../../src/system/clint.rs))

- `mtime`: a counter that ticks at **10 MHz**, derived from the host's monotonic clock (or from `icount / 10` with `--deterministic`, which makes runs reproducible).
- `mtimecmp[hart]`: when `mtime ≥ mtimecmp`, the machine timer interrupt (MTIP) is pending.
- `msip[hart]`: writing 1 raises a machine software interrupt (used to poke another CPU).

### 3.2 PLIC: platform-level interrupt controller ([`plic.rs`](../../src/system/plic.rs))

Devices (UART, disk) raise **interrupt lines**. The PLIC collects up to 53 of them and decides which CPU context gets interrupted:
- each **source** has a priority
- each **context** (hart 0 M-mode, hart 0 S-mode, hart 1 M-mode, …) has an enable bit per source and a threshold
- a context's output is high when some pending, enabled source has priority above its threshold → MEIP / SEIP in that hart's `mip`
- the handler **claims** the interrupt (reads the claim register → gets the source number), services the device, then **completes** it (writes the number back)

Sources are **level-triggered**: pending while the device's line is high and not being serviced.

### 3.3 UART 16550: the serial console ([`uart16550.rs`](../../src/system/uart16550.rs))

The classic PC serial chip, which Linux has a driver for (`ns16550a`):
- **transmit**: writing to register 0 (THR) sends a byte, which Bridge-V writes straight to your terminal. Transmission is instant, so the "transmitter empty" bit is always set.
- **receive**: a host thread started in `run_boot()` reads your keyboard (stdin) into a queue; register 0 (RBR) returns the next byte, and the "data ready" bit in LSR says whether one is available.
- **interrupts**: when data arrives (and the driver enabled receive interrupts), line 10 goes high → PLIC → SEIP → Linux's UART interrupt handler reads the byte. This is how your typing reaches the BusyBox shell.

### 3.4 Syscon / test finisher ([`syscon.rs`](../../src/system/syscon.rs))

A 32-bit write of `0x5555` means "power off", `code << 16 | 0x3333` means "fail with code", `0x7777` means "reset". Linux's `poweroff` command uses it. The machine loop checks for a request after every slice.

### 3.5 virtio-blk: a virtual disk ([`virtio_blk.rs`](../../src/system/virtio_blk.rs), D58)

`--disk image.img` adds a disk Linux sees as `/dev/vda`. **virtio** is a standard interface designed for virtual machines: instead of emulating a real disk controller, the driver puts requests in a queue in guest memory (a "virtqueue") and notifies the device. The MMIO registers only record the requests; the machine loop processes the queue between slices, reading and writing the host file. Its writes into guest RAM go through `DirectMem`, so if the disk overwrites memory that held translated code, those translations are invalidated like any other code write (file 13).

---

## 4. SBI: the firmware interface

### 4.1 What it is

On RISC-V, the Linux kernel runs in S-mode and asks **M-mode firmware** to do a few machine-level things for it: set the timer, send inter-processor interrupts, print early boot messages, start other CPUs, shut down. The standard interface for these requests is the **SBI** (Supervisor Binary Interface). The kernel calls it with `ecall` from S-mode:

```
a7 = extension ID (EID), a6 = function ID (FID), a0–a5 = arguments
returns: a0 = error code (0 = success, −2 = not supported), a1 = value
```

### 4.2 The built-in SBI (decision D15)

The usual firmware is OpenSBI (a C program that runs in M-mode). Bridge-V instead implements SBI **in Rust** ([`sbi.rs`](../../src/system/sbi.rs)): the fastest way to get Linux booting, and it avoids emulating M-mode firmware at all. How it works:
- `Env.sbi = true` makes `deliver()` stop the engine with `Stop::Ecall` for an S-mode `ecall` (instead of trapping into M-mode).
- The machine loop calls `sbi::call()`, which reads `a7`/`a6`, does the work, writes `a0`/`a1`, and may return an `SbiAction` for the machine to apply.

| Extension | EID | What Bridge-V does |
|---|---|---|
| legacy | 0x00–0x08 | set_timer, console putchar/getchar, … |
| BASE | 0x10 | spec version 2.0, probe which extensions exist, vendor ids |
| TIME | `0x54494D45` ("TIME") | `set_timer(t)`: store the deadline; when `mtime ≥ t`, set STIP |
| IPI | `0x735049` ("sPI") | send software interrupts to other harts (`SbiAction::Ipi`) |
| RFENCE | `0x52464E43` ("RFNC") | remote `fence.i` / `sfence.vma` on other harts |
| HSM | `0x48534D` ("HSM") | hart start / stop / status (SMP) |
| SRST | `0x53525354` ("SRST") | system reset / shutdown |
| DBCN | `0x4442434E` ("DBCN") | debug console write/read |

Because M-mode is "virtual", the CSRs are set up at reset as if firmware had already run: every synchronous exception except S-mode `ecall` is delegated to S-mode (`medeleg`), the supervisor interrupts are delegated (`mideleg = 0x222`), and the counters are readable (`mcounteren = 7`).

### 4.3 Real firmware instead (`--firmware`, D53)

`bridgev boot --firmware fw_dynamic.bin` loads real OpenSBI at the start of RAM and starts every hart in **M-mode** there, as QEMU's reset code does (`a0` = hart id, `a1` = DTB, `a2` = a `fw_dynamic_info` structure telling OpenSBI where the kernel is). Now S-mode `ecall`s trap into OpenSBI like on real hardware. This validates the whole M-mode path (MPRV, PMP CSRs, delegation) against real firmware, and it also runs clean in lockstep.

---

## 5. The devicetree: telling Linux what exists

Linux doesn't probe for hardware on RISC-V; it reads a **devicetree**: a tree of nodes describing the CPUs, memory and devices, with their addresses and interrupt numbers. Bridge-V generates one at boot ([`fdt.rs`](../../src/system/fdt.rs)) in the binary **FDT** format (Flattened Devicetree: a big-endian header, a structure block of `BEGIN_NODE`/`PROP`/`END_NODE` tokens, and a strings block).

Main contents:
- `/cpus`: `timebase-frequency = 10000000`; each `cpu@N` with `riscv,isa = "rv64imafdc_zicsr_zifencei"`, `mmu-type = "riscv,sv39"`, and an interrupt-controller child
- `/memory@80000000`: the RAM range
- `/soc`: the CLINT, PLIC, UART (`ns16550a`, interrupts = 10), test finisher (`syscon`), and virtio-mmio if a disk is attached
- `/chosen`: `bootargs = "console=ttyS0 earlycon=sbi"`, the console path, and the initrd location

`--dump-dtb out.dtb` writes it out; `dtc -I dtb -O dts out.dtb` shows it as text.

---

## 6. The boot sequence (`machine::boot()`)

1. **Read the kernel `Image`** and check its header (magic `"RISCV\0\0\0"` and `"RSC\x05"`); read `text_offset`.
2. **Map RAM** (512 MiB at `0x8000_0000`, all permissions) and copy the kernel to `RAM + text_offset` (usually `0x8020_0000`).
3. **Place the DTB** at the top of RAM (2 MiB aligned) and the **initrd** (the initial root filesystem) just below it.
4. **Create the devices** and register them at their addresses.
5. **Create the harts** (CPUs): each a `CpuState` with `softmmu = 1`. With the built-in SBI, hart 0 starts in **S-mode at the kernel entry** with `a0 = 0` (hart id) and `a1 = DTB address`, `satp = 0` (MMU off). Other harts wait, stopped, until the kernel starts them through SBI HSM.
6. Run the **machine loop**.

The guest software (decision D50) is Ubuntu 24.04's stock riscv64 Linux **6.8** kernel and Ubuntu's `busybox-static` 1.36.1, both downloaded with pinned SHA-256 checksums by `tools/fetch-guest-images.sh`. `bridgev mkinitramfs` packs BusyBox and a small `/init` script (mount `/proc`, `/sys`, `/dev`, print a banner, start a shell) into a cpio archive, without needing root.

---

## 7. The machine loop

The heart of system mode (the `loop` in `machine::boot()`):

```
loop {
    if total instructions ≥ --max-insns:  stop
    // 1. devices → interrupt-pending bits
    now = the current time
    process virtio-blk requests queued during the last slice
    PLIC: set line 10 = UART wants attention, line 1 = disk wants attention
    for each hart h:
        mip.MSIP = CLINT msip[h]
        mip.MTIP = now ≥ mtimecmp[h]
        mip.MEIP, mip.SEIP = PLIC outputs for h's two contexts
        mip.STIP = now ≥ the SBI timer deadline for h    (built-in SBI)
        if h was waiting in WFI and something is now pending: wake it
    // 2. pick a hart
    h = the next started, non-waiting hart (round-robin)
    if there is none (all waiting in WFI):
        sleep until the next timer deadline, a keypress, a disk request or power-off
        continue
    // 3. run it for one slice
    stop = harts[h].engine.run(cpu, mem, env, 100_000)
    match stop {
        Ecall → sbi::call(); pc += 4; apply the SbiAction (IPI, remote fence, hart start/stop, shutdown)
        Wfi   → mark hart h as waiting
        Limit → (slice used up) nothing to do
    }
    if the power-off device was written: stop (PowerOff / Failure / Reset)
}
```

**Interrupt latency:** device state is copied into `mip` only between slices (every 100,000 guest instructions at most, `--slice`), and the engine takes pending interrupts between blocks. That is fast enough for a timer ticking hundreds of times per second.

### 7.1 WFI: sleeping instead of spinning

When Linux has nothing to do, its idle loop executes `wfi` ("wait for interrupt"). If nothing is pending, the engine stops with `Stop::Wfi` (exit reason 11), and once every hart is waiting, the host thread **sleeps** until the next timer deadline or keyboard input. So an idle guest uses almost no host CPU, and the boot's "time to shell" isn't inflated by busy-waiting.

### 7.2 SMP: several CPUs (`--smp N`, D60)

Each hart has its own `CpuState` **and its own engine** (so its own translation cache). Harts run **one at a time, round-robin, one slice each**, in the machine thread (no host parallelism, like user-mode threads). Per-hart CLINT registers and PLIC contexts, SBI IPIs and remote fences, and the `smc_log` (so each hart's engine learns about code written by others) make real SMP semantics work: Linux brings up 4 CPUs, with the built-in SBI and with OpenSBI.

---

## 8. What a boot looks like

```
$ bridgev boot --kernel guest/build/linux/Image --initrd guest/build/linux/rootfs.cpio --stats
[    0.000000] Linux version 6.8.0-60-generic (buildd@bos03-riscv64-060) ...
[    0.000000] Machine model: bridgev,virt
[    0.000000] SBI specification v2.0 detected
...
[    0.901770] Run /init as init process

BusyBox v1.36.1 (Ubuntu 1:1.36.1-6ubuntu3.1) built-in shell (ash)
/ # uname -a
Linux (none) 6.8.0-60-generic #63.1-Ubuntu SMP PREEMPT_DYNAMIC ... riscv64 GNU/Linux
/ # poweroff -f
[    1.220557] reboot: Power down
bridgev: machine stopped: PowerOff
bridgev: 926257566 guest instructions in 1.621 s (571.4 MIPS)
```

**Measured** (final benchmark, `tools/boot-bench.py`): time to the shell prompt is **1.48 s** under the JIT, **1.54 s** under `qemu-system-riscv64`, and 10.49 s under the interpreter. About 926 million guest instructions run on the way (~592 million per second).

The whole boot plus a shell session has also been run in **lockstep** (84 million blocks compared, no difference). Devices make that tricky: a device read can have side effects, so it must not happen twice. Lockstep therefore **records** the interpreter's device accesses and **replays** them to the JIT run, comparing instead of repeating them (decision D52, file 17).

[`tests/linux_boot.rs`](../../tests/linux_boot.rs) boots Linux, types `uname -a; cat /proc/cpuinfo; ls /` into the emulated UART, and checks the output. It is `#[ignore]`d by default because it needs the downloaded images; CI runs it in a separate job.

---

## Check yourself

1. List what a Linux kernel needs from the machine, and which Bridge-V component provides each item.
2. What is MMIO? How does a guest store to the UART reach the device's Rust code?
3. How does a keypress on your keyboard end up as a character in the BusyBox shell? Name every component on the path.
4. What is the SBI? Why is it implemented in Rust by default, and what does `--firmware` change?
5. What is a devicetree for? Name three nodes Bridge-V generates.
6. Describe the first five steps of `machine::boot()`.
7. Walk through one iteration of the machine loop.
8. What happens when the guest executes `wfi` with nothing pending?
9. How are multiple harts run? Do they run in parallel?
