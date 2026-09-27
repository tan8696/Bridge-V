# Phase 09 report: Milestone B (boot Linux to a BusyBox shell)

| Field | Value |
|---|---|
| Phase | 9: Milestone B (see `docs/ROADMAP.md`) |
| Status | Complete (with one deviation: a stock Ubuntu 6.8 kernel instead of a self-built 6.6, §9) |
| Dates | 2026-09-27 |
| Branch / final commit | `claude/compassionate-babbage-ul3prl` @ the commit that adds this report (code: `ee9f7a6` first boot, `a08a562` system-mode chaining and lockstep over devices, `c24bc9c` WFI idle and the CI job) |
| Sessions used | 2 (part of each) |

## 1. Summary

Bridge-V now emulates a whole RISC-V computer. `bridgev boot --kernel Image --initrd rootfs.cpio` boots an unmodified Linux 6.8 kernel (Ubuntu 24.04's riscv64 build) in S-mode on the built-in SBI, and reaches an interactive BusyBox shell on the emulated 16550 UART.

**Results:**
- **Time to shell:** 1.25 s under the JIT, against 1.50 s for `qemu-system-riscv64` 8.2.2 (TCG) on the same kernel and initramfs. The interpreter takes 9.99 s (§7).
- **Correctness:** the whole boot plus a shell session also runs under `--engine lockstep`, which compares the JIT against the interpreter after every one of 84 million translated blocks. Device accesses are included (D52).
- **Automated test:** `tests/linux_boot.rs` checks it under jit, interp and lockstep. It sends `uname -a`, `cat /proc/cpuinfo`, `ls /` and `echo $((6 * 7))` over the UART, checks the answers, and expects a clean `poweroff`. A new CI job (`linux-boot`) runs it.

**Getting from "it boots" to faster than QEMU** took four system-mode improvements (D51):
- cross-page exits that probe the jump cache;
- jump-cache entries tagged with the TB flags;
- per-page SFENCE.VMA;
- chaining after CSR instructions.

Together they cut dispatcher entries 9×, from 6.5 M to 0.7 M per boot, and time to shell from 1.80 s to 1.30 s. WFI idle (D50) stops an idle shell from burning a host core.

## 2. Planned vs delivered

| Task ID | Task | Status | Notes |
|---|---|---|---|
| P9.1 | Devices: CLINT, PLIC, UART 16550A, syscon | done | Plus WFI idle. Unit tests per device (§6). The host tty is not switched to raw mode (§10). |
| P9.2 | Devicetree generator | done | `src/system/fdt.rs`, validated with `dtc` in a unit test. |
| P9.3 | Boot flow and built-in SBI | done | Image header parsing, DTB at the top of RAM, initrd below it, S-mode start state per §20.2. SBI: legacy 0x00–0x08, BASE, TIME, IPI, RFENCE, HSM, SRST, DBCN. |
| P9.4 | Guest image build and caching | done, differently | kernel.org and GitHub are blocked from the container, so there is no source build. `tools/fetch-guest-images.sh` takes Ubuntu's stock riscv64 kernel and `busybox-static` from the Ubuntu archive (pinned SHA-256), and `bridgev mkinitramfs` builds the initramfs (D50, §9). The images were validated under `qemu-system-riscv64 -M virt` first. |
| P9.5 | Bring-up | done | Booted under the interpreter first, then the JIT, then lockstep (§8). |
| P9.6 | Automated boot test | done | `tests/linux_boot.rs` (3 ignored tests) and the `linux-boot` CI job. |
| P9.7 | Boot performance | done | `tools/boot-bench.py`; §7. |

## 3. What was built

### 3.1 The machine (`src/system/machine.rs`)
`boot()` builds a QEMU-`virt`-compatible machine (§20.1):
- **RAM:** `--ram`, default 512 MiB, at 0x8000_0000, mapped in `DirectMem`.
- **Devices** on the `DirectMem::devices` MMIO bus: CLINT at 0x0200_0000, PLIC at 0x0C00_0000, UART at 0x1000_0000 and the test finisher at 0x0010_0000.

**Loading:**
- `image_layout` checks the Image header magics (`RISCV\0\0\0`, `RSC\x05`) and reads `text_offset` and `image_size`.
- The kernel goes to RAM + `text_offset` (0x8020_0000).
- The DTB goes at the top of RAM, 2 MiB aligned. The initrd goes just below it (`linux,initrd-start/end` in `/chosen`).

**Start state (§20.2):**
- Hart 0 in S-mode at the kernel entry, with `a0 = 0`, `a1 = dtb`, `satp = 0`.
- `medeleg` = every delegable cause except ecall-from-S.
- `mideleg` = SSI|STI|SEI.
- `mcounteren = 7`.
- `cpu.softmmu = 1`: every access is translated (D48).

**Slice loop.** The machine repeatedly runs the engine for up to `--slice` instructions (default 100 000). Before each slice it:
- publishes `mtime` (the `time` CSR clock: 10 MHz host time, or icount/10 with `--deterministic`);
- feeds the UART's interrupt line into PLIC source 10;
- rebuilds `mip`: MSIP/MTIP from the CLINT, MEIP/SEIP from the PLIC contexts, and STIP from the SBI timer (`time ≥ stimecmp`).

The engines take a pending, enabled interrupt when they next return to their dispatch loop, at the latest at the start of the next slice.

**The engine stops for three reasons:**
- **S-mode ECALL** (`Env::sbi`): the loop calls `sbi::call`, advances `pc` by 4, and powers off or resets if asked.
- **WFI** (D50): the loop sleeps until the earliest of mtimecmp and stimecmp, or until console input arrives, polling input at least every millisecond.
- **The instruction limit**, which only ends the slice.

A syscon write or an SBI SRST ends the run.

### 3.2 Devices
| Device | File | Model |
|---|---|---|
| CLINT | `src/system/clint.rs` | msip (bit 0), 64-bit mtimecmp with 32-bit halves, read-only mtime from the machine's snapshot. |
| PLIC | `src/system/plic.rs` | 53 level-triggered sources, priorities, pending, per-context enables and thresholds, claim/complete; context 0 = M, context 1 = S. |
| UART 16550A | `src/system/uart16550.rs` | Full register file (DLAB, IER, IIR, FCR, LCR, MCR, LSR, MSR, SCR). TX is instant to stdout (or a buffer in tests). An RX queue is filled by a stdin thread. The IRQ covers received data (RDI) and THR empty (THRI). |
| Test finisher | `src/system/syscon.rs` | 0x5555 poweroff, `code << 16 \| 0x3333` fail, 0x7777 reset. |

### 3.3 The built-in SBI (`src/system/sbi.rs`)
S-mode ECALLs don't trap to M-mode. The engine returns `Stop::Ecall`, and `sbi::call` serves the call in Rust:

| Extension | Implemented |
|---|---|
| Legacy 0x00–0x08 | set_timer, console putchar/getchar, clear/send IPI, remote fence.i / sfence.vma, shutdown |
| BASE | spec version 2.0, implementation id 0xb5, probe (every extension below), mvendor/march/mimp = 0 |
| TIME | set_timer: arms `stimecmp` and clears STIP |
| IPI | send_ipi to hart 0 sets SSIP |
| RFENCE | remote_fence_i calls `Engine::fence_i` (resets the jump cache, D49); remote_sfence_vma* flushes the TLB |
| HSM | hart_get_status for hart 0; start/stop of the only hart fail as the spec requires |
| SRST | shutdown (reason 1 = failure) and reset |
| DBCN | console write, read, write_byte |

Linux 6.8 detects all of these at boot (log, §5). It uses SRST for `poweroff`: the log shows `pm_power_off already claimed for sbi_srst_power_off`.

### 3.4 Devicetree (`src/system/fdt.rs`)
A small FDT writer (header, reservation block, structure block, strings with name reuse) and `virt_dtb()`, which follows §20.3:
- one `rv64imafdc_zicntr_zicsr_zifencei` hart with `riscv,isa-base`/`riscv,isa-extensions` (for 6.7+ kernels), `mmu-type = "riscv,sv39"` and a `riscv,cpu-intc`;
- memory, CLINT, PLIC and `ns16550a`;
- the test finisher, with `syscon-poweroff`/`syscon-reboot` at the root, as in QEMU;
- `/chosen` with bootargs, `stdout-path` and the initrd range.

`--dump-dtb` writes the blob out, and `--dtb` replaces it.

### 3.5 Guest images (`tools/fetch-guest-images.sh`, `bridgev mkinitramfs`)
- **Kernel:** `linux-image-6.8.0-60-generic` (riscv64) from Ubuntu 24.04. Its `vmlinuz` is an EFI-stub `Image` with the drivers the machine needs built in.
- **Userland:** `busybox-static` 1.36.1.
- **Fetching:** both `.deb`s are downloaded from `ports.ubuntu.com` and checked against pinned SHA-256 sums.
- **Initramfs:** `bridgev mkinitramfs` writes a newc cpio (`machine::initramfs`) with directories, `/dev/console` and `/dev/null` nodes (no root needed to create them), BusyBox and this `/init`:
  ```sh
  /bin/busybox --install -s /bin
  mount -t proc proc /proc; mount -t sysfs sysfs /sys; mount -t devtmpfs devtmpfs /dev
  echo "Bridge-V: BusyBox $(busybox | head -1 | cut -d' ' -f2) on Linux $(uname -r)"
  exec setsid cttyhack sh      # (or plain sh)
  ```

Nothing binary is committed. CI caches the `.deb`s keyed by the script's hash.

### 3.6 System-mode chaining and the jump cache (D51)
The first boot (`ee9f7a6`) reached the shell in 1.80 s, slower than QEMU (1.48 s). `--stats` showed 6.5 M dispatcher entries (6,965 per million guest instructions), 6.1 M of them plain direct exits. That was expected: D48 only chains within a virtual page, because a mapping can change without the TB being invalidated. Four changes fixed it.

1. **Cross-page exits probe the jump cache inline.** A direct exit whose target is on another page now emits the JALR lookup sequence in its stub (`direct_stub` → `jump_cache`), instead of returning to the dispatcher with slot 0/1. The jump cache is keyed by virtual pc and is valid for the current translation regime, which is exactly what a cross-page transfer needs.
2. **Flag-tagged entries.** Entries hold `pc ^ flags << 56` (`jc_tagged`), and a lookup XORs its TB's flags in before the compare (`movabs r11, tag; xor r11, rax; cmp r11, [rbp+r10+jc]`). A kernel↔user switch or a SUM toggle therefore no longer resets the table. Bits 12–55 of the stored value remain the page number, so a page's entries can still be found.
3. **Per-page SFENCE.VMA.** Linux issues about 8,250 `sfence.vma <addr>` per boot and about 50 global ones. Each used to flush all four TLBs and reset the jump cache. `tlb::flush_page` now drops only the page's entries in each MMU index, and its jump-cache entries (a page's pcs occupy one contiguous half of the 4096-entry table). It falls back to a full flush when the address lies in the range of superpages cached since the last full flush (`cpu.tlb_super`), because a superpage fills many 4 KiB slots. `cpu.jc_gen` moves only on full flushes. `cpu.mmu_gen` still moves every time, for the interpreter's decoded blocks.
4. **Chaining after CSR instructions.** TBs ending in a CSR instruction used to be unchainable, because a CSR write may change the flags the successor was looked up with. Linux's `local_irq_save/restore` (`csrrc/csrs sstatus`) made those a large share of exits. Now `helper_interp_one` compares (TB flags, `jc_gen`) before and after the instruction. It leaves the TB only if that changed, if an interrupt became deliverable (so enabling interrupts is still noticed at once), or if the instruction was SFENCE.VMA. Otherwise the linked exit is valid.

| `--stats`, one jit boot to the shell and poweroff | `ee9f7a6` | after D51 (`a08a562`) |
|---|---:|---:|
| dispatcher entries | 6,496,688 | 693,696 |
| plain (unlinked) direct exits | 6,062,423 | 301,866 |
| jump-cache misses | 416,183 | 371,266 |
| TLB fills | 1,438,070 | 583,131 |
| time to shell (median of 5, `tools/boot-bench.py`) | 1.80 s (3 runs) | 1.30 s |

The remaining 0.3 M plain exits are almost all into page-straddling 32-bit instructions: about 230 K per boot, interpreted by design (D48) and never linked (§10).

### 3.7 Lockstep over devices (D52)
Lockstep runs each TB twice: the interpreter first, whose RAM writes are logged and undone, then the JIT. Device accesses cannot be undone. The first lockstep boot diverged after 77.5 M identical TBs in `plic_toggle`: the PLIC enable word was read-modify-written twice, so the JIT read the interpreter's update (`x12: interp 0x0, jit 0x400`, bit 10 = the UART's source).

`DirectMem::mmio_log` now handles devices in two modes:
- **Reference run:** accesses are performed and recorded.
- **JIT run:** they are replayed. Reads return the recorded values, and writes are compared, not performed.

A different address, size, kind, written value or count is reported as a divergence.

### 3.8 WFI idle (D50)
Linux's idle loop executes `wfi` with interrupts globally disabled. It relies on WFI waking on any interrupt pending in `mip & mie`. WFI used to be a no-op, so an idle shell spun the idle loop at full speed.

With `Csrs::wfi_idle` (set by the machine, ineffective under deterministic time), a WFI with nothing pending in `mip & mie`:
1. retires;
2. stops the engine (`Flow/BlockExit/Stop::Wfi`; in JIT code, `helper_interp_one` exits with the new reason 11 `WFI`);
3. lets the machine sleep until the next timer deadline or console input.

Lockstep forces deterministic time, so WFI stays a no-op there. Otherwise time would never advance while the hart sleeps.

| 5 s at the shell prompt, then `poweroff -f` | host CPU (user) | guest instructions |
|---|---:|---:|
| before (`a08a562`) | 6.27 s | 6,537,844,197 |
| after (`c24bc9c`) | 1.43 s | 935,417,947 (5.01 s idle in 21 WFIs) |

## 4. Design decisions made

- **D50:** the guest software (Ubuntu 6.8 + busybox-static instead of a source-built 6.6; supersedes D17) and the machine design: built-in SBI, device set, slice loop, WFI idle.
- **D51:** system-mode chaining and the jump cache (§3.6). It amends D48, which reset the jump cache on every flags change and never chained out of CSR-ending TBs.
- **D52:** lockstep over devices (§3.7).

**Choices and the alternatives considered:**
- **SBI in Rust vs OpenSBI.** In Rust, as D15 planned: the kernel only needs the SBI calls, and OpenSBI would add a whole M-mode firmware to bring up. OpenSBI stays a Phase 10 item.
- **Instant UART TX.** A byte written to THR is on stdout at once and THRE is always set. Modelling baud-rate timing would only slow the console.
- **Device state is polled between slices** rather than delivered asynchronously. The budget (D12) bounds interrupt latency to one slice, 100 k instructions, about 0.15 ms of JIT time.
- **Tag the jump cache instead of keeping one table per flags value.** One XOR with a translate-time constant, against the cost of switching 64 KiB tables at every privilege change.

## 5. How it works: worked example (the idle loop and a timer tick)

With the shell waiting for input, the kernel is in `do_idle` → `arch_cpu_idle` → `wfi`, with `sstatus.SIE = 0` and `sie.STIE = 1`. The SBI timer was armed earlier through TIME `set_timer`: a7 = 0x54494D45 and a0 = the deadline, stored in `SbiState::stimecmp`.

1. The TB containing `wfi` calls `helper_interp_one` (WFI is not lowered natively). `step` sees `mip & mie == 0`, so it returns `Flow::Wfi`. The helper retires the instruction, sets `pc = wfi + 4` and exit reason 11, and the TB leaves through its helper exit.
2. `Jit::exec` maps reason 11 to `BlockExit::Wfi`, and `deliver` turns that into `Stop::Wfi`. The engine returns to the machine loop.
3. The machine takes `deadline = min(clint.mtimecmp, sbi.stimecmp)` and sleeps in steps of at most 1 ms until `cpu.time() ≥ deadline` or the UART RX queue is non-empty.
4. The loop top rebuilds `mip`: `time ≥ stimecmp` sets STIP.
5. The next `engine.run` resumes after the `wfi`. STIP is pending and `sie.STIE` is set, but `sstatus.SIE` is still 0, so the interrupt waits. The kernel's idle path then re-enables interrupts (`csrs sstatus, SIE`) through `helper_interp_one`. The helper sees that an interrupt became deliverable and leaves the TB (D51). The engine's `deliver_interrupt` takes the supervisor timer interrupt: `scause = 1 << 63 | 5`, `sepc` = the instruction after the `csrs`, pc = `stvec`.
6. The kernel's timer handler reprograms the next tick with another SBI TIME call. The engine stops with `Stop::Ecall`, the machine serves it, and the hart returns to the idle loop and to step 1.

A keypress takes the same path, with UART RX → IRQ 10 → PLIC context 1 → SEIP in step 4.

The trimmed boot log (full log: `docs/bench/2026-09-27-c24bc9c-boot/boot-log.txt`; `--stats`: `stats.txt` next to it):
```
[    0.000000] Linux version 6.8.0-60-generic (buildd@bos03-riscv64-060) ... #63.1-Ubuntu SMP PREEMPT_DYNAMIC ...
[    0.000000] Machine model: bridgev,virt
[    0.000000] SBI specification v2.0 detected
[    0.000000] SBI implementation ID=0xb5 Version=0x1
[    0.000000] SBI TIME extension detected
[    0.000000] SBI IPI extension detected
[    0.000000] SBI RFENCE extension detected
[    0.000000] SBI SRST extension detected
[    0.000000] SBI DBCN extension detected
[    0.000000] earlycon: sbi0 at I/O port 0x0 (options '')
[    0.000000] SBI HSM extension detected
[    0.000000] riscv: base ISA extensions acdfim
[    0.000000] Kernel command line: console=ttyS0 earlycon=sbi
[    0.000000] Memory: 423272K/524288K available (12952K kernel code, 6991K rwdata, 10240K rodata, 6136K init, 799K bss, 68248K reserved, 32768K cma-reserved)
[    0.000000] riscv-intc: 64 local interrupts mapped
[    0.000000] plic: plic@c000000: mapped 53 interrupts with 1 handlers for 2 contexts.
[    0.000123] sched_clock: 64 bits at 10MHz, resolution 100ns, wraps every 4398046511100ns
[    0.014735] Calibrating delay loop (skipped), value calculated using timer frequency.. 20.00 BogoMIPS (lpj=40000)
[    0.310499] clocksource: Switched to clocksource riscv_clocksource
[    0.435856] Trying to unpack rootfs image as initramfs...
[    0.473089] Freeing initrd memory: 1680K
[    0.608496] 10000000.serial: ttyS0 at MMIO 0x10000000 (irq = 12, base_baud = 230400) is a 16550A
[    0.610720] printk: legacy console [ttyS0] enabled
[    0.662836] syscon-poweroff poweroff: pm_power_off already claimed for sbi_srst_power_off
[    0.825030] Run /init as init process

Bridge-V: BusyBox v1.36.1 on Linux 6.8.0-60-generic

BusyBox v1.36.1 (Ubuntu 1:1.36.1-6ubuntu3.1) built-in shell (ash)
/ # uname -a
Linux (none) 6.8.0-60-generic #63.1-Ubuntu SMP PREEMPT_DYNAMIC Thu May  1 06:01:27 UTC 2025 riscv64 GNU/Linux
/ # cat /proc/cpuinfo
processor	: 0
hart		: 0
isa		: rv64imafdc_zicntr_zicsr_zifencei
mmu		: sv39
/ # ls /
bin   dev   etc   init  proc  root  sbin  sys   tmp   usr
/ # poweroff -f
[    1.147652] reboot: Power down
```
(`irq = 12` is Linux's virtual IRQ number for PLIC source 10.)

## 6. Tests and verification

| Test | What | Result |
|---|---|---|
| `tests/linux_boot.rs` (3 tests, `--ignored`) | Boot to `/ # `, then `uname -a` (expects `Linux`, `riscv64`), `cat /proc/cpuinfo` (`processor`, `rv64imafdc`, `sv39`), `ls /` (`bin`, `proc`, `sys`), `echo $((6 * 7))` (`42`), `poweroff -f` (exit 0, `PowerOff`); under jit, interp and lockstep | pass: jit shell after 1.3 s, interp 10.4 s, lockstep 56.3 s (84,207,381 TBs identical, 908 M instructions) |
| `system::plic::tests::claim_complete_priority_threshold` | Priorities, enables, thresholds, claim/complete per context | pass |
| `system::uart16550::tests::tx_rx_and_interrupts` | Divisor latch, TX, RX queue, LSR, IIR/IER interrupt causes | pass |
| `system::clint::tests::registers_and_partial_writes` (new) | 32-bit halves of mtimecmp, read-only mtime, msip bit 0 | pass |
| `system::syscon::tests::finisher_codes` (new) | Pass / fail code / reset, other values and offsets ignored | pass |
| `system::fdt::tests::header_and_blocks_are_consistent` | Header offsets and sizes, block order, strings | pass |
| `system::fdt::tests::dtc_accepts_the_blob` (new) | `dtc -I dtb -O dts` decompiles with no warnings; memory, ISA string, MMU type, CLINT, PLIC, UART, test finisher, initrd range and stdout-path present | pass |
| `mem::mmu::tests::single_page_flush` (new) | SFENCE.VMA with an address drops only that page (TLB and jump cache, whatever the flag tag); superpage range forces a full flush and resets it | pass |
| `cli::boot_rejects_a_missing_or_bad_kernel` | Missing file, and an ELF that is not an Image | pass |
| Whole suite | `cargo test --release`: 244 riscv-tests × 8 configurations, both fuzzers, user programs vs QEMU (both memory modes), … | all pass (`c24bc9c`) |
| Lint | `cargo fmt --check`, `cargo clippy --all-targets [--features disasm] -- -D warnings` | clean |

**Acceptance criteria:**
- ✅ Linux (6.8, see §9) reaches `/ #` under jit (and interp, and lockstep).
- ✅ The automated UART test passes, and CI runs it (`linux-boot` job).
- ✅ The boot metrics are recorded (§7).

## 7. Performance

Host: Intel Xeon @ 2.10 GHz (shared cloud VM, noisy). `tools/boot-bench.py`: `taskset -c 2`, 1 warm-up run, then 5 measured runs; median (min–max). Time to shell = from process start until `/ # ` appears on the console. Instructions and MIPS cover the whole run up to `poweroff -f`. QEMU 8.2.2 TCG uses its own OpenSBI and devicetree (`qemu`), or bridgev's devicetree (`qemu-bvdtb`, the same device set).

| config (`c24bc9c`) | time to shell (s) | guest instructions | MIPS |
|---|---:|---:|---:|
| **bridgev jit** | **1.25 (1.21–1.42)** | 930,751,294 | 701 |
| bridgev interp | 9.99 (9.82–10.21) | 895,355,098 | 89 |
| qemu-system-riscv64 | 1.50 (1.36–1.56) | — | — |
| qemu-system-riscv64, bridgev's DTB | 1.62 (1.25–1.72) | — | — |

Raw data: `docs/bench/2026-09-27-c24bc9c-boot/` (also `2026-09-27-ee9f7a6-boot/` and `2026-09-27-a08a562-boot/` for the before/after in §3.6). The JIT is 8.0× the interpreter here, far below user mode's 26.5×, because about 36% of JIT boot time is translation (next paragraph).

**Where the JIT's time goes** (one run's `--stats`, `stats.txt`):
- **Translation:** 552 ms of 1.52 s. There are 66,669 TBs for 417 K guest instructions, and 45 MB of host code (111 bytes per guest instruction; inline TLB probes make softmmu code large). A boot runs a lot of code once, including every BusyBox applet process. The TBs are small (6.3 instructions on average) and are translated with cold caches. The IR passes and lowering take about 7 µs per TB (measured with temporary timers: build 33 ms, lift 19 ms, optimize 164 ms, lower 290 ms, place 41 ms).
- **SMC:** 117 code-page writes invalidated 10,600 TBs (freed pages reused for code).
- **Dispatcher:** 703 K entries.
- **TLB:** 674 K fills for 929 M instructions, i.e. 0.73 walks per 1,000 instructions. Hits are not counted (no counter on the hot path), so this fill rate stands in for the miss rate.

## 8. Bugs found and fixed

| Symptom | Root cause | Fix (commit) | Regression test |
|---|---|---|---|
| `dtc` warnings on the first DTB (`poweroff`/`reboot` under `/soc` without `reg`) | Wrong node placement | Moved to the root, as in QEMU (`ee9f7a6`) | `fdt::tests::dtc_accepts_the_blob` |
| Lockstep boot diverged after 77.5 M TBs: `x12: interp 0x0, jit 0x400` in `plic_toggle` | Lockstep executed device accesses twice (PLIC enable read-modify-write) | MMIO record/replay (D52, `a08a562`) | `linux_boot::linux_boots_to_busybox_shell_lockstep` |
| Boot slower than QEMU (1.80 s vs 1.48 s), 6.5 M dispatcher entries | No cross-page chaining, jump-cache resets on every flags change, full flushes on per-page SFENCE.VMA, CSR-ending TBs unchainable | D51 (`a08a562`) | lockstep boot (checks every chained and jump-cache transfer); `mmu::tests::single_page_flush` |
| An idle shell used 100% of a host core (6.5 G guest instructions in 5 s) | WFI was a no-op | WFI idle (D50, `c24bc9c`) | boot tests (idle phases while waiting for commands) |

Otherwise, the Phase 7 privileged/Sv39 work and the Phase 8 SMC work carried the kernel through on the first attempt. The first interpreter boot went straight through SBI detection, earlycon and memory setup, and the JIT boot reached the shell. The typical culprits the roadmap anticipated (FS state, sstatus views, timer interrupts, SUM/MXR, A/D bits, `sfence.vma`) had already been exercised by the 244 riscv-tests and the softmmu fuzzer.

## 9. Deviations from the plan / spec

- **Kernel and BusyBox (D50 supersedes D17).** The plan was a Linux 6.6 LTS kernel and BusyBox built from source with `defconfig` + `bridgev.config`. kernel.org and GitHub are blocked by the container's network policy (`tools/fetch-guest-images.sh` notes this), and the Ubuntu archive is not. The Ubuntu kernel is an unmodified distro build of Linux 6.8, which serves goal §1 ("boot an unmodified RISC-V Linux kernel v6.x") at least as well. There is no `guest/linux/build.sh` or config fragment. With network access, a self-built 6.6 needs only the same `Image` + initramfs interface.
- **Initramfs outside the kernel.** It is passed with `--initrd` (in `/chosen`), not embedded. `bridgev mkinitramfs` builds it without root.
- **`--firmware`** (OpenSBI) is not implemented. The built-in SBI is the boot path (D15), and OpenSBI stays in Phase 10.
- **§13.3/§13.4/§14.4/§15** were updated as built (D51, WFI). §23 now lists the real `boot` options and `mkinitramfs`.

## 10. Known limitations and technical debt

- **Page-straddling 32-bit instructions** are interpreted every time: about 230 K per boot, each costing two dispatcher round trips. §13.2's one-instruction TB that records both pages would remove them.
- **Translation cost** (about 36% of JIT boot time): a cheaper tier for cold code, or caching translations across boots, would help. Neither is planned yet.
- **TLB statistics:** no hit counter (by design), so no true miss rate.
- **Console:** the host terminal is not put in raw mode. Line editing is the host's, and Ctrl-C reaches bridgev. Scripted use (tests, CI) is unaffected.
- **Devices:** UART TX has no timing, and there is no virtio-blk (Phase 10). SBI HSM supports one hart, and IPIs only target hart 0.
- **SFENCE.VMA ignores ASIDs** (always flushes the page for every address space). This is correct, only conservative.

## 11. How to reproduce

```sh
tools/setup.sh                                   # apt packages (qemu, dtc, cross gcc, ...)
cargo build --release
tools/fetch-guest-images.sh                      # guest/build/linux/{Image,busybox,rootfs.cpio}
target/release/bridgev boot --kernel guest/build/linux/Image \
    --initrd guest/build/linux/rootfs.cpio --stats   # interactive shell; `poweroff -f` to leave
cargo test --release --test linux_boot -- --ignored --test-threads 1   # jit, interp, lockstep
python3 tools/boot-bench.py                      # §7 table (jit, interp, qemu, qemu-bvdtb)
cargo test --release                             # everything else
```

## 12. Next steps

Phase 10 (stretch goals, one mini-report each), in the roadmap's order. The first is **OpenSBI boot**: `--firmware fw_jump.bin`, starting in M-mode with the full M-mode path (MPRV, misaligned-access emulation). It validates the privileged implementation against real firmware instead of the built-in SBI. Ubuntu's `opensbi` package supplies `fw_jump.bin` the same way the kernel was fetched.
