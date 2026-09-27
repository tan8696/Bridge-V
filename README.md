# Bridge-V

**A dynamic binary translator that runs RISC-V (RV64GC) Linux programs, and whole RISC-V Linux systems, on x86-64 by JIT-compiling guest basic blocks into native x86-64 machine code.**

It is written from scratch in Rust. There is no JIT library and no external assembler. It has four core subsystems:
- **A hand-written x86-64 emitter**, with W^X dual-mapped code memory.
- **Direct block chaining**, by hot-patching aligned rel32 jumps in the code cache, plus an inline jump cache for returns.
- **Guest registers pinned in R12–R15**, plus a per-block linear-scan allocator with lazy write-back and precise faults.
- **An Sv39/Sv48 software MMU** with an inline, direct-mapped software TLB.

## Results

Final measurements from the benchmark harness (`tools/bench.py`, `tools/boot-bench.py`; commit `567255a`). The host is an Intel Xeon @ 2.10 GHz shared cloud VM, pinned to one CPU. Each figure is the median of 5 runs. Method and raw data: [`docs/BENCHMARKS.md`](docs/BENCHMARKS.md).

| workload | Bridge-V JIT | vs `qemu-riscv64` 8.2 | vs native x86-64 |
|---|---:|---:|---:|
| CoreMark | 13,409 it/s (4.8 billion guest instructions/s) | **1.49×** | 52.5% |
| Dhrystone | 11,798 DMIPS | **4.36×** | 45.5% |
| FP benchmark (nbody, matrix multiply, conversions) | 3,711 units/s | **5.83×** | 22.4% |
| CoreMark with every access through the software TLB (`--mem=softmmu`) | 7,526 it/s | 0.84× | 29.5% |
| Linux 6.8 boot to a BusyBox shell (system mode) | **1.48 s** | `qemu-system-riscv64`: 1.54 s | — |

**Correctness checks:**
- All 244 official riscv-tests pass under every engine.
- User programs match `qemu-riscv64` byte for byte.
- The JIT is checked in lockstep against the reference interpreter, including a complete Linux boot (84 M blocks, no divergence).
- Random fuzzers compare 10⁶ integer blocks against the interpreter and check 10⁶ FP cases bit-exact against Berkeley SoftFloat.

## Quick start

```sh
tools/setup.sh                   # apt: riscv64 cross gcc, qemu-user/system, dtc (Ubuntu 24.04)
cargo build --release
tools/build-guests.sh            # test programs → guest/build/*.elf

# User mode: run a RISC-V Linux program
target/release/bridgev run --engine jit --stats guest/build/hello-O2.elf

# Milestone A: CoreMark under the interpreter and the JIT, with the speedup (about 2 minutes)
tools/demo-milestone-a.sh

# Milestone B: boot Linux (the pinned Ubuntu kernel and busybox-static; no root needed)
tools/fetch-guest-images.sh
target/release/bridgev boot --kernel guest/build/linux/Image --initrd guest/build/linux/rootfs.cpio
```
Other options: `--engine interp|jit|lockstep`, `--mem softmmu`, `--firmware fw_dynamic.bin` (OpenSBI), `--smp 4`, `--mmu sv48`, `--disk disk.img`, `--gdb PORT`. The full CLI reference is in [`CLAUDE.md` §23](CLAUDE.md).

## Demo

All three transcripts below are real output from runs on the machine above.

**CoreMark under the JIT** (`bridgev run --engine jit --stats guest/build/bench/coremark-rv64.elf 0x0 0x0 0x66 150000`, trimmed):
```
Total time (secs): 11.488000
Iterations/Sec   : 13057.103064
Compiler flags   : -O2 -march=rv64gc -mabi=lp64d -static -DPERFORMANCE_RUN=1  -lrt
Correct operation validated. See README.md for run and reporting rules.
bridgev: 53821813220 guest instructions in 11.500 s (4680.2 MIPS)
bridgev: jit: 1691 TBs translated (9503 guest insns, 231 KiB host code, 24.9 bytes/insn), ...
         translate time 10.5 ms; 540245 dispatcher entries (10 per M guest insns); ... chain: on, 1733 links
```

**Milestone A** (`tools/demo-milestone-a.sh`, one run per configuration):
```
CoreMark (validated): interpreter 452.4 it/s, JIT 13,332.7 it/s -> JIT speedup 29.5x over the interpreter
```

**Milestone B: Linux boots to a shell** (`bridgev boot --kernel Image --initrd rootfs.cpio --stats`, with commands typed at the prompt; trimmed):
```
[    0.000000] Linux version 6.8.0-60-generic (buildd@bos03-riscv64-060) ...
[    0.000000] Machine model: bridgev,virt
[    0.000000] SBI specification v2.0 detected
[    0.901770] Run /init as init process

BusyBox v1.36.1 (Ubuntu 1:1.36.1-6ubuntu3.1) built-in shell (ash)
/ # uname -a
Linux (none) 6.8.0-60-generic #63.1-Ubuntu SMP PREEMPT_DYNAMIC Thu May  1 06:01:27 UTC 2025 riscv64 GNU/Linux
/ # cat /proc/cpuinfo
processor	: 0
isa		: rv64imafdc_zicntr_zicsr_zifencei
mmu		: sv39
/ # poweroff -f
[    1.220557] reboot: Power down
bridgev: machine stopped: PowerOff
bridgev: 926257566 guest instructions in 1.621 s (571.4 MIPS)
softmmu: 642031 TLB fills
```

To see what the JIT actually emits, [`docs/WHITEBOARD.md`](docs/WHITEBOARD.md) walks through real translated blocks byte by byte: a chained loop, a return through the jump cache, and the inline TLB probe.

## What it supports

- **User mode**, like `qemu-riscv64`:
  - static and dynamic (PIE, `ld.so`) RV64GC Linux ELF programs;
  - 62 syscalls with struct translation;
  - pthreads (serialized);
  - guest signal handlers;
  - self-modifying code with eager invalidation;
  - a GDB remote stub.
- **System mode**, like `qemu-system-riscv64 -M virt`:
  - M/S/U privilege and Sv39/Sv48;
  - CLINT, PLIC, NS16550A UART and a test finisher;
  - a built-in SBI, or real OpenSBI firmware;
  - a generated devicetree;
  - virtio-blk;
  - SMP guests (up to 8 harts, serialized);
  - WFI idling.
- **Engines:**
  - the reference interpreter;
  - the JIT, at levels from naive to full (`--regalloc none|pinned|linear`, `--no-chain`);
  - lockstep, which runs both and compares them after every block.

## Documents

| Document | What it covers |
|---|---|
| [`docs/PROJECT_EXPLAINED.md`](docs/PROJECT_EXPLAINED.md) | What Bridge-V is, how it works, what it's used for. Start here. |
| [`docs/WHITEBOARD.md`](docs/WHITEBOARD.md) | Real translated blocks, byte by byte (interview walkthrough). |
| [`docs/BENCHMARKS.md`](docs/BENCHMARKS.md) | Measured results from the benchmark harness, with method and raw data. |
| [`docs/phase-reports/`](docs/phase-reports/) | A detailed report for every phase (0–11). |
| [`docs/ROADMAP.md`](docs/ROADMAP.md) | The phase-by-phase build plan with acceptance criteria. |
| [`CLAUDE.md`](CLAUDE.md) | The full engineering specification and design decision log (D1–D60). |

**Status:** all phases (0–11) are complete: Milestone A (CoreMark/Dhrystone speedup report), Milestone B (Linux boot) and 8 of the 10 Phase 10 stretch goals. Not built: parallel execution of guest threads and harts (they run one at a time), a return-address stack and superblocks. See [`docs/phase-reports/phase-10-not-pursued.md`](docs/phase-reports/phase-10-not-pursued.md).
