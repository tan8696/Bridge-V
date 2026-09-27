# 04 · Code map: every folder and file, and what order to read them in

## What you will learn

- how the repository is organised
- what every source file does, in one or two lines
- a suggested order for reading the code
- which files are "hot" (performance-critical) and which contain `unsafe` code

The Rust source is about **19,000 lines** in `src/`, plus about **5,000 lines** of tests. You don't need to read it all. The files marked ⭐ are the core; read those first.

---

## 1. Top level

```
Bridge-V/
├── CLAUDE.md          the engineering spec + decision log (D1–D60). Dense but complete.
├── README.md          landing page with results and quick start
├── Cargo.toml         Rust package: one library ("bridgev") + one binary ("bridgev")
├── build.rs           compiles the vendored Berkeley SoftFloat C library (for exact FP)
├── rust-toolchain.toml  pins Rust 1.94
├── src/               the program (below)
├── tests/             integration tests (below)
├── guest/             RISC-V test programs and benchmark sources (C and assembly)
├── third_party/       git submodules: riscv-tests, CoreMark, berkeley-softfloat-3
├── tools/             shell/Python scripts: setup, building guests, benchmarks
└── docs/              documentation (this folder is docs/study/)
```

Dependencies (from `Cargo.toml`) are deliberately few: `libc` (system calls), `clap` (command-line parsing), `anyhow` (errors), `rustc-hash` (fast hash maps), `cc` (build the C library). Test-only: `iced-x86` (x86 disassembler, used to check the encoder) and `proptest` (random testing).

---

## 2. `src/`, folder by folder

### 2.1 Entry point

| File | Lines | What it does |
|---|---:|---|
| ⭐ [`main.rs`](../../src/main.rs) | 568 | The command line (`clap`). Subcommands `run`, `boot`, `mkinitramfs`, `disasm`. Turns flags into `JitOptions`, `RunOptions`, `BootOptions`, then calls `user::run`, `bare::run` or `machine::boot`. |
| [`lib.rs`](../../src/lib.rs) | 16 | Lists the modules. |
| [`elf.rs`](../../src/elf.rs) | 309 | Hand-written ELF64 parser: header checks, program headers (`loads()`), entry point, symbol lookup. Never panics on bad input. |
| [`stats.rs`](../../src/stats.rs) | 112 | `--stats=regs`: counts how often each guest register is used. |

### 2.2 `isa/`: the RISC-V instruction set (file 02)

| File | What it does |
|---|---|
| ⭐ [`inst.rs`](../../src/isa/inst.rs) | `enum Inst` (every decoded instruction), `Decoded { inst, len, raw }`, `ends_block()`. |
| ⭐ [`decode.rs`](../../src/isa/decode.rs) | 32-bit decoder, immediate extraction, instruction length, `decode_parts`. |
| [`rvc.rs`](../../src/isa/rvc.rs) | Compressed (16-bit) → 32-bit expansion. |
| [`disasm.rs`](../../src/isa/disasm.rs) | `Inst` → assembly text, matching LLVM's output exactly. |

### 2.3 `cpu/`: the guest CPU's state

| File | What it does |
|---|---|
| ⭐ [`state.rs`](../../src/cpu/state.rs) | `#[repr(C)] struct CpuState`: registers, pc, budget, exit reason, jump cache, spill slots, TLB, CSRs. Compile-time asserts pin every field offset the generated code uses. Also the `exit::*` reason codes. |
| [`csr.rs`](../../src/cpu/csr.rs) | The CSR file (`Csrs`): read/write rules, counters, the `time` clock, `satp` mode checks. |
| ⭐ [`trap.rs`](../../src/cpu/trap.rs) | Exception causes, `take_trap()` (trap entry + delegation), `mret()`, `sret()`, `pending_interrupt()` (priority rules). |
| [`fp.rs`](../../src/cpu/fp.rs) | Floating point for the interpreter, using Berkeley SoftFloat: NaN-boxing, rounding modes, `fflags`, FMIN/FMAX/FCLASS. |
| `softfloat_shim.c` | 12 lines of C to reach SoftFloat's global state. |

### 2.4 `interp/`: the reference interpreter (file 06)

| File | What it does |
|---|---|
| ⭐ [`mod.rs`](../../src/interp/mod.rs) | The `Engine` trait and `Stop` enum (used by every engine). `build_block` (decode a block), `exec_block` (run it), `step` (execute one instruction: the reference semantics), `alu`/`aluw`, atomics, `deliver` (turn a block exit into a trap or a stop). |

### 2.5 `ir/`: the intermediate representation (file 07)

| File | What it does |
|---|---|
| ⭐ [`ops.rs`](../../src/ir/ops.rs) | `Op` (the IR operations), `V` (a value), `Block`, `BinOp`, `Cond`; a printer for `--dump-ir`. |
| ⭐ [`lift.rs`](../../src/ir/lift.rs) | `Inst` → IR ("lifting"). Unsupported instructions become `Interp` (call the interpreter). |
| ⭐ [`opt.rs`](../../src/ir/opt.rs) | The four optimizer passes: `forward`, `dead_writes`, `fold`, `dce`. |
| [`liveness.rs`](../../src/ir/liveness.rs) | Where each value is defined and used; fixed-register constraints (`RaxRdx`, `Rcx`, `Call`). |
| [`eval.rs`](../../src/ir/eval.rs) | An IR interpreter. It is the test oracle: tests check that IR (before and after optimizing) behaves exactly like the real interpreter. |

### 2.6 `regalloc/`: register allocation (file 08)

| File | What it does |
|---|---|
| ⭐ [`linear_scan.rs`](../../src/regalloc/linear_scan.rs) | `Alloc`: picks x86 registers for IR values during code generation. Pinned registers, lazy write-back, eviction by furthest next use, spill slots, `dirty_state()` (the state map for precise faults), `sync_for_call()`. |

### 2.7 `backend/x86/`: generating x86 code (files 03, 07)

| File | What it does |
|---|---|
| ⭐ [`emit.rs`](../../src/backend/x86/emit.rs) | The hand-written x86-64 encoder (`Asm`): REX/ModRM/SIB, every instruction form, labels and fixups, NOP padding and alignment. |
| [`regs.rs`](../../src/backend/x86/regs.rs) | `enum Reg`, the pool, scratch, pinned and budget registers. |
| ⭐ [`lower_ir.rs`](../../src/backend/x86/lower_ir.rs) | The main back end: IR → x86 using the allocator. Budget prologue, ALU ops, division guards, loads/stores (direct or TLB probe), helper calls, exits and stubs, jump cache, inline FP, fault-site records. |
| [`lower.rs`](../../src/backend/x86/lower.rs) | The older, simpler back end (`--regalloc=none`): each RISC-V instruction becomes x86 directly, all guest registers kept in memory. Kept for comparison. |
| [`features.rs`](../../src/backend/x86/features.rs) | Detects BMI2 / FMA / POPCNT with `cpuid`. |
| [`disasm.rs`](../../src/backend/x86/disasm.rs) | Prints generated x86 code (text with `--features disasm`, else hex). |

### 2.8 `jit/`: the translator runtime (files 09, 10)

| File | What it does |
|---|---|
| ⭐ [`dispatch.rs`](../../src/jit/dispatch.rs) | The heart of the JIT. `Jit` struct, the dispatcher loop (`Engine::run`), `select` (find/translate the next TB), `tb_for_key` (translation), `exec` (enter code and interpret the exit), `link_last` (chaining), fault resolution (`resolve_host_fault`, `apply_site`), SMC draining, statistics. |
| ⭐ [`code_mem.rs`](../../src/jit/code_mem.rs) | The executable code buffer: W^X dual mapping, bump allocation, `patch_rel32`. |
| ⭐ [`trampoline.rs`](../../src/jit/trampoline.rs) | Generates `enter_jit`, `exit_jit`, `fault_exit` and the helper table; the Rust helpers called from generated code (`helper_interp_one`, `helper_mmu_access`). |
| [`cache.rs`](../../src/jit/cache.rs) | `TranslationBlock` metadata, `TbKey`, `TbCache` (pc → TB map, host address → TB lookup). |
| [`chain.rs`](../../src/jit/chain.rs) | `link()` and `unlink_incoming()`: patch exit jumps (30 lines of logic). |
| [`lockstep.rs`](../../src/jit/lockstep.rs) | `--engine lockstep`: run each TB in both engines and compare (file 17). |
| [`perfmap.rs`](../../src/jit/perfmap.rs) | `--perf-map`: tell the `perf` profiler which code is which TB. |
| [`mod.rs`](../../src/jit/mod.rs) | `EngineKind`, `make_engine()`. |

### 2.9 `mem/`: guest memory (file 11)

| File | What it does |
|---|---|
| ⭐ [`direct.rs`](../../src/mem/direct.rs) | `DirectMem`: the 256 GiB reserved guest address space, `map`/`unmap`/`protect`, checked `load`/`store`/`fetch16`, per-page permissions, code-page marks for SMC, the MMIO device list and logs. |
| ⭐ [`mmu.rs`](../../src/mem/mmu.rs) | The Sv39/Sv48 page-table walker (`walk`), TLB `fill`, `translate`, and the softmmu `load`/`store`/`fetch16` used by the interpreter and the JIT's slow path. |
| ⭐ [`tlb.rs`](../../src/mem/tlb.rs) | `TlbEntry`, MMU indices, `flush_all`, `flush_page`, `set_code_flag`. |
| [`phys.rs`](../../src/mem/phys.rs) | The `Mmio` device trait. |
| [`smc.rs`](../../src/mem/smc.rs) | Only a comment: explains where the self-modifying-code logic lives. |
| [`mod.rs`](../../src/mem/mod.rs) | `GuestVirt`, `GuestPhys`, `prot`, `PAGE_SIZE`, `MemFault`. |

### 2.10 `user/`: Linux user mode (file 15)

| File | What it does |
|---|---|
| ⭐ [`mod.rs`](../../src/user/mod.rs) | `user::run()`: load the program, then hand it to `thread::run_process`. |
| ⭐ [`loader.rs`](../../src/user/loader.rs) | Map ELF segments, load `ld.so` for dynamic programs, build the initial stack (argc, argv, envp, auxv). |
| ⭐ [`syscall.rs`](../../src/user/syscall.rs) | `Syscalls::dispatch`: about 60 Linux syscalls translated to the host, including the `struct stat` conversion, `brk`, `mmap`, `futex`, `clone`. |
| [`thread.rs`](../../src/user/thread.rs) | Guest threads: one host thread each, one fair "global lock" so only one runs guest code at a time. The main user-mode run loop (`guest_thread`). |
| [`signal.rs`](../../src/user/signal.rs) | The host SIGSEGV handler for faults in generated code, and the SIGPROF sampling profiler. |
| [`guest_signal.rs`](../../src/user/guest_signal.rs) | Guest signal handlers: building the Linux `rt_sigframe`, `rt_sigreturn`. |
| [`gdb.rs`](../../src/user/gdb.rs) | A GDB remote-protocol server (`--gdb PORT`). |

### 2.11 `system/`: the emulated computer (file 16)

| File | What it does |
|---|---|
| ⭐ [`machine.rs`](../../src/system/machine.rs) | `boot()`: load kernel/initrd/DTB, create devices and harts, and the **machine loop** (devices → interrupts, run a slice, serve SBI calls, WFI idle). `initramfs()` builds a cpio archive. |
| [`sbi.rs`](../../src/system/sbi.rs) | The built-in firmware interface (SBI): timer, console, IPIs, remote fences, hart start/stop, shutdown. |
| [`fdt.rs`](../../src/system/fdt.rs) | Generates the devicetree blob that tells Linux what hardware exists. |
| [`clint.rs`](../../src/system/clint.rs) | Timer + software interrupts. |
| [`plic.rs`](../../src/system/plic.rs) | Platform-level interrupt controller. |
| [`uart16550.rs`](../../src/system/uart16550.rs) | Serial port (the console). |
| [`syscon.rs`](../../src/system/syscon.rs) | Power-off / reset device. |
| [`virtio_blk.rs`](../../src/system/virtio_blk.rs) | A virtual disk. |
| [`bare.rs`](../../src/system/bare.rs) | Runs the official riscv-tests (bare metal, no OS). |

---

## 3. `tests/`

| File | What it tests |
|---|---|
| [`decoder_vectors.rs`](../../tests/decoder_vectors.rs) | Decoder + disassembler vs 1,496 LLVM golden encodings |
| [`emitter_golden.rs`](../../tests/emitter_golden.rs) | x86 encoder vs the `iced-x86` decoder, all registers and displacement boundaries |
| [`elf.rs`](../../tests/elf.rs) | ELF parser; random input never panics |
| [`jit_lowering.rs`](../../tests/jit_lowering.rs) | Every ALU op on edge cases; block formation; code-cache flush |
| [`ir_passes.rs`](../../tests/ir_passes.rs) | Lift + each optimizer pass preserves meaning (random blocks, IR evaluator) |
| [`fuzz_blocks.rs`](../../tests/fuzz_blocks.rs) | Random blocks: JIT (every allocator level) = interpreter |
| [`fuzz_fp.rs`](../../tests/fuzz_fp.rs) | Random FP blocks: JIT = SoftFloat, bit for bit |
| [`softmmu.rs`](../../tests/softmmu.rs) | Random loads/stores through page tables: JIT TLB path = interpreter MMU |
| [`riscv_tests.rs`](../../tests/riscv_tests.rs) | All 244 official riscv-tests, every engine |
| [`user_programs.rs`](../../tests/user_programs.rs) | C test programs: output identical to QEMU, every engine |
| [`cli.rs`](../../tests/cli.rs) | Command-line behaviour, precise host faults, W^X |
| [`gdb.rs`](../../tests/gdb.rs) | The GDB stub |
| [`linux_boot.rs`](../../tests/linux_boot.rs) | Boot Linux and type commands (ignored by default: needs downloaded images) |
| `common/rvgen.rs` | The random RISC-V block generator used by the fuzzers |

---

## 4. `tools/` and `guest/`

| Path | Purpose |
|---|---|
| `tools/setup.sh` | Installs the RISC-V cross compiler, QEMU and friends with apt (Ubuntu 24.04). |
| `tools/build-guests.sh` | Builds `guest/asm/*.S` and `guest/c/*.c` into `guest/build/*.elf` (with `-O0`/`-O2`, with and without compressed instructions, plus a dynamic version). |
| `tools/build-riscv-tests.sh` | Builds the 244 official tests. |
| `tools/ref-check.sh` | Checks guest programs' output against the recorded QEMU output. |
| `tools/build-bench.sh`, `tools/bench.py` | Builds CoreMark/Dhrystone/fpbench and runs the benchmark matrix. |
| `tools/demo-milestone-a.sh` | One-command demo: CoreMark, interpreter vs JIT. |
| `tools/fetch-guest-images.sh` | Downloads the Ubuntu RISC-V kernel and BusyBox (pinned SHA-256) and builds the initramfs. |
| `tools/boot-bench.py` | Times the Linux boot. |
| `tools/tlb-bench.py` | TLB hit/miss micro-benchmark. |
| `tools/gen-decoder-vectors.py` | Regenerates the decoder golden vectors with `llvm-mc`. |
| `guest/c/*.c` | Small C test programs: hello, fib, printf with floats, malloc, qsort, setjmp, strings, stat, signals, threads, self-modifying code. |
| `guest/asm/*.S` | Tiny assembly tests: hello, loop, fault. |
| `guest/bench/` | Dhrystone shim, FP benchmark, TLB benchmarks. |

---

## 5. Where `unsafe` lives

Rust's safety checks are switched off only where they must be. The allowed places (CLAUDE.md §25):

| File | Why it needs `unsafe` |
|---|---|
| `jit/code_mem.rs` | `mmap`, `memfd_create`, raw writes into the code buffer |
| `jit/trampoline.rs` | turning a code address into a callable function pointer; raw `CpuState` pointer in helpers |
| `jit/dispatch.rs` | the call into generated code |
| `mem/direct.rs` | the guest address space: `mmap`, raw pointer loads and stores |
| `user/signal.rs` | the SIGSEGV handler (reads and changes the interrupted CPU registers) |
| `user/syscall.rs`, `user/thread.rs` | host syscalls with pointers into guest memory |

Every `unsafe` block has a `// SAFETY:` comment explaining why it is correct.

---

## 6. Suggested reading order for the code

Read with the matching study file open next to it.

1. `cpu/state.rs` — know the central data structure first.
2. `isa/inst.rs`, then skim `isa/decode.rs`.
3. `interp/mod.rs` — `step()` is the reference meaning of every instruction.
4. `user/mod.rs` → `user/loader.rs` → `user/thread.rs` (`guest_thread`) — how a program starts and the outer loop.
5. `jit/dispatch.rs` — `Engine::run` for `Jit`, then `select`, `tb_for_key`, `translate_insns`, `exec`.
6. `ir/ops.rs` → `ir/lift.rs` → `ir/opt.rs`.
7. `regalloc/linear_scan.rs`.
8. `backend/x86/lower_ir.rs` — start at `translate()` at the bottom, then `terminator`, `load`, `store`, `interp`.
9. `backend/x86/emit.rs` — `emit_op` and `modrm_rm` first.
10. `jit/code_mem.rs`, `jit/trampoline.rs`, `jit/chain.rs`, `jit/cache.rs`.
11. `user/signal.rs` and the fault functions at the end of `dispatch.rs`.
12. `mem/direct.rs`, `mem/tlb.rs`, `mem/mmu.rs`.
13. `cpu/trap.rs`, then `system/machine.rs`.

A good trick: open a file and read only the `//!` comment at the top and the `///` comments above each function first. The authors wrote these to explain *why*, and they cite the spec section or design decision (like "D30") that each piece implements. You can look up any "D number" in CLAUDE.md §3.

---

## Check yourself

1. Which file would you open to see how a `beq` instruction becomes x86 code?
2. Where is the meaning of a RISC-V instruction defined most simply?
3. Which file decides which TB runs next?
4. Where does the program turn a syscall number into a host system call?
5. Why is `CpuState` marked `#[repr(C)]`, and where are its offsets checked?
