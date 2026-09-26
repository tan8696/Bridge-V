# Bridge-V Roadmap (detailed, phase by phase)

This is the step-by-step build plan for Bridge-V, the RISC-V RV64GC → x86-64 JIT dynamic binary translator.

- **Specs live in [`CLAUDE.md`](../CLAUDE.md).** This file says *what to build, in what order, and how to prove it works*. When a task says "per §N", that's the CLAUDE.md section with the exact design.
- **Every phase ends with a phase report**, `docs/phase-reports/phase-NN-<name>.md`, written from [`TEMPLATE.md`](phase-reports/TEMPLATE.md). A phase is not done until its report is committed.
- The checkboxes (`- [ ]`) are ticked in the same commit that completes the task.

---

## Contents

1. [How to use this roadmap](#1-how-to-use-this-roadmap)
2. [Phase overview](#2-phase-overview)
3. [Dependency graph](#3-dependency-graph)
4. [Budget strategy and priorities](#4-budget-strategy-and-priorities)
5. [Definition of Done (applies to every phase)](#5-definition-of-done-applies-to-every-phase)
6. [Phase 0: Project setup and toolchain](#phase-0-project-setup-and-toolchain)
7. [Phase 1: Front end and reference interpreter](#phase-1-front-end-and-reference-interpreter)
8. [Phase 2: Naive JIT](#phase-2-naive-jit)
9. [Phase 3: Block chaining and jump cache](#phase-3-block-chaining-and-jump-cache)
10. [Phase 4: IR, optimizer and register allocation](#phase-4-ir-optimizer-and-register-allocation)
11. [Phase 5: Milestone A (benchmarks and speedup report)](#phase-5-milestone-a-benchmarks-and-speedup-report)
12. [Phase 6: Floating point in the JIT](#phase-6-floating-point-in-the-jit)
13. [Phase 7: Privileged architecture and SoftMMU (SV39 + inline TLB)](#phase-7-privileged-architecture-and-softmmu-sv39--inline-tlb)
14. [Phase 8: Self-modifying code](#phase-8-self-modifying-code)
15. [Phase 9: Milestone B (boot Linux to a BusyBox shell)](#phase-9-milestone-b-boot-linux-to-a-busybox-shell)
16. [Phase 10: Stretch goals](#phase-10-stretch-goals)
17. [Phase 11: Polish, presentation and resume](#phase-11-polish-presentation-and-resume)
18. [Open questions for the project owner](#18-open-questions-for-the-project-owner)

---

## 1. How to use this roadmap

- **Work strictly in order.** Don't start phase N+1 while phase N's acceptance criteria fail. The one exception is §3, which marks tasks that may overlap.
- **Tasks are IDs like `P2.4`.** Commit messages reference them, e.g. `jit: dual-mapped code buffer (P2.3)`.
- **Each task lists its deliverables** (files), **its tests** and **"done when"** (the objective check).
- **"Size" is a rough scale.** S ≈ < 300 lines of code, M ≈ 300–1000, L ≈ 1000–2500, XL ≈ > 2500. It's a planning aid, not a promise.
- **Record facts, not guesses.** Every number quoted in a report or doc comes from a command whose output is pasted into the phase report.
- **Commit and push after every green task.** The cloud container is ephemeral (CLAUDE.md §26).

## 2. Phase overview

| # | Phase | Main goal | Depends on | Size | Milestone |
|---|---|---|---|---|---|
| 0 | Setup & toolchain | Buildable skeleton, cross-toolchain, guest build scripts, CI | — | M | — |
| 1 | Front end & interpreter | ELF loader, full RV64GC decoder, reference interpreter, Linux user-mode syscalls | 0 | XL | "Runs hello world" |
| 2 | Naive JIT | x86 emitter, W^X code memory, trampolines, dispatcher, 1:1 instruction lowering | 1 | XL | "First JIT code executes" |
| 3 | Chaining & jump cache | Patch block exits to jump directly, inline JALR lookup, budget pre-emption | 2 | M | — |
| 4 | IR & register allocation | IR, optimizer passes, pinned R12–R15, linear-scan allocator, cold stubs, fuzzer | 3 | XL | — |
| 5 | **Milestone A** | CoreMark + Dhrystone, benchmark harness, speedup report | 4 | M | **A (required)** |
| 6 | FP in JIT | Inline SSE for F/D with exact RISC-V semantics | 5 | L | — |
| 7 | Privileged + SoftMMU | M/S/U, traps, interrupts, SV39 walker, inline TLB fast path | 5 | XL | — |
| 8 | SMC | Code-page write protection, TB invalidation, unlinking, FENCE.I | 7 | M | — |
| 9 | **Milestone B** | CLINT/PLIC/UART, SBI, devicetree, Linux 6.6 → BusyBox `/ #` | 7, 8 | XL | **B (stretch)** |
| 10 | Stretch | OpenSBI, SV48, multithreading, dynamic ELF, signals, virtio, … | 9 | — | — |
| 11 | Polish & presentation | README/demo, measured resume bullets, interview material | 5 (or 9) | S | — |

## 3. Dependency graph

```
P0 ──► P1 ──► P2 ──► P3 ──► P4 ──► P5 (Milestone A) ──┬──► P6 (FP JIT)
                                                        │
                                                        └──► P7 (Priv+SoftMMU) ──► P8 (SMC) ──► P9 (Milestone B) ──► P10
P11 (polish) can start after P5 and is revisited after P9.

Allowed overlaps:
  • P1 decoder work (P1.3–P1.6) may start while P0 CI is still being tuned.
  • P2.1 (x86 emitter + golden tests) is independent of P1 and may be built in parallel.
  • P7.3 (Sv39 walker unit tests) is pure Rust logic and may be written during P6.
```

## 4. Budget strategy and priorities

The project is developed in Claude Code cloud sessions on about **$100 of credit**. A full Linux-booting DBT is a large project, and the whole roadmap probably does not fit in that budget. So the plan is tiered:

| Tier | Phases | Why |
|---|---|---|
| **Must-have** | 0 → 5 | Produces Milestone A. It covers three of the four headline subsystems: JIT code generation, block chaining, register allocation. It also produces measured speedup numbers. This is a complete, demo-able, resume-worthy project on its own. |
| **Should-have** | 7 (+ 8) | Adds the fourth subsystem (SV39 + inline TLB) and the SMC story from the interview brief. Phase 7 can be scoped down to "riscv-tests `-v-` pass + TLB microbenchmark" without the Linux boot. |
| **Nice-to-have** | 6, 9 | The FP fast path and the Linux boot are impressive but expensive. Phase 9 alone is probably as large as phases 0–3 combined. |
| **Stretch** | 10 | Only if budget remains. |

**Token-saving rules for every session:**
- Read only the files you need.
- Run targeted tests (`cargo test <filter>`).
- Pipe long logs through `tail`/`grep`.
- Don't paste huge outputs into chat.
- Keep phase reports detailed but factual.

## 5. Definition of Done (applies to every phase)

A phase is **done** only when all of the following hold:

1. Every task checkbox in the phase is ticked, or it was explicitly deferred, with a reason recorded in the phase report.
2. `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` are clean.
3. `cargo test` passes, and so do the phase's integration suites. The exact commands and their output summaries go in the report.
4. The phase's **acceptance criteria** (listed per phase) are met, with evidence.
5. CLAUDE.md is updated:
   - §0 status table.
   - New decisions appended to §3.
   - Any spec that changed during implementation (offsets, register map, encodings).
6. **The phase report** `docs/phase-reports/phase-NN-<name>.md` is written from the template and committed.
7. Everything is committed and pushed.

---

## Phase 0: Project setup and toolchain

**Goal:** an empty but correctly structured Rust project that builds, lints and tests. Plus a working RISC-V cross-toolchain, reproducible guest-program builds, the reference emulators installed, and CI.

**Entry criteria:** repository exists, and CLAUDE.md is committed.

### P0.1 Repository hygiene (S)
- [x] `.gitignore`: `target/`, `guest/build/`, `*.elf`, `*.o`, `Image`, `*.cpio`, `*.dtb`, `third_party/*/build/`, `perf-*.map`, `*.x86dump`.
- [x] `rust-toolchain.toml`: pin `channel = "1.94"` (or `stable`) with the `rustfmt` and `clippy` components.
- [x] `rustfmt.toml` (defaults plus `max_width = 100`) and `clippy.toml` (if needed).
- [x] `.editorconfig`: LF line endings, 4-space indents for Rust, tabs for Makefiles.
- [ ] `LICENSE`: **ask the owner** (§18). Until then, leave it out rather than guess. *(Deferred: awaiting the owner's choice.)*
- **Done when:** `git status` is clean after a build.

### P0.2 Cargo skeleton (S)
- [x] `Cargo.toml` for a single crate with `lib` + `[[bin]] name = "bridgev"`, edition 2024.
- [x] Dependencies per D18: `libc`, `rustc-hash`, `clap` (derive), `anyhow`. `cc` goes in build-deps later (P1.11).
- [x] Dev-dependencies: `iced-x86` (features: `decoder`, `intel`), `proptest`.
- [x] Profiles:
  - `release`: `panic = "abort"`, `lto = "thin"`, `codegen-units = 1`, `debug = 1`.
  - `dev`: `opt-level = 1`, so test runs of the interpreter aren't painfully slow.
- [x] Module tree matching CLAUDE.md §6. Every file gets a `//!` doc comment stating its responsibility, and nothing else yet.
- [x] `src/main.rs` with clap subcommands `run`, `boot`, `disasm`, `bench`. Each prints `not implemented yet (Phase N)` and exits 2. `bridgev --version` works.
- **Tests:** `tests/cli.rs` checks that `bridgev --version` exits 0 and that the subcommands exit 2.
- **Done when:** `cargo build && cargo test && cargo clippy --all-targets -- -D warnings` all pass.

### P0.3 `tools/setup.sh` (S)
- [x] An idempotent script: `apt-get update` then install `gcc-riscv64-linux-gnu g++-riscv64-linux-gnu libc6-dev-riscv64-cross qemu-user qemu-system-misc device-tree-compiler flex bison bc libssl-dev libelf-dev cpio autoconf automake`.
- [x] Skip packages that are already installed. Print the versions of every tool at the end: `riscv64-linux-gnu-gcc --version`, `qemu-riscv64 --version`, `clang --version`, `rustc --version`.
- [x] Fail loudly with a helpful message if apt is blocked by the network policy.
- **Done when:** the script runs twice in a row without errors, and the second run installs nothing.

### P0.4 Guest build infrastructure (M)
- [x] `guest/asm/hello.S`: a bare RV64 Linux program that does `write(1, "hello\n", 6)` then `exit(0)` via `ecall` (a7 = 64, then 93).
- [x] `guest/asm/loop.S`: a tight counted loop (1e8 iterations), then exit with the low byte of the counter. Used later for MIPS measurements.
- [x] `guest/c/hello.c`, `guest/c/printf_float.c`, `guest/c/malloc.c`, `guest/c/qsort.c`, `guest/c/setjmp.c`, `guest/c/strings.c`. Each prints deterministic output.
- [x] `tools/build-guests.sh`:
  - asm: `clang --target=riscv64-unknown-linux-gnu -march=rv64gc -nostdlib -static -fuse-ld=lld`.
  - C: `riscv64-linux-gnu-gcc -static -O2` and `-O0`, each with `-march=rv64gc` and `-march=rv64imafd`.
  - Output goes to `guest/build/<name>[-O0|-O2][-nc].elf`.
- [x] `tests/data/expected/*.out`: expected stdout for each C program, generated with `qemu-riscv64` and committed. They are small text files.
- **Done when:** `file guest/build/hello.elf` reports `ELF 64-bit LSB executable, UCB RISC-V`, and `qemu-riscv64 guest/build/hello.elf` prints `hello`.

### P0.5 Third-party sources (M)
- [x] Git submodules, each pinned to a specific commit that is recorded in the phase report:
  - `third_party/riscv-tests` (with its `env` submodule)
  - `third_party/coremark`
  - `third_party/berkeley-softfloat-3`
- [x] `tools/build-riscv-tests.sh`: `autoconf` if needed, `./configure --with-xlen=64`, then `make isa RISCV_PREFIX=riscv64-linux-gnu-`. Copy `rv64u*-p-*`, `rv64m*-p-*`, `rv64s*-p-*`, `rv64u*-v-*` into `guest/build/riscv-tests/`.
  - Fallback if the linux-gnu GCC misbehaves: override `RISCV_GCC` with clang + lld.
- [x] Record the number of test ELFs produced. The expected order of magnitude is 200+.
- **Done when:** `ls guest/build/riscv-tests | wc -l` is non-zero, and a sample test (`rv64ui-p-add`) runs to completion under `spike` or with `qemu-system-riscv64 -M virt -bios none -kernel <elf> -nographic`.
  - The qemu check is best-effort. Its only purpose is to confirm the ELFs themselves are valid.

### P0.6 Reference emulator sanity (S)
- [x] Run every C guest program under `qemu-riscv64` and diff the output against `tests/data/expected/`.
- [x] `tools/ref-run.sh <elf>` runs a program under qemu and prints its stdout and exit code. Later phases use it for differential comparison.
- **Done when:** every program matches its expected output under qemu.

### P0.7 Test harness scaffolding (S)
- [x] `tests/common/mod.rs`: `guest_elf(name) -> PathBuf`.
  - If the ELF is missing and `BRIDGEV_REQUIRE_GUESTS=1` is set (as in CI), **fail** with the message "run tools/build-guests.sh".
  - If it is missing otherwise, skip the test with a visible `eprintln!`.
- [x] `tests/common/mod.rs`: `run_bridgev(args) -> (stdout, stderr, exit_code)`.
- **Done when:** a placeholder integration test uses both helpers.

### P0.8 Continuous integration (M)
- [x] `.github/workflows/ci.yml` on `ubuntu-24.04`:
  1. checkout with submodules
  2. cache `~/.cargo` and `target/`
  3. `tools/setup.sh`
  4. `cargo fmt --check`
  5. `cargo clippy --all-targets -- -D warnings`
  6. `tools/build-guests.sh`
  7. `cargo test` with `BRIDGEV_REQUIRE_GUESTS=1`
- [x] A separate, optional job builds riscv-tests (cached by submodule commit).
- **Done when:** CI is green on the pushed branch (link the run in the report). If GitHub access isn't available, record that and run the same steps locally.

### P0.9 Documentation scaffolding (S)
- [x] `README.md`: a short description and links to `docs/PROJECT_EXPLAINED.md`, `docs/ROADMAP.md` and `CLAUDE.md`.
- [x] `docs/phase-reports/TEMPLATE.md`, which already exists.
- [x] `docs/BENCHMARKS.md` containing the header "No measurements yet".
- **Done when:** the files exist, and their links resolve on GitHub.

### Phase 0 acceptance criteria
- `cargo build`, `cargo test`, `cargo fmt --check` and clippy are all clean.
- `tools/setup.sh` succeeds.
- `tools/build-guests.sh` produces all the guest ELFs.
- riscv-tests are built.
- qemu reference outputs match.
- CI is green, or local-equivalent evidence exists.

### Phase 0 risks
| Risk | Mitigation |
|---|---|
| apt blocked by the cloud network policy | Check with `curl -sS "$HTTPS_PROXY/__agentproxy/status"`. Use clang + lld for asm tests, which is already installed. Document the policy change needed. |
| riscv-tests won't build with the linux-gnu toolchain | Use clang `--target=riscv64-unknown-elf` with `-nostdlib`, or build only the ISA subset we need. |
| Submodules make clones slow | Use shallow submodules (`shallow = true` in `.gitmodules`). |

### Phase 0 report must include
Tool versions, submodule commits, the list and count of built guest programs, the qemu reference results, CI run link and status, and any environment problems hit.

---

## Phase 1: Front end and reference interpreter

**Goal:** a correct, reasonably fast interpreter that executes RV64GC. It is the golden model for everything later, and the speedup baseline. It must run the riscv-tests `p` suites and static glibc Linux programs.

**Entry criteria:** Phase 0 done.

### P1.1 Core types (S)
- [x] `GuestVirt(u64)` and `GuestPhys(u64)` newtypes (CLAUDE.md §25). `Xreg(u8)` and `Freg(u8)` with range-checked constructors. *(Deviation: registers stay plain `u8`, always < 32 from 5-bit fields; see the Phase 1 report §9.)*
- [x] `ExitReason`, `Exception { cause, tval }` and `Priv { U, S, M }` enums.
- **Tests:** unit tests for constructors and conversions.

### P1.2 ELF64 loader (M), `src/elf.rs`
- [x] Parse the ELF header and validate class, endianness, machine = 243 and type. Parse program headers (`PT_LOAD`, `PT_INTERP`, `PT_GNU_STACK`, `PT_TLS`), section headers, `.symtab` and `.strtab`.
- [x] API: `Elf::parse(&[u8]) -> Result<Elf>`, `segments()`, `entry()`, `symbol(name) -> Option<u64>` (for `tohost`/`fromhost`), `flags()` (RVC and float ABI).
- [x] Reject `PT_INTERP` (dynamic) with a clear error. That support comes in Phase 10.
- **Tests:**
  - Parse every ELF in `guest/build/`.
  - Malformed inputs: truncated header, bad magic, wrong machine, overlapping segments, `p_filesz > p_memsz`. All must return errors, never panic.
  - proptest: random bytes must never panic.

### P1.3 Instruction model (M), `src/isa/inst.rs`
- [x] `enum Inst` covering all of RV64I, M, A (LR/SC and AMO*, W/D, aq/rl), F, D, Zicsr, Zifencei, and the system instructions (ECALL, EBREAK, MRET, SRET, WFI, SFENCE.VMA).
  - Every variant carries its operands (typed register fields and a sign-extended `i64` immediate).
  - Plus `Illegal(u32)`.
- [x] `struct Decoded { inst: Inst, len: u8, raw: u32 }`.

### P1.4 32-bit decoder (L), `src/isa/decode.rs`
- [x] Immediate extractors for I/S/B/U/J (per §7.2), each its own tiny tested function.
- [x] Opcode dispatch per §7.3, with funct3/funct7/funct5/fmt/rm sub-decoding.
- [x] RV64 shift-immediate rules: a 6-bit shamt for SLLI/SRLI/SRAI; for the W forms, `inst[25]` must be 0.
- [x] Reserved and illegal encodings produce `Inst::Illegal`.
- **Tests:**
  - [x] `tools/gen-decoder-vectors.sh` uses `llvm-mc -triple=riscv64 -mattr=+m,+a,+f,+d,+c -show-encoding` to produce `tests/data/rv64_vectors.txt`. Each line holds the asm text and its encoding. There is at least one vector per instruction, with edge immediates (min, max, zero, −1) and every register at least once. The file is committed so the tests don't need llvm-mc.
  - [x] A test decodes every vector and compares it against the disassembler output (P1.6).
  - [x] Negative vectors: a list of known-illegal words.

### P1.5 RVC expansion (M), `src/isa/rvc.rs`
- [x] Expand every RV64C instruction (§7.4) to its 32-bit `Inst` equivalent with `len = 2`. `rd'` maps to x8–x15.
- [x] Reserved encodings: nzimm = 0 cases, the all-zero word, RV32-only slots.
- **Tests:** llvm-mc vectors for all compressed instructions (`-mattr=+c`) must decode to the same `Inst` as their expanded 32-bit form, plus all the reserved cases.

### P1.6 Disassembler (S), `src/isa/disasm.rs`
- [x] objdump-style text (`addi a0, a0, 1`), using ABI register names.
- [x] `bridgev disasm <elf>` prints a linear sweep of the executable segments.
- **Tests:** round-trip against the llvm-mc vector text, after normalizing whitespace and pseudo-instructions.

### P1.7 CPU state v1 (S), `src/cpu/state.rs`
- [x] `#[repr(C, align(64))] CpuState` with the fields the interpreter needs now. Follow the §8.1 layout from the start, so the JIT can reuse it.
- [x] `const _: () = assert!(offset_of!(CpuState, x) == 0)` and so on for every JIT-visible field.
- **Tests:** the offset asserts compile, and a runtime test prints the layout for the report.

### P1.8 Guest memory: direct backend v1 (M), `src/mem/direct.rs`
- [x] Reserve `2^38 + 8 GiB` `PROT_NONE` (§14.1). `map(gaddr, len, prot)` and `unmap`, `protect`.
- [x] Safe accessors for the interpreter: `read_u8…u64` and `write_*`. They return `Result<_, Exception>` using a Rust-side page-permission map, so the interpreter never segfaults the host.
- [x] Separate host-side fast-access paths for the JIT come later (P2).
- **Tests:** map/unmap/protect; reads beyond a mapping return a fault; misaligned accesses work.

### P1.9 Interpreter core (L), `src/interp/`
- [x] A pre-decoded block cache: decode a basic block once into `Vec<Decoded>`, keyed by PC, and execute it with a `match`. This is still an interpreter, but without re-decoding.
- [x] Integer semantics, with every gotcha in §7.5 covered: division edge cases, W-op sign extension, JALR `rd == rs1`, x0 writes, MULHSU.
- [x] Branches, jumps, and `ecall` → `ExitReason::Ecall`.
- [x] Instruction counting (`icount`) for MIPS statistics.
- **Tests:** unit tests per instruction class. Each §7.5 gotcha gets its own named test, e.g. `div_by_zero_returns_all_ones`.

### P1.10 A extension (S)
- [x] LR/SC with a single reservation, and the AMO* ops (W and D). The W forms sign-extend the loaded value.
- **Tests:** SC without an LR fails; SC to a different address fails; AMOMIN/MAX signed vs unsigned.

### P1.11 F and D extensions via SoftFloat (L), `build.rs`, `src/cpu/fp.rs`
- [x] `build.rs` compiles `third_party/berkeley-softfloat-3/source` with the `RISCV` specialization and `SOFTFLOAT_FAST_INT64` (following its `build/Linux-x86_64-GCC` makefile flags) using the `cc` crate.
- [x] A minimal FFI (`extern "C"`) for f32/f64 add, sub, mul, div, sqrt, mulAdd, comparisons, all conversions, `softfloat_roundingMode` and `softfloat_exceptionFlags`.
- [x] Wrappers:
  - NaN-boxing on read and write.
  - Canonical NaN.
  - The frm/DYN rule, with invalid rm raising illegal-instruction.
  - fflags accrual.
  - fmin/fmax semantics.
  - fclass.
  - fsgnj*.
  - fmv.x.* and fmv.*.x.
  - fcvt saturation.
- [x] FP loads and stores: flw/fld/fsw/fsd. flw NaN-boxes its result.
- **Tests:** targeted edge cases (NaN, ±0, ±inf, subnormals, overflow, every rounding mode) plus the riscv-tests `rv64uf`/`rv64ud` suites (P1.13).

### P1.12 CSRs and minimal M-mode (M), `src/cpu/csr.rs`, `src/cpu/trap.rs`
- [x] User CSRs: fflags, frm, fcsr, cycle, time, instret.
- [x] The minimal machine set the riscv-tests `p` environment needs: mhartid, mstatus, misa, medeleg, mideleg, mie, mip, mtvec, mscratch, mepc, mcause, mtval, pmpcfg0/pmpaddr0 (accept writes), satp (Bare only for now).
- [x] CSR instruction semantics per §7.5: no write when rs1 = x0 for RS/RC; no read for CSRRW with rd = x0. Accessing a missing CSR raises illegal-instruction.
- [x] Trap entry to M, and MRET (§15). Plus `ecall` from U/M → cause 8/11.
- **Tests:** CSR read/write rules; trap entry state; MRET state restore.

### P1.13 riscv-tests runner (M), `tests/riscv_tests.rs`, `bridgev run --mode=bare`
- [x] Bare-metal mode:
  - Load the ELF physically at its link address (0x8000_0000) into a RAM mapping.
  - Start in M-mode at the entry point.
  - Stop when `tohost` (found from the symbol table) is written.
  - Result: 1 means pass; `(n << 1) | 1` means test n failed.
  - A timeout of 10M instructions per test.
- [x] The runner iterates every `rv64u{i,m,a,f,d,c}-p-*` ELF and prints a pass/fail table.
- **Done when:** **all** `rv64ui/um/ua/uf/ud/uc-p-*` pass. Record the count in the report.

### P1.14 Linux user-mode loader (M), `src/user/loader.rs`
- [x] Map the PT_LOAD segments with their permissions, zero the bss, set up brk.
- [x] The initial stack per §19: argc, argv, envp, auxv (including AT_RANDOM with 16 bytes from the host `getrandom`, AT_HWCAP = IMAFDC, AT_PHDR/PHENT/PHNUM/ENTRY, AT_PAGESZ) and the strings, with 16-byte alignment.
- **Tests:** stack layout unit test (parse it back and verify every pointer); a guest `env.c` program prints argv/envp.

### P1.15 Syscalls (L), `src/user/syscall.rs`
- [x] The dispatch table for the §19 set, plus **`riscv_hwprobe` (258)**. Newer glibc may probe it at startup. Return `-ENOSYS` or a minimal honest answer.
- [x] Struct translation: `stat`/`newfstatat`/`fstat` (asm-generic 128-byte layout ↔ x86-64 144-byte layout), `statx`, `uname` (machine = "riscv64"), `timespec`.
- [x] `mmap`/`munmap`/`mprotect`/`brk` go through the direct backend's allocator (top-down mmap region).
- [x] `readlinkat("/proc/self/exe")` returns the guest path.
- [x] Unknown syscalls are logged once with their number and name, and return `-ENOSYS`.
- **Tests:** every C guest program's stdout and exit code match `tests/data/expected/`. The `stat` test checks translated fields.

### P1.16 CLI, trace and stats (S)
- [x] `bridgev run --engine=interp [--trace=insn] [--stats] <elf> [args]`.
- [x] `--stats` prints icount, wall time, MIPS and exits by reason.
- [x] `--trace=insn` prints `pc: raw disasm` per instruction, for debugging.

### Phase 1 acceptance criteria
- All `rv64u{i,m,a,f,d,c}-p-*` riscv-tests pass under the interpreter.
- All C guest programs (O0/O2, rv64gc/rv64imafd) match the qemu reference output.
- The decoder vector tests pass (count recorded).
- The first interpreter MIPS number is measured on `loop.elf` and CoreMark (built but not yet official). It is recorded as a baseline, not judged.

### Phase 1 risks
| Risk | Mitigation |
|---|---|
| glibc startup needs unexpected syscalls or CSRs | Log unknown syscalls and CSRs, and implement them on demand. Compare with `strace qemu-riscv64 …`, or `qemu-riscv64 -strace`. |
| SoftFloat build friction | The fallback is `rustc_apfloat` (D13). It must still pass rv64uf/ud. |
| The interpreter is too slow for large tests | A pre-decoded cache, `opt-level = 1` in dev, and release builds for benchmarks. |

### Phase 1 report must include
The instruction coverage table, decoder vector count, the riscv-tests pass table, the syscall list implemented, every deviation from §7/§19, the baseline MIPS, and bugs found through the reference comparison.

---

## Phase 2: Naive JIT

**Goal:** execute guest code as generated x86-64 machine code. Every RISC-V instruction is lowered 1:1, all guest registers stay in `CpuState` memory, and every block returns to the dispatcher. Correctness first. This phase proves the whole JIT pipeline works end to end.

**Entry criteria:** Phase 1 done. (P2.1 may start earlier.)

### P2.1 x86-64 emitter (L), `src/backend/x86/emit.rs`, `regs.rs`
- [x] `enum Reg` (RAX…R15, with `.low3()` and `.rex_bit()`), `struct Mem { base, index: Option<(Reg, Scale)>, disp: i32 }` and `enum Cond`.
- [x] Core encoders: `rex()`, `modrm()`, `sib()`, `mem_operand()`, handling every §11.1 gotcha (RSP/R12 need SIB; RBP/R13 need disp8; SIL/DIL need REX; disp8 vs disp32 selection).
- [x] The instruction set from §11.2: mov (all forms), movsx/movzx/movsxd, lea, the ALU group (reg-reg, reg-mem, reg-imm8/imm32), test, shifts (imm and CL), imul, mul/div/idiv, cqo, setcc, cmovcc, jcc/jmp (rel8/rel32), call (rel32, reg, [rip+d32]), ret, nop (1–9 byte forms), xchg, lock xadd, lock cmpxchg, mfence, push/pop (trampolines only).
- [x] Labels and fixups: `new_label()`, `bind(label)`, `jcc_label(cond, label)` (rel32 by default, with an optional short form), and `patch_rel32(at, target)`.
- **Tests** (`tests/emitter_golden.rs`):
  - [x] For every instruction form × **all 16 registers** as dst/src/base/index (skipping invalid combinations) × disp ∈ {0, 1, −128, 127, 128, −129, i32::MIN, i32::MAX}: encode, decode with `iced-x86`, and compare the formatted text with the expected text.
  - [x] Named regression tests for each gotcha: `[r12]`, `[r13]`, `[rsp+8]`, `[rbp]`, `mov sil, al`, and so on.
  - [x] Label tests: forward and backward, near and far.

### P2.2 CPU feature detection (S), `src/backend/x86/features.rs`
- [x] `cpuid` for BMI1, BMI2, FMA, POPCNT, LZCNT, AVX2, SSE4.1. A global `HostFeatures`.
- [x] `--no-host-features` forces the baseline code paths, so they are tested too.

### P2.3 Code memory with W^X (M), `src/jit/code_mem.rs`
- [x] Dual mapping (memfd, RW + RX views) and the `--wx=mprotect` fallback (§12). A bump allocator with 16-byte alignment, and `flush()`.
- [x] Both views stay mapped for the process lifetime. Assert there is never an RWX mapping (a debug-build check via `/proc/self/maps` in tests).
- **Tests:** "hello JIT": emit `mov eax, 42; ret`, transmute it to `extern "sysv64" fn() -> u64`, call it and get 42. Test both W^X modes. Also check that a write through the RW view is visible through the RX view.

### P2.4 Trampolines (M), `src/jit/trampoline.rs`
- [x] Generate `enter_jit` and `exit_jit` into the code buffer at startup, exactly per §8.4. Pinned registers aren't used yet in Phase 2, but save and restore every callee-saved register anyway.
- [x] A helper address table for calls into Rust (`call [rip+disp32]`).
- **Tests:**
  - A hand-emitted block increments `cpu.x[5]` and exits with code 7. Verify both.
  - A callee-saved preservation test: an `asm!` harness sets RBX/RBP/R12–R15 to sentinels, calls `enter_jit` with a block that clobbers them, and checks they are restored.

### P2.5 Translation cache v1 (M), `src/jit/cache.rs`
- [x] `TranslationBlock` per §13.1 (the chaining fields can be unused), `tb_map`, allocation of TB metadata, and full flush when the code buffer is full.
- [x] Block formation rules per §13.2.
- **Tests:** a block ends at a branch; a block ends at the page end; a max-length cut; flush-when-full (use a tiny cache size in the test).

### P2.6 Naive lowering (L), `src/backend/x86/lower.rs` (v1: direct from `Inst`, no IR yet)
- [x] For each instruction: load the operands from `CpuState` into R10/R11/RAX…, compute, and store the result back. x0 reads become constant 0, and x0 writes are skipped.
- [x] Memory ops in the direct backend: `[rbx + reg + disp32]` with RBX = `mem_base` (load it in `enter_jit`).
- [x] Division guards for all four division cases (§7.5). MULH/MULHU/MULHSU.
- [x] Block exits:
  - Branches: `cmp` + `jcc` to two exit stubs. Each stub sets `pc`, sets the exit code, and `jmp exit_jit`.
  - JAL: a direct exit.
  - JALR: compute the target, store `pc`, and take exit slot 2.
- [x] ECALL/EBREAK exit with a reason. Unsupported instructions (FP, CSR, AMO at first) use `call helper_interp_one` with a full state sync (D14).
- [x] `pcmap` (host offset → guest pc) per TB.

### P2.7 Dispatcher (M), `src/jit/dispatch.rs`
- [x] The main loop per §5, without chaining. It handles exit reasons: syscalls (reusing P1.15), exceptions, halt, and the `tohost` write in bare mode.
- [x] A SIGSEGV handler (sigaltstack): map host RIP → TB → guest pc via `pcmap`, then report the guest fault and exit 139. (Phase 8 extends this for SMC.)
- **Tests:** all P1 riscv-tests and C programs under `--engine=jit`.

### P2.8 Lockstep engine (M), `--engine=lockstep`
- [x] For each TB:
  1. Snapshot the registers.
  2. Run the **interpreter** over the TB's instructions with a **write log** of (addr, old, new).
  3. Undo the logged writes and restore the registers.
  4. Run the **JIT** TB.
  5. Compare all x/f registers, pc and fcsr, and check that memory at every logged address equals the interpreter's new value.
- [x] On divergence, print the guest disassembly, the x86 dump (via iced-x86) and the register diff, then abort.
- **Tests:** lockstep over all suites; a deliberately broken lowering (behind a test-only flag) is caught.

### P2.9 Debug tooling (S)
- [x] `--dump-x86` (writes a file per TB and prints disassembly with iced-x86 behind a `disasm` cargo feature), `--perf-map`, and `--stats` (TBs, code bytes, exits by reason, translate vs execute time).

### Phase 2 acceptance criteria
- Every Phase-1 suite passes under `--engine=jit` and `--engine=lockstep` with no divergence.
- The emitter golden test count and pass rate are recorded.
- The first **JIT vs interpreter MIPS** comparison is recorded (`loop.elf`, CoreMark). The naive JIT may be only a modest speedup, and that's fine.

### Phase 2 risks
| Risk | Mitigation |
|---|---|
| Encoding bugs crash the host | Exhaustive golden tests first. Run under `gdb` with `--dump-x86`, and use lockstep to localize. |
| Unwinding through JIT frames | `panic = "abort"`, and helpers wrapped with `catch_unwind` → abort (§8.4). |
| Stack misalignment on helper calls | An alignment assert helper called from JIT code in debug builds. |

### Phase 2 report must include
The emitter coverage table, a hello-JIT walkthrough (bytes and disassembly), a sample TB dump of real guest code, test pass tables for jit and lockstep, the first speedup numbers, and bugs found.

---

## Phase 3: Block chaining and jump cache

**Goal:** remove the dispatcher round-trip. Patch direct exits to jump straight into the successor TB, make JALR use an inline jump cache, and keep chained loops pre-emptible with the budget counter.

**Entry criteria:** Phase 2 done.

### P3.1 Exit slot layout (M)
- [x] The branch exit layout per §13.3: `jcc rel32` is slot 1 and `jmp rel32` is slot 0, with NOP padding so that each **rel32 field is 4-byte aligned**. Stubs go at the TB tail.
- [x] `ExitSlot { patch_off, target_pc, linked }` is filled at translation time.
- **Tests:** alignment asserts for every emitted exit; the stubs' behaviour is unchanged from Phase 2.

### P3.2 Patching and unlinking (M), `src/jit/chain.rs`
- [x] `link(a, slot, b)`: compute the rel32 against **RX** addresses, write it through the RW view with one aligned `u32` store, and push `(a, slot)` onto `b.incoming`.
- [x] `unlink_all_incoming(b)`: restore each predecessor's rel32 to point at its own stub.
- [x] `may_link` rules (§13.3). `--no-chain` disables linking.
- **Tests:**
  - A chained loop (`loop.elf`) runs correctly.
  - The unlink test: link A→B, invalidate B, run again, and A must exit via its stub rather than jump into stale code.
  - A patch-arithmetic unit test that reuses the CLAUDE.md §28.5 worked example (`E9 BB 0F 00 00`).

### P3.3 Budget prologue (S)
- [x] Every TB starts with `sub qword [rbp+BUDGET], n ; jl budget_stub` (§13.3, D12). The dispatcher sets the budget to `SLICE` (default 100 000 instructions).
- [x] icount stays exact: executed = initial budget − remaining. Correct for partially executed TBs on exceptions, using the instruction index from the fault site.
- **Tests:** an infinite chained loop still returns to the dispatcher (verify with a test-only "stop after N slices"); icount matches the interpreter exactly on all suites.

### P3.4 Jump cache (M)
- [x] A 4096-entry `{pc, host}` table in `CpuState`, with the inline lookup sequence of §13.4 for JALR. A miss exits with `EXIT_LOOKUP`; the dispatcher translates the target and fills the entry.
- [x] Flush rules (§13.4).
- **Tests:** a recursive `fib` program (JALR-heavy) produces correct output; stats show a jump-cache hit rate above 90% on it; flush on cache flush.

### P3.5 Statistics (S)
- [x] Chain patches, unlinks, jump-cache hits and misses, dispatcher entries per million guest instructions.

### Phase 3 acceptance criteria
- All suites pass under jit and lockstep, with and without `--no-chain`.
- Recorded: MIPS with vs without chaining on `loop.elf`, `fib`, and the CoreMark build; the fall in dispatcher entries; the jump-cache hit rate.

### Phase 3 report must include
Before and after exit layouts (disassembly), an explanation of one real patch with its bytes, the performance table, and the chaining and jump-cache statistics.

---

## Phase 4: IR, optimizer and register allocation

**Goal:** replace the 1:1 lowering with the IR pipeline of CLAUDE.md §9–10. That means optimizer passes, pinned R12–R15, linear-scan allocation with guest-register caching, and cold stubs with state maps. This is the phase where the JIT becomes fast.

**Entry criteria:** Phase 3 done.

### P4.1 IR definitions and printer (M), `src/ir/ops.rs`
- [x] The ops per §9 (as amended by D34), the `Block` container, and a readable text printer (`--dump-ir`).

### P4.2 Lifter (L), `src/ir/lift.rs`
- [x] `Inst` → IR for all integer, memory and branch instructions. FP, CSR, AMO and system ops become `Interp` (the D14 fallback continues; D34).
- [x] `InsnStart` markers (`Op::Insn`).
- **Tests:** a tiny **IR evaluator** (test-only) executes the IR directly, and is compared against the interpreter on all instruction unit tests. This isolates lifter bugs from backend bugs.

### P4.3 Optimizer passes (M), `src/ir/opt.rs`
- [x] Guest-register forwarding, read CSE, constant folding (LUI/ADDI/AUIPC/identities), constant branches, and dead write-back elimination, respecting fault sites (§9).
- **Tests:** a golden IR test per pass (input IR → expected IR). The IR evaluator gives the same results before and after each pass (property test).

### P4.4 Liveness and intervals (S), `src/ir/liveness.rs`
- [x] Backward liveness, intervals `[def, last_use]`, and fixed constraints (RAX/RDX, RCX, helper-call clobbers).

### P4.5 Pinned registers (M)
- [x] R12–R15 hold x2/x1/x10/x15 (§8.2). Update `enter_jit`/`exit_jit` to load and store them. The block-boundary ABI (§8.3) is enforced everywhere, including stubs, helper calls (full sync writes pinned to `CpuState` and reloads after) and the jump cache.
- [x] `--regalloc=pinned`. `--pin=` to override the set.
- [ ] ~~`--check-abi` (debug builds): verify the pinned registers against a shadow copy at block boundaries.~~ Dropped: lockstep and the fuzzer subsume it (D41).

### P4.6 Linear-scan allocator (L), `src/regalloc/linear_scan.rs`
- [x] Poletto–Sarkar over the 7-register pool (plus RBX in softmmu mode later), with fixed-register handling.
- [x] Guest-register caching with clean/dirty state. Spill choice by furthest next use: a clean guest register is dropped for free, a dirty one is written home, a temporary goes to a `cpu.spill[k]` slot.
- [x] Write-back of dirty registers on every exit path.
- **Tests:** allocator unit tests on synthetic IR (pressure > 7 registers, fixed constraints, back-to-back DIVs).

### P4.7 Lowering v2 and cold stubs (L), `src/backend/x86/lower.rs`
- [x] IR → x86 using allocator assignments (`backend/x86/lower_ir.rs`), with better instruction selection: `lea` for add and addi, `imul` 3-operand, BMI2 shifts when available, `xor`-zeroing, `setcc` handling.
- [x] Cold stubs at the TB tail for exits and the budget; faults use fault-site state maps resolved by the dispatcher (D37), helper calls do an inline full sync (D36). **State maps** at each fault site: pc plus the dirty guest registers → host registers (§15). The hot path stays spill-free.
- [x] `--regalloc=none|pinned|linear` all keep working (`none` = the Phase-3 behaviour).

### P4.8 Random block fuzzer (M), `tests/fuzz_blocks.rs`
- [x] A proptest generator of valid RV64GC straight-line sequences (integer + memory into a scratch page + branches at the end) with random initial registers.
- [x] Execute under the interpreter and under the JIT at each regalloc level, and compare. Failing cases are shrunk and saved as regression tests.
- [x] CI runs a fixed-seed smoke run (about 10k blocks: 2000 blocks × 5 configurations, `PROPTEST_RNG_SEED` in `ci.yml`). The ≥ 1e6-block run is done locally or on a schedule, and recorded in the report.

### P4.9 Pinned-set profiling (S)
- [x] `--stats=regs`: a static and dynamic guest-register use histogram. Run it on CoreMark, Dhrystone and the C programs. If another set beats x2/x1/x10/x15, append a decision (D19+) and change the default.

### Phase 4 acceptance criteria
- The fuzzer is clean over ≥ 1e6 random blocks.
- All suites pass at each regalloc level, under jit and lockstep.
- A per-level speedup table is recorded (`none` → `pinned` → `linear`).

### Phase 4 report must include
IR dumps of one real block before and after the passes, the allocator decisions for that block, its generated x86 (hot path + cold stub), the fuzz statistics, the speedup table and the register-use histogram.

---

## Phase 5: Milestone A (benchmarks and speedup report)

**Goal:** the required demo. CoreMark and Dhrystone run correctly under the interpreter and every JIT configuration, and a reproducible harness produces the speedup table.

**Entry criteria:** Phase 4 done.

### P5.1 Benchmark builds (S)
- [x] CoreMark: `make PORT_DIR=linux` with `CC=riscv64-linux-gnu-gcc`, `-O2 -march=rv64gc -static -DPERFORMANCE_RUN=1`; iterations are passed at run time and sized by the harness (≥ 10 s). Native x86-64 build from the same source (`tools/build-bench.sh`, D43).
- [x] Dhrystone ("C, Version 2.2" per its source; the riscv-tests copy, BSD, unmodified + a Linux shim; provenance in the report): guest and native builds, with `-O2`.
- [x] Record the exact commits, flags and binary hashes (`guest/build/bench/BUILDINFO.txt`, copied into the results JSON).

### P5.2 Benchmark harness (M), `tools/bench.py` (D43)
- [x] The configuration matrix per §22: interp; jit naive; +chain; +pinned; +linear; softmmu (n/a until Phase 7); `qemu-riscv64`; native (`tools/bench.py`).
- [x] Procedure: `taskset -c 2`, 1 warm-up run, 5 measured runs, median/min/max. Validate the CoreMark output ("Correct operation validated") on **every** run (Dhrystone: every self-check value).
- [x] Output: a markdown table plus a JSON file with host info (CPU model, kernel, rustc version, commit, date).

### P5.3 Profiling and tuning pass (M)
- [x] Translation-time share, top TBs by time (`--profile-tbs` sampling, D44), code size per guest instruction.
- [x] At least one measured tuning iteration. Keep changes that win and revert changes that don't, with before/after numbers (kept: budget in R9, D46; reverted: loop-resident registers, D45).

### P5.4 Results documentation (S)
- [x] `docs/BENCHMARKS.md`: tables, methodology, environment and the caveat that this is a noisy cloud VM.
- [x] `tools/demo-milestone-a.sh`: builds, runs the interpreter and the JIT on CoreMark, and prints the speedup.

### Phase 5 acceptance criteria
- CoreMark validates under every engine and configuration.
- `docs/BENCHMARKS.md` has the full table.
- The demo script works from a clean checkout (after `tools/setup.sh`).

### Phase 5 report must include
The complete results table, methodology, analysis (where time goes and why each level helps), the tuning experiments, and an honest comparison against qemu.

---

## Phase 6: Floating point in the JIT

**Goal:** replace the FP helper calls with inline SSE2/FMA3 code where the semantics can be matched exactly, and keep SoftFloat for the rest.

**Entry criteria:** Phase 5 done.

### P6.1 FP register handling (M)
- [ ] FP values in XMM scratch registers within a TB (a simple allocation, or load/store per instruction to start). NaN-boxing checks on single-precision reads; writes box.

### P6.2 Arithmetic fast paths (L)
- [ ] add/sub/mul/div/sqrt (S and D) inline when the rounding mode is statically RNE (or DYN with frm = RNE checked at translate time via `TbFlags`, ending the TB on frm writes).
- [ ] NaN canonicalization after each op: `ucomisd` + `jp` to a cold fix-up.
- [ ] fmadd/fmsub/fnmadd/fnmsub with FMA3 when available, else SoftFloat.

### P6.3 fflags via MXCSR (M)
- [ ] Clear the MXCSR exception bits at TB entry if the TB contains FP ops. Read them back (`stmxcsr`) on exits and before fcsr reads, and OR the mapped bits into fflags (§17).
- [ ] Verify underflow/tininess behaviour matches SoftFloat. If it doesn't, route the affected ops to helpers and record a decision.

### P6.4 Conversions, compares, min/max, sign ops (M)
- [ ] fcvt with correct RISC-V saturation, done inline with fix-up branches or via helpers. feq/flt/fle via `ucomisd`/`comisd`, with correct NV semantics (flt/fle signal on quiet NaN, feq doesn't). fsgnj via bit operations. fmin/fmax via helper or careful inline code.

### P6.5 FP fuzzing (M)
- [ ] Random operands biased toward special values (NaN payloads, ±0, ±inf, subnormals, the max/min normals), with random rm. The inline JIT must match the SoftFloat interpreter bit-exactly, **fflags included**.

### Phase 6 acceptance criteria
- rv64uf/ud pass under jit and lockstep.
- The FP fuzzer is clean (≥ 1e6 cases).
- Measured speedup on an FP-heavy benchmark (a small nbody/linpack-style guest program added in P6.5).

### Phase 6 report must include
The list of ops inline vs helper, how each semantic difference was handled, fuzz statistics and the performance table.

---

## Phase 7: Privileged architecture and SoftMMU (SV39 + inline TLB)

**Goal:** full M/S/U privilege support, traps and interrupts, an SV39 page walker, and the **inline software TLB fast path** in JIT code.

**Entry criteria:** Phase 5 done. (Phase 6 is not required.)

### P7.1 Complete CSR file (L)
- [ ] All M and S CSRs in §15 and §20: sstatus/sie/sip as views, misa, mcounteren/scounteren, menvcfg/senvcfg, stvec/sepc/scause/stval/sscratch, satp with Sv39 (and Bare), and the pmp CSRs (accept, allow all).
- [ ] WARL behaviour for the fields we don't implement.
- [ ] TVM/TW/TSR trapping behaviour.

### P7.2 Traps and interrupts (M), `src/cpu/trap.rs`
- [ ] Delegation via medeleg/mideleg, trap entry to M or S, MRET/SRET, vectored mode, and the interrupt priority and enable logic (§15).
- [ ] The dispatcher delivers interrupts between TBs. The instructions listed in §15 end the TB. WFI.
- **Tests:** unit tests for every delegation and enable combination; riscv-tests `rv64mi-p-*` and `rv64si-p-*`.

### P7.3 Physical memory bus (S), `src/mem/phys.rs`
- [ ] RAM plus a sorted MMIO device list (§14.2), and access faults outside them.

### P7.4 Sv39 walker (M), `src/mem/mmu.rs`
- [ ] The walk, permission checks, A/D update, superpage alignment and canonical-address check, exactly per §14.3. Sv48 is left behind a `todo` to be enabled in Phase 10.
- **Tests (§21 item 8):** hand-built page tables covering 4K/2M/1G pages, misaligned superpages, every U/SUM/MXR/priv combination, A/D updates, non-canonical addresses, invalid PTE encodings, and faults while reading a PTE itself.

### P7.5 TLB (M), `src/mem/tlb.rs`
- [ ] A direct-mapped TLB per MMU index in `CpuState` (§14.4), with fill, flag bits (MMIO, CODE, WATCH) and the flush rules. The MMU index goes into `TbFlags`.

### P7.6 Inline fast path in JIT code (L)
- [ ] The load and store sequence per §14.4 (tag compare, addend, access). Misaligned accesses always take the slow path.
- [ ] Instruction fetch at translate time goes through the exec TLB and walker. Cross-page jumps go through the jump cache, validated against `addr_code`.
- [ ] Cold slow-path stubs call `helper_load_slow`/`helper_store_slow` with state maps. An exception exit path.
- **Tests:** a fast path vs slow path equivalence fuzz (random vaddrs over a mapped/unmapped layout); MMIO access through the slow path.

### P7.7 System-mode test runner (M)
- [ ] `bridgev run --mode=bare` extended with S/U support and the Sv39 environment for riscv-tests `-v-` variants.
- **Done when:** `rv64mi-p-*`, `rv64si-p-*` and **all `rv64u*-v-*`** pass under interp, jit and lockstep.

### P7.8 SoftMMU in user mode (S)
- [ ] `--mem=softmmu` for Linux user programs. The "page table" is the guest's mmap state, so the TLB fast path gets exercised by CoreMark. Add this configuration to the benchmark matrix.

### P7.9 TLB microbenchmark (S)
- [ ] A guest loop of N loads over a working set that fits in the TLB, versus one that is larger than it. Measure host cycles per access with `rdtsc` around the runs, subtract the loop overhead, and report the hit vs miss cost. **These are the numbers behind the "45 → 4 cycles" resume bullet. Use whatever is actually measured.**

### Phase 7 acceptance criteria
- `rv64mi/si-p-*` and `rv64u*-v-*` pass (interp, jit, lockstep).
- Walker unit tests pass.
- Recorded: the TLB microbenchmark and softmmu vs direct CoreMark numbers.

### Phase 7 report must include
The CSR coverage table, walker test matrix, a TLB fast-path disassembly from a real TB, microbenchmark methodology and results, and the direct vs softmmu comparison.

---

## Phase 8: Self-modifying code

**Goal:** stay correct when guest code writes to memory that holds translated code, in both memory backends, with chained blocks correctly unlinked.

**Entry criteria:** Phase 7 done.

### P8.1 Code-page tracking (S), `src/mem/smc.rs`
- [ ] `page_tbs: phys_page → [TbId]` is populated at translation time. Mark the pages.

### P8.2 SoftMMU write detection (S)
- [ ] The `TLB_CODE` flag on `addr_write` for code pages. The store slow path detects it.

### P8.3 Direct-mode write protection (M)
- [ ] `mprotect(PROT_READ)` on host pages backing code pages. The SIGSEGV handler distinguishes an SMC trap from a genuine guest fault using the tracked guest protections (§14.1). For an SMC trap: invalidate, unprotect, return and retry.

### P8.4 Invalidation (M)
- [ ] Remove the TBs from `tb_map` and the jump cache, unlink every incoming chain, and clear the tracking. If the running TB is affected, `SMC_SELF` makes it exit after the store (§16).

### P8.5 FENCE.I and flush syscall (S)
- [ ] FENCE.I ends the TB and flushes the jump cache. `riscv_flush_icache` (259) does the same. `--smc=flush-on-fence` provides the debug cross-check.

### P8.6 SMC test programs (M)
- [ ] (a) Write a function into an RWX guest mmap, call it, rewrite it, and call it again (with FENCE.I).
- [ ] (b) The same without FENCE.I, which must still be correct thanks to eager invalidation.
- [ ] (c) A loop that patches its own next iteration.
- [ ] (d) A tiny "JIT inside the guest" that generates and runs code repeatedly.
- [ ] (e) A chained-predecessor case: A→B chained, B's page rewritten, A must not jump into stale code.

### Phase 8 acceptance criteria
All SMC programs pass under the direct and softmmu backends, jit and lockstep, with chaining enabled.

### Phase 8 report must include
Walkthroughs of the invalidation sequence (with logs), the test matrix, and the overhead of SMC tracking on CoreMark (should be about zero).

---

## Phase 9: Milestone B (boot Linux to a BusyBox shell)

**Goal:** boot an unmodified Linux 6.6 kernel under Bridge-V in system mode, and reach an interactive BusyBox `/ #` shell.

**Entry criteria:** Phases 7 and 8 done.

### P9.1 Devices (L), `src/system/`
- [ ] CLINT: mtime from the host monotonic clock (or icount under `--deterministic`), mtimecmp → MTIP, msip → MSIP.
- [ ] PLIC: priorities, pending, per-context enable and threshold, claim/complete. Context 1 (S-mode) → SEIP.
- [ ] UART 16550A: the register model per §20.1. TX goes to stdout. A stdin reader thread (raw tty) feeds the RX FIFO and raises IRQ 10 via the PLIC.
- [ ] syscon test finisher: poweroff and reboot.
- **Tests:** a device unit test for each register-level behaviour.

### P9.2 Devicetree generator (M), `src/system/fdt.rs`
- [ ] An FDT blob writer (§20.3). Validate it by decompiling with `dtc -I dtb -O dts` and diffing against the expected `.dts` in a test.

### P9.3 Boot flow and built-in SBI (M), `src/system/machine.rs`, `sbi.rs`
- [ ] Image loading and header validation, DTB placement, and S-mode start state (§20.2).
- [ ] The SBI extensions from the §20.2 table: BASE, TIME, IPI, RFENCE, HSM, SRST, DBCN, plus legacy console.

### P9.4 Guest image build and caching (M), `guest/linux/`
- [ ] `build.sh`: fetch Linux 6.6.x and BusyBox 1.36.x (with pinned versions and checksums). Build the kernel with `defconfig` + `bridgev.config`, build BusyBox statically, create the initramfs (`/init` script), and embed it.
- [ ] **Validate the image in `qemu-system-riscv64 -M virt` first** (the reference boot log).
- [ ] Cache the artifacts outside git (a GitHub release or CI artifact). `tools/fetch-guest-images.sh` downloads them. Record the SHA-256 checksums.

### P9.5 Bring-up (L)
- [ ] Boot under `--engine=interp` first, with `earlycon=sbi` for early output. Then boot under jit, using lockstep to find divergences.
- [ ] Record every bring-up bug and its fix in the report. Typical culprits are FS-state handling, sstatus views, timer interrupts, SUM/MXR, A/D bits, `sfence.vma`, WFI and UART IRQ flow.

### P9.6 Automated boot test (M), `tests/linux_boot.rs` (ignored by default, run in a dedicated CI job)
- [ ] Spawn `bridgev boot` with piped stdio and wait for `/ #` (with a timeout). Send `uname -a; cat /proc/cpuinfo; ls /; poweroff`, check the output, and expect a clean exit via SRST or syscon.

### P9.7 Boot performance (S)
- [ ] Time to shell, MIPS during boot, TLB miss rate, and a comparison with `qemu-system-riscv64` TCG on the same image.

### Phase 9 acceptance criteria
- Linux 6.6 reaches `/ #` under jit.
- The automated UART test passes.
- The boot metrics are recorded.

### Phase 9 report must include
The full boot log (trimmed), the bring-up bug diary, the device coverage, performance vs qemu, and reproduction steps from a clean checkout.

---

## Phase 10: Stretch goals

Pick items in this order, as budget allows. Each gets its own mini-report (`phase-10-<item>.md`).

| Item | Plan outline | Done when |
|---|---|---|
| OpenSBI boot | Full M-mode path; load `fw_jump.bin`; MPRV and misaligned emulation paths | Linux boots via OpenSBI |
| Sv48 | 4-level walker; satp mode 9; DT `mmu-type = "riscv,sv48"` | Walker tests + Linux boots with sv48 |
| Multithreaded user mode | `clone`/`futex`, per-thread `CpuState`, shared cache with a mutex + exclusive sections, `lock`-based AMOs, MFENCE for W→R fences | pthread tests and multithreaded CoreMark pass; scaling numbers |
| Dynamic ELF | Load PT_INTERP (`ld-linux-riscv64-lp64d.so.1`) from a sysroot (`-L` flag like qemu) | Dynamically linked hello runs |
| Guest signals | rt_sigframe setup and rt_sigreturn; SIGSEGV delivery to guest handlers | Signal test programs pass |
| virtio-blk | virtio-mmio transport and a block device backed by a file | Linux mounts an ext2 image |
| Return-address stack | Predict `ret` targets via a shadow stack in `CpuState` | Measurable speedup on call-heavy code |
| Superblocks/traces | Hot-path trace formation across TB boundaries | Measurable speedup |
| GDB stub | Remote serial protocol, interpreter-based single step | `gdb-multiarch` can break and step |
| SMP guest | Multiple harts, IPIs, per-hart TLBs | Linux boots with 2+ harts |

---

## Phase 11: Polish, presentation and resume

- [ ] The README contains a demo (an asciinema recording or screenshots of the speedup and boot) and quick-start commands.
- [ ] `docs/PROJECT_EXPLAINED.md` is updated with the real measured results and final architecture details.
- [ ] Resume bullets in CLAUDE.md §28.1 are rewritten with **measured numbers only**.
- [ ] Interview prep: re-derive the §28.5 byte encodings against the real emitter output. Prepare the whiteboard walkthrough from real TB dumps.
- [ ] Optional: a blog post or write-up of the design, with lessons learned.

---

## 18. Open questions for the project owner

These are decisions only you should make. Until you answer, work proceeds without them.

1. **License:** MIT, Apache-2.0, dual MIT/Apache-2.0 (the Rust-ecosystem convention), or GPL? This affects whether we may borrow ideas or code from GPL projects like QEMU; today the rule is "reference only, never copy".
2. **Milestone B commitment:** attempt the Linux boot (Phase 9), or stop at Milestone A plus Phase 7 (the SV39 + TLB demo via riscv-tests)?
3. **Kernel image caching:** is creating GitHub Releases on this repository OK for storing the built kernel and initramfs?
4. **CI budget:** is a scheduled long fuzz job (≈ 1 h) on GitHub Actions acceptable?
