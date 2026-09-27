# CLAUDE.md — Bridge-V

**Bridge-V: a dynamic binary translator (DBT) that JIT-compiles RISC-V RV64GC guest code into native x86-64 host code.**
Like QEMU TCG, Apple Rosetta 2, box64 and FEX-Emu, it translates whole guest basic blocks into x86-64 machine code, links them together in a translation cache, and runs them directly on the host CPU. It does not interpret one instruction at a time in a `switch(opcode)` loop.

This file is the single source of truth for the project: scope, architecture, design decisions, encodings, roadmap and working rules. Read it before touching code, and keep it current (see §27).

---

## 0. Current status

| Item | State |
|---|---|
| Phase | **Phase 7 (privileged + softmmu) complete.** Sv39 walker + inline software TLB; all 244 riscv-tests (p, v, mi, si) under every engine; TLB hit 4.1 ns vs Sv39 miss 28 ns; CoreMark under `--mem=softmmu` 54% of direct (2.7 G guest insns/s) (`docs/BENCHMARKS.md`). Next up: Phase 8, task P8.1 (`docs/ROADMAP.md`). |
| Language | Rust (decided, see §3) |
| Detailed plan | [`docs/ROADMAP.md`](docs/ROADMAP.md), with task IDs, tests and acceptance criteria per phase |
| Phase reports | [`docs/phase-reports/`](docs/phase-reports/), latest: `phase-07-privileged-softmmu.md` |
| Project explainer | [`docs/PROJECT_EXPLAINED.md`](docs/PROJECT_EXPLAINED.md) |
| Blockers | None. GitHub push access was fixed on 2026-09-26 (Claude GitHub App installed). |
| Last updated | 2026-09-26 |

Update this table at the end of every phase.

---

## 1. Goals

1. **Correct.** Execute unmodified RV64GC Linux ELF binaries (user mode). Stretch: boot an unmodified RISC-V Linux kernel (v6.x) to an interactive BusyBox shell (system mode).
2. **Fast.** Reach 100M+ emulated guest instructions/sec. Measure the JIT against our own interpreter, against `qemu-riscv64`, and against native.
3. **Demonstrate four hard subsystems** (the core of the project):
   1. **Runtime machine-code generation.** A hand-written x86-64 byte emitter writes into `mmap`'d memory under a W^X discipline. The buffer is then cast to a function pointer and called.
   2. **Direct block chaining.** Block exits are hot-patched with `E9 rel32` / `0F 8x rel32` so they jump straight to the translated successor.
   3. **Register remapping plus a spill allocator.** Hot guest registers are pinned to R12–R15. The rest live in `CpuState` behind RBP. Each block gets linear-scan allocation, with spills and fills kept out of hot paths.
   4. **Software MMU.** Emulates SV39 (and SV48 as a stretch), with an inline, direct-mapped software TLB. The hit path is about 5 x86 instructions. The slow path calls the Rust page walker.
4. **Milestone demo** (§24):
   - **A (required).** Run CoreMark and Dhrystone compiled for rv64gc. Print the JIT speedup over the pure interpreter.
   - **B (stretch).** Boot Linux to a BusyBox `/ #` prompt.
5. **Interview-ready.** The design must be explainable on a whiteboard (§28): translation-cache layout, self-modifying code (SMC) handling, raw x86 branch encoding.

## 2. Non-goals (for now)

- Vector (V), hypervisor (H), bit-manipulation (Zb*) and crypto extensions. They can be added later as fallback helpers.
- Hosts other than x86-64, and OSes other than Linux. Keep the `CodeMemory` abstraction so a Windows port can use `VirtualAlloc`/`VirtualProtect` later. Note: the Windows x64 ABI differs. RDI, RSI and XMM6–15 are callee-saved there, and every call needs 32 bytes of shadow space.
- Using an external assembler or JIT library at runtime (AsmJit, dynasm, Cranelift, LLVM, the iced-x86 encoder). The emitter is hand-written; that is the point of the project.
- Graphics, networking devices and SMP guests (SMP is a stretch goal).
- Bit-exact cycle timing.

---

## 3. Key decisions (decision log; append new entries, don't silently change)

| # | Topic | Decision | Rationale |
|---|---|---|---|
| D1 | Language | **Rust**, stable toolchain (1.94+), edition 2024 | The decoder, MMU, ELF loader and syscall layer are memory-safe. `unsafe` is confined to the JIT, `mmap` and signal code. `cargo test`/`bench` built in. `core::mem::offset_of!` for ABI asserts. |
| D2 | Host | Linux x86-64 only, System V AMD64 ABI | Matches the dev container. `mmap`, `memfd_create` and `sigaction` are available. |
| D3 | Emitter | Hand-written byte-level x86-64 assembler (`backend/x86/emit.rs`) | Core resume claim. Verified in tests by disassembling with `iced-x86` (dev-dependency only). |
| D4 | IR | Per-block linear IR with single-assignment virtual values (SSA without phis, because a block is single-entry straight-line code) | Simple, linear-time passes. Enables constant folding, dead-writeback elimination and liveness. |
| D5 | Register allocation | 4 statically pinned guest registers (R12–R15), plus per-block linear scan (Poletto & Sarkar) over a caller-saved pool | Taken from the brief. Pinned registers survive across chained blocks at zero cost. |
| D6 | W^X | **Dual mapping** by default: a `memfd_create` region is mapped twice, once RW (emit/patch) and once RX (execute). Fallback `--wx=mprotect`: RW → emit → `mprotect(RX)`. | No `mprotect` syscall or TLB shootdown per block. Patching chains stays cheap. Never map RWX. |
| D7 | Chaining | Patch rel32 of `jmp`/`jcc` exit slots in place. Keep the rel32 field 4-byte aligned. Keep incoming-link lists so blocks can be unlinked. | Taken from the brief. Aligned 32-bit stores are atomic on x86, which keeps the multithreaded stretch goal possible. |
| D8 | Indirect branches (JALR) | Inline jump-cache lookup: hash the guest PC into a 4096-entry `{guest_pc, host_ptr}` table and `jmp [entry+8]`. On a miss, exit to the dispatcher. | Keeps returns and indirect calls out of the dispatcher. |
| D9 | Guest memory | Two backends: **`direct`** (user mode, host = base + guest address, zero-cost) and **`softmmu`** (always used in system mode; optional in user mode). | Benchmarking direct against softmmu is itself an interview talking point. |
| D10 | TLB | Direct-mapped, 256 entries (configurable, power of 2) per MMU index, 32-byte entries `{addr_read, addr_write, addr_code, addend}` | QEMU-style. Hit path is about 5 instructions (§14). |
| D11 | SMC | Write-protect guest pages that hold translated code (a TLB flag in softmmu; `mprotect` + SIGSEGV in direct mode). On a write, invalidate that page's TBs eagerly. FENCE.I also flushes the jump cache and ends the block. Debug fallback: `--smc=flush-on-fence`. | Taken from the brief. Correct for x86-style and for RISC-V-style (FENCE.I) code patching. |
| D12 | Timeslice and interrupts | Every block prologue runs `sub qword [rbp+BUDGET], n_insns ; jl exit`. The dispatcher sets the budget. | Chained loops can't run forever. Doubles as the instruction counter. Asynchronous events are delayed by at most one slice. |
| D13 | Floating point | Phase F1: every FP instruction calls a helper backed by **Berkeley SoftFloat 3e with the RISC-V specialization** (vendored C, built with the `cc` crate). This is what Spike uses, so it is bit-exact. Phase F2: inline SSE2 fast path for RNE add/sub/mul/div/sqrt/fma, with NaN canonicalization and fflags taken from MXCSR. | Correctness first. Alternative if C vendoring hurts: `rustc_apfloat` (all 5 rounding modes plus status flags); verify it against rv64uf/ud first. |
| D14 | Unsupported instructions | The JIT emits a full-sync `call helper_interp_one(cpu, insn, pc)`, so every instruction the interpreter supports also runs under the JIT. | Correct first, then fast, one instruction at a time. |
| D15 | System boot | First a **built-in SBI** implemented in Rust. The guest starts in S-mode at the kernel entry, and S-mode `ecall` is serviced natively. Later, run real **OpenSBI** (`fw_jump.bin`) in M-mode to validate. | Fastest bring-up. OpenSBI then exercises M-mode. |
| D16 | Machine model | Memory map and devicetree compatible with QEMU `virt` | Stock kernel `defconfig` works unchanged. |
| D17 | Guest software | Linux **6.6 LTS**, BusyBox **1.36.x** static, initramfs embedded in the kernel | Well-trodden on QEMU virt. |
| D18 | Dependencies | Runtime: `libc`, `rustc-hash`, `clap` (derive), `anyhow`, `cc` (build-time, SoftFloat). Dev: `iced-x86` (decoder + fmt features), `proptest`. Anything else must be justified in this table. | Minimal attack surface and fast builds. |
| D19 | Process | The detailed plan lives in `docs/ROADMAP.md`, and §24 here is its summary. **Every phase ends with a detailed report** at `docs/phase-reports/phase-NN-<name>.md`, based on `TEMPLATE.md`. A phase isn't done without its report. | Owner requirement: a written account of what was done, how it works and how it was verified, after every phase. |
| D20 | Reference validation | Guest programs are checked against `qemu-riscv64` 8.2.2 with byte-exact stdout and exit codes (`tests/data/expected/`). riscv-tests are validated under `qemu-system-riscv64 -M spike` (HTIF). Tests QEMU itself gets wrong are listed in `tests/data/qemu-known-failures.txt` (XFAIL, and an XPASS is an error). | Every input is proven valid before bridgev runs it, so a future failure is bridgev's bug, not a bad test binary. |
| D21 | riscv-tests scope and build | Only the RV64GC + privileged suites are built (rv64ui/um/ua/uf/ud/uc `-p`/`-v`, rv64si/mi `-p`: 244 ELFs). Ubuntu-toolchain flags: `-no-pie -fno-pic -Wl,--build-id=none`. rv64ua is built directly with `-march=rv64g`, excluding `amocas_*` (Zacas). | Upstream compiles rv64ua with `zacas_zabha`, which binutils 2.42 rejects. PIC turns `la` into GOT loads, and the build-id note displaced `_start` from 0x80000000. |
| D22 | Decoder oracle | Golden decoder vectors come from `llvm-mc -M no-aliases` (`tools/gen-decoder-vectors.py`, committed as `tests/data/rv64_vectors.txt`). The disassembler matches LLVM's canonical text exactly, and LLVM's own compressed encodings cross-check the RVC expansion. | An independent oracle for decode *and* disassembly. The tests don't need LLVM at run time. |
| D23 | Guest permissions (Phase 1) | Host pages of the direct backend stay RW. Guest R/W/X live in a two-level per-page table checked on every interpreter access. Host `mprotect` mirroring the guest arrives with the JIT's unchecked `[rbx+g]` accesses (Phase 2) and SMC write-protection (Phase 8). | The interpreter is memory-safe against any guest, and the design stays simple until the JIT needs host-level protection. |
| D24 | Sv39 gating | `satp` accepts only Bare mode until Phase 7 (`SV39_SUPPORTED` in `cpu/csr.rs`). *Superseded by D48 (Phase 7): Sv39 is accepted.* | A guest can't enable translation that isn't implemented yet and have it silently ignored. |
| D25 | JIT exit protocol (Phase 2) | Exit code `rax = (tb_id << 2) \| slot` (slot 0/1 = direct exits, 2 = special). The reason lives in `cpu.exit_reason`: 0 NONE, 1 ECALL, 2 EXCEPTION (`exc_cause`/`exc_tval`), 3 FLUSH (FENCE.I), 4 HOST_FAULT (`fault_rip`/`fault_addr`, set by the SIGSEGV handler); later 5 BUDGET, 6 LOOKUP (D30), 7 FP_VARIANT (D47), 8 MMU_FAULT (D48). The dispatcher zeroes `exit_reason` before every entry. Supersedes the `tb_ptr \| slot` wording of §8.4. | An index is stable and bounds-checkable, and it can never dangle after a flush, unlike a heap pointer. |
| D26 | x86 disassembly in the binary | `iced-x86` (decoder + Intel formatter) is also an **optional** runtime dependency behind the `disasm` cargo feature (for `--dump-x86` text and lockstep reports). The default build prints hex plus the matching `objdump` command. It amends D18. | Readable divergence reports on demand, without changing the default dependency set. |
| D27 | Host protection mirrors guest permissions (Phase 2) | `DirectMem` maps each guest page with host `PROT_READ` if the guest may read, write or execute, plus `PROT_WRITE` if it may write. Loader writes (`write_bytes`) lift protection temporarily. JIT loads and stores are unchecked `[rbx + addr]`. A host SIGSEGV whose RIP is in the code buffer is redirected to `fault_exit`, and the dispatcher maps RIP → TB → `pcmap` → guest pc and recomputes `tval` from `x[rs1] + imm` (or the first faulting byte of a split access). This fulfils D23's deferred step. Execute-only guest pages stay host-readable, so JIT loads from them do not fault (the same as qemu-user). | Zero-cost checks on the hot path, precise guest exceptions, and the same fault reports as the interpreter (proven by `tests/cli.rs::jit_host_fault_is_precise`). |
| D28 | Phase 2 TB key and helpers | TBs are keyed by guest pc only. Lowering doesn't depend on privilege or FS yet, because CSR, AMO, FP, privileged and illegal instructions all run through `helper_interp_one` (D14), which checks them at run time. `TbFlags` arrive with Phase 7. `cpu.icount` is updated at exits and before helper calls. The `pcmap` records the not-yet-added count per instruction for faults. | Keeps Phase 2 minimal but correct. |
| D29 | Lockstep determinism | `--engine=lockstep` forces `csr.deterministic_time` (`time` = icount / 10), so the interpreter and the JIT read the same `time` CSR. `--deterministic` enables the same for other engines. | Otherwise every `rdtime` would be a false divergence. |
| D30 | Instruction counting via the budget (Phase 3) | JIT code never touches `icount`. The TB prologue `sub qword [budget], n; jl budget_stub` charges all `n` instructions up front, and `icount += budget_ref - budget` after every return to the dispatcher. Paths that retire fewer give the rest back: ECALL refunds 1, a host fault refunds `n - idx` (dispatcher), and a helper call at index `k` refunds `n - k` before the call and re-charges it after a normal return. `helper_interp_one` first folds `budget_ref - budget` into `icount` (and resets `budget_ref`), so CSR counters stay exact. New exit reasons: 5 BUDGET, 6 LOOKUP (jump-cache miss). The dispatcher's budget is `max(min(slice, limit - icount), n_first_tb)`, so the first TB always runs whole. `slice` defaults to 100 000. | One RMW per TB instead of an `icount` update on every exit path. Chained code stays exact and preemptible (tested with slice = 1). |
| D31 | Jump cache ownership | The jump cache is `CpuState.jmp_cache[4096]` at 0x300 (`{pc, host}`; `pc = u64::MAX` means empty; slot `(pc >> 1) & 4095`, so byte offset `(pc << 3) & 0xFFF0`). The dispatcher fills the entry for every TB it enters. `cpu.jc_tag = jit_id << 48 \| version`, where `version` is bumped on every flush or invalidation; on a mismatch the dispatcher clears the whole cache before entering JIT code. | The `Engine::flush` path has no `CpuState` at hand. Lazy, tag-based clearing can never leave a host pointer into flushed code, even with several `Jit`s per process (tests). |
| D32 | Lockstep with chaining | Lockstep enters each TB with `budget = n` (its length). A linked exit or jump-cache hit reaches the successor's prologue, which exits with BUDGET before executing anything. Snapshots copy only architectural state (`ArchState`), not the 64 KiB jump cache. | Linked exits and jump-cache hits are exercised, yet every comparison still covers exactly one TB. |
| D33 | `--no-chain` / `--profile-jit` | `--no-chain` disables both linking and jump-cache fills, so every TB returns to the dispatcher (the Phase 2 behaviour with the Phase 3 code layout). `--profile-jit` makes JALRs increment `cpu.prof_jalr` to report the jump-cache hit rate. It changes the generated code, so timings are taken without it. | Clean A/B measurements of chaining, and a hit-rate metric that costs nothing when off. |
| D34 | IR shape (Phase 4) | Per-TB SSA list of `Op`s (`ir/ops.rs`): `Insn{pc,idx}` markers, `Const`, `ReadReg`/`WriteReg`, `Bin`/`BinImm` (W ops explicit, sign extension folded in), `Load{size,signed}`/`Store`, `Interp{raw}` and the terminators `Branch`/`Jump`/`JumpInd`/`Exit`. There are no `CallHelper`/`Ext`/`Amo`/`Fence` ops: every instruction not lowered natively (CSR, AMO, FP, system) is `Interp`, the D14 helper. `ir::eval` executes IR directly and is the test oracle for the lifter and every pass. Amends §9. | The smallest IR that covers what the backend lowers natively; new native ops (FP in Phase 6, AMOs) get IR ops when they get lowering. |
| D35 | Optimizer passes | `forward` (register forwarding + read CSE, reset at `Interp`), `dead_writes` (a `WriteReg` overwritten later with no `Load`/`Store`/`Interp` in between), `fold` (constants, immediate forms, identities, constant branches, constant `JumpInd` → `Jump`), `dce`. Dead write-backs across fault sites are left to the allocator's lazy write-back (D36). | Each pass is one linear sweep; the eval oracle proves each preserves semantics (`tests/ir_passes.rs`). |
| D36 | Allocator (Phase 4) | One forward walk interleaved with emission (linear scan over a straight-line block). Values carry a backing (`Home(g)`, spill `Slot(k)`, `Const`) so a clean value is dropped for free. Non-pinned guest registers are written back lazily (dirty until an exit, helper call or eviction). Eviction takes the furthest next use. The current op's operands count as live while it is lowered. R10/R11 are never held across an allocator call (the allocator uses R11 for copies). A helper call is a full sync: write back, store pinned registers, move live values to slots, reload pinned registers after. | Linear in block size, no separate allocation pass, and a block overwriting a register many times stores it once. |
| D37 | Precise faults without cold stubs (direct mode) | JIT loads/stores stay single `[rbx+r+disp]` instructions. Each records a *fault site* (host offset, guest idx/pc, address register, offset, size, and where every dirty guest register lives: pool register, slot, constant or another home). The SIGSEGV handler copies the host GPRs to `cpu.fault_regs`; the dispatcher (`resolve_site`) writes the dirty values home, sets pc, refunds the budget and computes `tval`. The hot path carries no state-map code at all. Softmmu slow paths (Phase 7) will be real cold stubs. Amends §10/§15 for direct mode. | Zero hot-path cost and exact state; the fuzzer's faulting terminators check it. |
| D38 | Spill slots and oversize blocks | `CpuState.spill[64]` at 0x10300. If a TB needs more slots, `translate` returns `OutOfSlots` and the dispatcher retranslates it with half the instructions. | A fixed, disp32-addressable area; running out is rare and handled without failing. |
| D39 | `--regalloc` levels, `--pin` | `none` = the Phase 3 lowering (`lower.rs`, no pins). `pinned` = IR without optimizer passes and with eager stores (every `WriteReg` stores home at once), pinned registers in R12–R15. `linear` (default) = passes + lazy write-back. `--pin` takes up to 4 registers mapped to R12, R13, R14, R15 in order; `--pin ""` pins nothing. | The levels isolate what pinning and what caching/optimization each contribute (§22). |
| D40 | `--stats=regs` | Interpreter engine only (a property of the guest program): counts integer register references (reads + writes, x0 excluded) once per decoded block (static) and once per retired instruction (dynamic), and reports the dynamic share covered by the default and by the best 4-register set. Other engines reject it. | No extra code in JIT output; exact dynamic counts. |
| D41 | `--check-abi` dropped | Lockstep (after every TB, including chained entries per D32) and the block fuzzer compare full architectural state, which includes the pinned registers `exit_jit` writes back. A wrong pinned value is reported as a register divergence at the first TB that exposes it. | The shadow-copy check would test a subset of what lockstep already tests. |
| D42 | Pinned set stays x2, x1, x10, x15 (P4.9) | `--stats=regs` on CoreMark ranks x15, x14, x13, x10 highest (72.1% of dynamic register uses vs 39.9% for the default set), but measured CoreMark speed with `--pin x15,x14,x13,x10` was only +2.6% (median of 5, ranges overlapping), fib was −6% (noisy), and x2,x15,x10,x14 was −1.7%. Use counts are a poor proxy: pinning pays for values live *across* blocks (sp, ra for returns through the jump cache), which the linear allocator cannot cache. Default unchanged; revisit with Dhrystone and the Phase 5 harness. | Change the default only on a clear, reproducible win. |
| D43 | Benchmark builds and harness (Phase 5) | CoreMark (EEMBC, submodule) is built with its own Makefile (`PORT_DIR=linux`, `-O2`, static, `PERFORMANCE_RUN=1`; iterations at run time). Dhrystone is riscv-tests' `benchmarks/dhrystone` sources, **unmodified**, plus a Linux shim (`guest/bench/dhrystone`: run count from argv, `clock_gettime` timer, working `debug_printf` for Weicker's self-check); netlib is unreachable from the container. Native x86-64 builds use the same sources and flags (host gcc 13.3 = cross gcc 13.3). The harness is `tools/bench.py` (Python stdlib), not a `bridgev bench` subcommand: per-cell calibration to the target run time, `taskset`, warm-up + 5 runs, validation of every run (CoreMark's own CRC checks and 10 s rule, Dhrystone's final-value self-check), JSON + markdown. | Process control, statistics and JSON are simpler in Python; keeping bridgev free of benchmark logic. |
| D44 | `--profile-tbs` | SIGPROF on process CPU time (1 kHz requested; the kernel delivers at its tick, about 250 Hz here) records the host RIP in a lock-free buffer (2^17 samples); `--stats` attributes samples to TBs (`find_host`), trampolines, or "elsewhere" (Rust + kernel) and lists the 12 hottest TBs. | Answers "where does time go" without `perf` (not installed in the container) and without changing the generated code. |
| D45 | Loop-resident registers: tried, **not adopted** | Self-loop TBs kept their most-used guest registers in pool registers across iterations (loaded once, stored on exit paths, listed in fault-site state maps). It doubled `loop.elf` (3,170 → 6,225 MIPS) but moved neither CoreMark (−0.4%) nor Dhrystone (−1.4%) in same-batch A/B runs, so it was reverted (Phase 5 report §7.3). The fuzzer's self-loop blocks and multi-iteration mode stayed. | Only changes that win on the benchmarks stay (P5.3). |
| D46 | Budget in R9 (IR back end) | The `pinned`/`linear` back end keeps the budget in R9 instead of `CpuState.budget`: prologue `sub r9, n; jl`, stubs `add r9, refund`; `enter_jit`/`exit_jit` load/store it (so every exit, fault and the dispatcher see `CpuState.budget` as before), helper calls store it before and reload it after. R9 leaves the pool (6 registers). The naive back end keeps the memory budget. Amends D30/§13.3 for the IR levels. | The memory RMW formed a store-to-load dependency chain through every TB; same-batch A/B: CoreMark +10.5%, Dhrystone +22.4%. |
| D47 | Inline FP (Phase 6) | IR back end only. f registers stay in `CpuState`; each inline op uses XMM0–2 as scratch. F/D loads, stores, `fmv` and sign injection are integer IR (`ReadF`/`WriteF`/`Unbox`). add/sub/mul/div/sqrt, fcvt.s.d/d.s, FMA (FMA3 only), feq/flt/fle, fcvt.{w,l}.{s,d} and fcvt.{s,d}.{w,wu,l} are inline for rm = RNE or DYN (RTZ too for float→int). fmin/fmax, fclass, the other unsigned conversions and other static rounding modes use `helper_interp_one`. Tail fix-ups: NaN canonicalization, unboxed single → canonical NaN, float→int saturation, NV for FMA (0 × ∞) + qNaN. fflags: `enter_jit` loads MXCSR 0x1F80; `exit_jit` and `helper_interp_one` fold IE/ZE/OE/UE/PE into NV/DZ/OF/UF/NX (the helper also resets MXCSR before and after). TBs come in two variants keyed `(pc, fp_slow)`: the fast one requires FS = Dirty and, if it has dynamic-rm ops, frm = RNE, re-checked in the prologue (exit reason 7 `FP_VARIANT`, nothing executed); the slow one is the helper-only code. `Block::fp_guard/fp_dyn` carry the guard so DCE cannot drop it. `--no-inline-fp` forces the slow variant. | Bit-exact against SoftFloat (1e6-case fuzzer, fflags included) at 60× the helper path on fpbench. XMM allocation across ops is left for later. |
| D48 | Softmmu (Phase 7) | `cpu.softmmu` (0x116) selects translated memory: always in bare/system mode, `--mem=softmmu` in user mode (where translation is the identity and `DirectMem`'s guest permissions act as physical permissions; the TLB is flushed after brk/mmap/munmap/mremap/mprotect). Physical memory = RAM mapped in `DirectMem` at its physical address + `DirectMem::devices` (MMIO); anything else is an access fault. **Four MMU indices** (U, S, S+SUM, M/Bare) so Linux's SUM toggling and MPRV need no flush; MXR changes, satp writes and SFENCE.VMA flush all (`tlb::flush_all`, which bumps `cpu.mmu_gen`). TLB: 256 direct-mapped 32-byte entries per index at 0x10580, tags = vpage \| flags (MMIO bit 3, CODE bit 4), `addr_write` only after D is set; the walker sets A/D itself (Svadu-like, §14.3). Interpreter: `mem::mmu::{load,store,fetch16}`; decoded blocks keyed (pc, fetch index), dropped on `mmu_gen` change. JIT: TBs keyed `TbKey{pc, fp_slow, flags = 0x80 \| fetch_idx \| data_idx << 2, physical page}`; IR loads/stores probe the TLB inline (9 instructions to the access, `tlb_probe`) with a cold slow path per site that saves the caller-saved registers to `fault_regs`, refunds the unretired budget, calls `helper_mmu_access(cpu, va, info, val)`, restores, and either continues or exits with reason 8 `MMU_FAULT` (the stub also saves R12–R15 and `fault_rip` = the site, so the D37 state map makes the fault precise). Chaining only between TBs with equal flags on the same virtual page, never out of a TB ending in a CSR instruction; the jump cache is reset when (flags, `mmu_gen`) changes. A 32-bit instruction straddling a page is interpreted (never translated). The naive back end sends loads/stores through `helper_interp_one`. `satp` accepts Sv39 (supersedes D24). | QEMU-style softmmu with zero cost for direct mode; one mechanism serves user-mode softmmu, bare-metal and (Phase 9) Linux. Precise faults reuse D37's state maps. |

---

## 4. Development environment (verified 2026-09-26, cloud container)

- Host CPU: Intel Xeon @ 2.80 GHz, 4 vCPU, 15 GiB RAM. Containers vary (the Phase 2 measurements ran on a Xeon @ 2.10 GHz), so always record `model name` from `/proc/cpuinfo` with every result. Flags present: `sse4_2 avx2 avx512f bmi1 bmi2 fma popcnt movbe adx erms`.
  - The JIT may use BMI2 (`SHLX/SHRX/SARX`, `MULX`), FMA3 and POPCNT, but **must check features at runtime with `cpuid`** and fall back gracefully.
- Tools already installed: `rustc`/`cargo` 1.94.1, `gcc`/`g++` 13.3, `clang` 18 (**supports `--target=riscv64`**), `ld.lld`, `llvm-mc`, `llvm-objdump`, `gdb`, `make`, `cmake`, `python3`.
- Installed by `tools/setup.sh` via apt (it must be re-run in each new container):
  - `gcc-riscv64-linux-gnu` 13.3.0 (binutils 2.42, cross glibc 2.39, lp64d only)
  - `qemu-user` (`qemu-riscv64` 8.2.2) and `qemu-system-misc` (`qemu-system-riscv64` 8.2.2)
  - `device-tree-compiler` 1.7.0
  - kernel build deps: `flex bison bc libssl-dev libelf-dev cpio autoconf automake`
  - Third-party PPAs in the container are blocked by the network policy (403); the Ubuntu archive works.
- `vm.overcommit_memory=0` and `ulimit -v unlimited`, so a 256 GiB `PROT_NONE` + `MAP_NORESERVE` reservation for the direct-mode guest space is fine.
- `perf` is not installed. Use in-process timers and `rdtsc`, and emit `/tmp/perf-<pid>.map` anyway for machines that have perf.
- Handy commands:
  - `llvm-mc -triple=riscv64 -mattr=+m,+a,+f,+d,+c -show-encoding`: golden RISC-V encodings for decoder tests.
  - `llvm-mc -triple=x86_64 -show-encoding` and `llvm-mc -disassemble -triple=x86_64`: cross-check x86 bytes.
  - `objdump -D -b binary -m i386:x86-64 --adjust-vma=0x...`: dump JIT output.

---

## 5. Architecture overview

```
Guest RISC-V Binary (ELF64, EM_RISCV=243)   or   Kernel Image + DTB (system mode)
            │
            ▼
┌──────────────────────────┐
│   Instruction Decoder    │ ──► 16/32-bit fetch; R/I/S/B/U/J/R4 formats; RVC expanded to 32-bit form
└──────────────────────────┘
            │  Vec<Inst> for one basic block
            ▼
┌──────────────────────────┐
│ Intermediate Rep (IR)    │ ──► linear single-assignment values; const-fold (LUI/AUIPC/ADDI);
│                          │     guest-reg forwarding; dead write-back elim; liveness intervals
└──────────────────────────┘
            │
            ▼
┌──────────────────────────┐
│ Register Allocator       │ ──► pinned R12–R15 + linear scan over caller-saved pool; spills to CpuState
└──────────────────────────┘
            │
            ▼
┌──────────────────────────┐
│   x86_64 JIT Emitter     │ ──► raw bytes (REX/ModRM/SIB) into RW view of code buffer;
│                          │     hot path linear, slow paths in cold stubs at block tail
└──────────────────────────┘
            │
            ▼
┌──────────────────────────┐
│ Translation Cache & Link │ ──► publish via RX view (dual map) or mprotect(RX);
│                          │     patch exit rel32 → successor (block chaining); jump cache for JALR
└──────────────────────────┘
            │
            ▼
  Direct CPU execution: enter_jit(cpu, host_code) → chained blocks → exit code in RAX → dispatcher
```

**Execution engines**, selected by `--engine`:
- `interp`: the pre-decoded interpreter. It is the golden reference model, the baseline for speedup numbers, and the fallback helper (D14).
- `jit`: the translator.
- `lockstep`: runs the JIT and the interpreter side by side and compares full architectural state at every block boundary (§21).

**Execution modes**, selected by `--mode`:
- `user`: syscall emulation, like `qemu-riscv64`. Loads a static RV64 ELF and translates Linux RISC-V syscalls to host syscalls.
- `system`: full system, like `qemu-system-riscv64 -M virt`. Supports M/S/U privilege, SV39, traps, CLINT/PLIC/UART, SBI and devicetree.

**Main loop (dispatcher, Rust):**
```
loop {
    deliver pending interrupts (system) / signals (user)
    tb = jump_cache.get(pc, flags) ?? tb_map.get(pc, flags) ?? translate(pc, flags)
    if chaining && last_exit.is_chainable() && may_link(last_tb, tb) { patch(last_tb.slot → tb.host) }
    cpu.budget = min(SLICE, insns_until_next_timer_event)
    ret = enter_jit(cpu, tb.host)              // extern "sysv64" fn(*mut CpuState, *const u8) -> u64
    last_exit = decode(ret)                    // (tb_ptr | slot) or special exit (cpu.exit_reason)
    handle exit_reason: ECALL/syscall, exception → trap entry, budget expired, halt, SMC flush, ...
}
```

---

## 6. Repository layout (planned; create in Phase 0)

```
Bridge-V/
├── CLAUDE.md                 ← this file
├── Cargo.toml                ← single crate: lib + bin `bridgev`
├── build.rs                  ← compiles vendored SoftFloat (Phase F1)
├── src/
│   ├── main.rs               ← CLI (clap)
│   ├── lib.rs
│   ├── elf.rs                ← ELF64 loader (hand-written): PT_LOAD, entry, symtab (tohost), e_flags
│   ├── isa/                  ← decoded instruction model
│   │   ├── inst.rs           ← enum Inst { Add{rd,rs1,rs2}, Addi{..}, ... }
│   │   ├── decode.rs         ← 32-bit decoder (RV64IMAFD + Zicsr + Zifencei)
│   │   ├── rvc.rs            ← compressed → 32-bit expansion
│   │   └── disasm.rs         ← objdump-style text (debug, tests)
│   ├── cpu/
│   │   ├── state.rs          ← #[repr(C)] CpuState + offset_of! asserts
│   │   ├── csr.rs            ← CSR file, read/write side effects, counters
│   │   ├── trap.rs           ← exception/interrupt entry, MRET/SRET, delegation
│   │   ├── fp.rs             ← FP helpers (SoftFloat wrapper), NaN-boxing, fflags
│   │   └── softfloat_shim.c  ← accessors for SoftFloat's thread-local state
│   ├── interp/               ← reference interpreter (pre-decoded block cache)
│   ├── ir/
│   │   ├── ops.rs            ← IR definitions + printer (--dump-ir)
│   │   ├── lift.rs           ← Inst → IR
│   │   ├── eval.rs           ← IR evaluator (the oracle for lifter/optimizer tests)
│   │   ├── opt.rs            ← const fold, forwarding, dead write-back elimination
│   │   └── liveness.rs       ← use positions → intervals, fixed-register constraints
│   ├── regalloc/linear_scan.rs ← per-TB allocator: pinned regs, lazy write-back, spills (D36)
│   ├── backend/x86/
│   │   ├── emit.rs           ← byte emitter: REX/ModRM/SIB/imm, labels, fixups
│   │   ├── regs.rs           ← host register enum, pools, pinned map
│   │   ├── lower.rs          ← Phase 2/3: Inst → x86 1:1, all guest regs in CpuState (`--regalloc=none`)
│   │   ├── lower_ir.rs       ← Phase 4: IR → x86 with the allocator, exit/budget stubs, fault sites
│   │   ├── features.rs       ← cpuid (BMI2, FMA, ...)
│   │   └── disasm.rs         ← host-code dump (iced-x86 with `--features disasm`, else hex)
│   ├── jit/
│   │   ├── code_mem.rs       ← dual-mapped / mprotect code buffer (unsafe)
│   │   ├── trampoline.rs     ← enter_jit / exit_jit / helper thunks (generated at startup)
│   │   ├── cache.rs          ← TranslationBlock, tb_map, page→TB index, flush
│   │   ├── chain.rs          ← link / unlink_incoming (aligned rel32 patches), incoming lists
│   │   ├── dispatch.rs       ← main loop, JitOptions, host-fault resolution
│   │   ├── lockstep.rs       ← --engine=lockstep (interp vs JIT per TB)
│   │   └── perfmap.rs        ← /tmp/perf-<pid>.map
│   ├── mem/
│   │   ├── direct.rs         ← user-mode host-mapped guest space (unsafe)
│   │   ├── phys.rs           ← RAM + MMIO bus (system)
│   │   ├── mmu.rs            ← SV39/SV48 page walker, A/D update, permissions
│   │   ├── tlb.rs            ← software TLB fill/flush, flags
│   │   └── smc.rs            ← code-page tracking, write-protect, invalidation
│   ├── user/                 ← Linux user-mode emulation
│   │   ├── loader.rs         ← stack: argc/argv/envp/auxv
│   │   ├── syscall.rs        ← dispatch table + struct translation
│   │   └── signal.rs         ← host SIGSEGV handler, guest signals (stretch)
│   ├── system/
│   │   ├── bare.rs           ← bare-metal HTIF harness for riscv-tests (`run --mode bare`)
│   │   ├── machine.rs        ← `virt` memory map, reset, boot ROM
│   │   ├── clint.rs  plic.rs  uart16550.rs  syscon.rs  virtio_mmio.rs(stretch)
│   │   ├── sbi.rs            ← built-in SBI (D15)
│   │   └── fdt.rs            ← devicetree blob generator
│   └── stats.rs              ← `--stats=regs` register-use histogram (RegStats)
├── tests/                    ← integration: riscv-tests (all engines), user programs (all engines), emitter_golden,
│                               jit_lowering, cli, decoder_vectors, elf, ir_passes, fuzz_blocks
│   ├── common/mod.rs         ← guest_elf() / run_bridgev() helpers
│   ├── common/rvgen.rs       ← random RV64 block generator + single-block harness (proptest)
│   └── data/                 ← expected/ (qemu reference outputs), qemu-known-failures.txt
├── guest/                    ← RISC-V guest sources + build scripts (outputs .gitignored)
│   ├── asm/  c/              ← hand tests, C tests
│   ├── bench/dhrystone/      ← Linux shim (util.h, shim.c) for riscv-tests' Dhrystone (P5.1)
│   └── linux/                ← kernel .config fragment, busybox .config, initramfs skeleton, build.sh
├── third_party/              ← git submodules: riscv-tests, coremark, berkeley-softfloat-3
├── tools/                    ← setup.sh, build-guests.sh, build-riscv-tests.sh, ref-run.sh, ref-check.sh,
│                               ref-riscv-tests.sh (Phase 0); gen-decoder-vectors.py (Phase 1);
│                               build-bench.sh, bench.py (the §22 harness), demo-milestone-a.sh (Phase 5);
│                               later: boot-linux.sh
├── README.md                 ← short landing page
├── docs/
│   ├── ROADMAP.md            ← detailed phase-by-phase plan (task IDs P<phase>.<n>)
│   ├── PROJECT_EXPLAINED.md  ← what the project is / does / is used for (plain language)
│   ├── BENCHMARKS.md         ← measured results only
│   └── phase-reports/        ← TEMPLATE.md + one report per completed phase (mandatory, D19)
└── .github/workflows/ci.yml  ← fmt, clippy, test, riscv-tests
```

---

## 7. Guest ISA reference (RV64GC = RV64IMAFD + Zicsr + Zifencei + C)

### 7.1 Instruction length
- Low 2 bits `!= 0b11`: 16-bit compressed.
- Low 2 bits `== 0b11` and `bits[4:2] != 0b111`: 32-bit.
- Anything longer (48/64-bit) raises illegal-instruction.
- The all-zero 16-bit word is defined illegal.
- Fetch 16 bits at a time. A 32-bit instruction may straddle a page boundary (§13.2).

### 7.2 32-bit formats (bit positions)
```
R : funct7[31:25] rs2[24:20] rs1[19:15] funct3[14:12] rd[11:7] opcode[6:0]
R4: rs3[31:27] fmt[26:25] rs2 rs1 rm/funct3 rd opcode            (FMADD/FMSUB/FNMSUB/FNMADD)
I : imm[11:0]=inst[31:20]                          rs1 funct3 rd opcode
S : imm[11:5]=inst[31:25]  imm[4:0]=inst[11:7]     rs2 rs1 funct3 opcode
B : imm[12]=inst[31] imm[10:5]=inst[30:25] imm[4:1]=inst[11:8] imm[11]=inst[7]; imm[0]=0
U : imm[31:12]=inst[31:12]
J : imm[20]=inst[31] imm[10:1]=inst[30:21] imm[11]=inst[20] imm[19:12]=inst[19:12]; imm[0]=0
```
All immediates are sign-extended from their top bit to 64 bits.

### 7.3 Major opcodes (`inst[6:0]`)
| Hex | Name | Hex | Name | Hex | Name |
|---|---|---|---|---|---|
| 0x03 | LOAD | 0x23 | STORE | 0x43 | MADD |
| 0x07 | LOAD-FP | 0x27 | STORE-FP | 0x47 | MSUB |
| 0x0F | MISC-MEM (FENCE, FENCE.I) | 0x2F | AMO | 0x4B | NMSUB |
| 0x13 | OP-IMM | 0x33 | OP (incl. M: funct7=0x01) | 0x4F | NMADD |
| 0x17 | AUIPC | 0x37 | LUI | 0x53 | OP-FP |
| 0x1B | OP-IMM-32 | 0x3B | OP-32 | 0x63 | BRANCH |
| 0x67 | JALR | 0x6F | JAL | 0x73 | SYSTEM (ECALL/EBREAK/xRET/WFI/SFENCE.VMA/CSR*) |

### 7.4 Compressed (RVC, RV64 variant)
Quadrant = `bits[1:0]`. `rd'`/`rs1'`/`rs2'` are 3 bits and map to **x8–x15**.

- **Q0:** C.ADDI4SPN(000), C.FLD(001), C.LW(010), **C.LD(011)**, reserved(100), C.FSD(101), C.SW(110), **C.SD(111)**
- **Q1:** C.NOP/C.ADDI(000), **C.ADDIW(001)** (this slot is C.JAL on RV32), C.LI(010), C.ADDI16SP/C.LUI(011), MISC-ALU(100: C.SRLI, C.SRAI, C.ANDI, C.SUB, C.XOR, C.OR, C.AND, **C.SUBW, C.ADDW**), C.J(101), C.BEQZ(110), C.BNEZ(111)
- **Q2:** C.SLLI(000), C.FLDSP(001), C.LWSP(010), **C.LDSP(011)**, C.JR/C.MV/C.EBREAK/C.JALR/C.ADD(100), C.FSDSP(101), C.SWSP(110), **C.SDSP(111)**

Reserved encodings (e.g. C.ADDI4SPN with nzuimm=0, C.LUI with imm=0, C.ADDI16SP with imm=0) raise illegal-instruction.

Link addresses depend on the instruction length: `rd = pc + 2` for C.JALR, and `pc + 4` for 32-bit instructions. The decoder records `len` in every `Inst`.

### 7.5 Semantic gotchas (each one needs a test)
- `x0` is hardwired to zero. Writes are discarded. Loads with `rd=x0` still perform the access, and can fault or have MMIO side effects.
- **32-bit W ops** (ADDW, SUBW, SLLW, SRLW, SRAW, ADDIW, SLLIW, SRLIW, SRAIW, MULW, DIVW, DIVUW, REMW, REMUW) compute on the low 32 bits and **sign-extend** the result to 64 bits. x86 32-bit ops *zero*-extend, so each needs a `MOVSXD`.
- Shift amounts: RV64 uses `rs2[5:0]` and W forms use `rs2[4:0]`, which matches x86 masking (63/31). SLLI/SRLI/SRAI have a 6-bit shamt (`inst[25:20]`). SLLIW/SRLIW/SRAIW have a 5-bit shamt and `inst[25]` must be 0.
- **Division never traps:**

  | Case | Result |
  |---|---|
  | DIV x/0 | −1 (all ones) |
  | DIVU x/0 | 2⁶⁴−1 |
  | REM/REMU x/0 | x |
  | DIV MIN/−1 | MIN |
  | REM MIN/−1 | 0 |

  x86 `IDIV`/`DIV` raise #DE on both cases, so the JIT **must** emit guards before them.
- MULHSU (signed × unsigned) has no x86 equivalent. Use `mulhu(a,b) − (a<0 ? b : 0)`.
- LUI: `rd = sext(imm[31:12] << 12)`. AUIPC: `rd = pc + sext(imm << 12)`. The PC is a translate-time constant, so **AUIPC always constant-folds**.
- JALR: `target = (rs1 + imm) & ~1` and `rd = pc + len`. Read `rs1` *before* writing `rd`, because `rd == rs1` is legal and common.
- With C enabled, branch and jump targets are always 2-byte aligned, so instruction-address-misaligned exceptions cannot occur. Keep the check under a `misa.C=0` configuration anyway.
- SLT/SLTU produce 0 or 1. SLTIU compares against the *sign-extended* immediate, treated as unsigned.
- The LR/SC reservation is cleared by any SC. SC writes 0 on success and 1 on failure. SC may fail spuriously; implementations clear the reservation on traps.
- The CSR instructions `csrrw/rs/rc(i)` do not write the CSR when `rs1=x0` (or uimm=0) for RS/RC. CSRRW with `rd=x0` does not read, so there are no read side effects.
- The user-mode counters `cycle` (0xC00), `time` (0xC01) and `instret` (0xC02) are gated by `mcounteren`/`scounteren` in system mode.
- FENCE is a no-op in single-threaded operation (x86 TSO is stronger than RVWMO). The exception: when `pred` contains W and `succ` contains R, emit `MFENCE` in multithreaded mode. FENCE.I is the SMC barrier (§16).

---

## 8. CPU state, host registers and the block-boundary ABI

### 8.1 `CpuState` (`#[repr(C, align(64))]`)
The offsets are illustrative. **The source of truth is the `offset_of!` compile-time asserts in `cpu/state.rs`.** The JIT addresses fields relative to **RBP = &CpuState + 128**, so that all of `x[0..32]` fall inside the disp8 range (−128…+127). That gives the shortest encodings.

| Offset | Field | Notes |
|---|---|---|
| 0x000 | `x: [u64; 32]` | GPRs. The `x[0]` slot always holds 0. disp8 from RBP: `8*i − 128`. |
| 0x100 | `pc: u64` | Written only at block exits, in cold stubs, and before helpers. |
| 0x108 | `budget: i64` | Decremented in each block prologue (D12). |
| 0x110 | `exit_reason: u32` | |
| 0x114 | `prv: u8` (U=0, S=1, M=3), `mmu_idx: u8` @0x115, `softmmu: u8` @0x116 | `priv` is a Rust keyword; `softmmu` = translated memory (D48) |
| 0x118 | `icount: u64` | Retired guest instructions (stats, MIPS). |
| 0x120 | `mem_base: u64` | Direct mode: host address of guest address 0. |
| 0x128 | `res_addr: u64`, `res_val` @0x130, `res_valid` @0x138 | LR/SC reservation. |
| 0x140 | `f: [u64; 32]` | FP registers, NaN-boxed. |
| 0x240 | `fflags: u8`, `frm: u8` @0x241 | fcsr fields (verified by `offset_of!` asserts as of Phase 1) |
| 0x248 | `exc_cause`, `exc_tval` @0x250 | exception raised by JIT code or a helper (D25) |
| 0x258 | `fault_rip`, `fault_addr` @0x260 | written by the SIGSEGV handler (D27) |
| 0x268 | `helper_mem` | `*mut DirectMem` for JIT helpers (set by the dispatcher) |
| 0x270 | `budget_ref: i64` | budget at the last icount sync (D30) |
| 0x278 | `jc_tag`, `prof_jalr` @0x280 | jump-cache owner/version (D31); JALR counter (`--profile-jit`) |
| 0x300 | `jmp_cache: [{pc, host}; 4096]` | 64 KiB, the inline JALR lookup table (§13.4) |
| 0x10300 | `spill: [u64; 64]` | Spill slots for IR values (D38) |
| 0x10500 | `fault_regs: [u64; 16]` | host GPRs at a SIGSEGV in JIT code, in `Reg` order (Rust-only, D37) |
| 0x10580 | `tlb: [[TlbEntry; 256]; 4]` | 32 B/entry, 32 KiB. MMU indices: U=0, S=1, S+SUM=2, M/Bare=3 (D48). |
| … | CSR block (mstatus, mie, mip, mtvec, mepc, …) | Accessed only by Rust helpers. |

### 8.2 Host register assignment (System V AMD64)

| Host reg | Role | Saved by |
|---|---|---|
| RSP | host stack, 16-byte aligned at every `call` (entry trampoline aligns it once, and JIT code never pushes on the hot path) | — |
| **RBP** | `&CpuState + 128` (biased) | callee-saved |
| **RBX** | direct mode: guest memory base. softmmu: extra allocatable register (it survives helper calls) | callee-saved |
| **R12** | pinned guest **x2 (sp)** | callee-saved |
| **R13** | pinned guest **x1 (ra)** | callee-saved |
| **R14** | pinned guest **x10 (a0)** | callee-saved |
| **R15** | pinned guest **x15 (a5)** (GCC's favourite temporary) | callee-saved |
| R10, R11 | emitter scratch (address computation, TLB path, patch thunks). Never allocated. | caller-saved |
| R9 | IR back end (`--regalloc=pinned/linear`): the instruction **budget** (D46), loaded by `enter_jit`, stored by `exit_jit`, synced around helper calls. `--regalloc=none`: pool | caller-saved |
| RAX, RCX, RDX, RSI, RDI, R8 | **allocatable pool** (6; 7 with R9 at `none`). Fixed constraints: RAX/RDX for MUL/IMUL(1-op)/DIV/IDIV/CQO, RCX for variable shifts when BMI2 is absent | caller-saved |
| XMM0–XMM15 | FP scratch: inline FP uses XMM0–2 within one guest instruction (D47). All caller-saved in SysV. | caller-saved |

The pinned set (x2, x1, x10, x15) is the default; `--pin` overrides it (D39). Phase 4 measured the alternatives with `--stats=regs` and CoreMark timings and kept it (D42); re-check with Dhrystone and a Linux boot.

### 8.3 Block-boundary ABI (invariant, never violate)
At **every** block entry and exit, including chained jumps and jump-cache jumps:
1. RBP = biased `CpuState*`. RBX = memory base (direct mode).
2. The pinned guest registers live **only** in R12–R15. Their `CpuState` slots are stale until `exit_jit` writes them back.
3. All other guest registers live in `CpuState`, up to date.
4. There are no live values in the allocatable pool or in scratch registers.
5. RSP is exactly what `enter_jit` left, so it is 16-byte aligned.

Inside a block, guest registers may be cached in pool registers and be *dirty*. Every exit path (normal exits and cold stubs) writes dirty values back first.

### 8.4 Entry and exit trampolines (generated into the code buffer at startup)
```
enter_jit(rdi = CpuState*, rsi = host_code) -> rax:
    push rbp; push rbx; push r12; push r13; push r14; push r15   ; 6 pushes
    sub  rsp, 8                    ; entry rsp≡8 (mod 16) → +48 +8 → ≡0: aligned for calls
    lea  rbp, [rdi + 128]
    mov  rbx, [rbp + MEM_BASE-128]
    mov  r12, [rbp + 8*2-128] ; mov r13, [rbp + 8*1-128] ; mov r14, [rbp + 8*10-128] ; mov r15, [rbp + 8*15-128]
    mov  r9, [rbp + BUDGET-128]    ; IR back end only (D46)
    jmp  rsi
exit_jit (rax = exit code):
    mov [rbp+8*2-128], r12 ; ... (write back pinned)
    mov [rbp+BUDGET-128], r9       ; IR back end only (D46)
    add rsp, 8 ; pop r15 ; pop r14 ; pop r13 ; pop r12 ; pop rbx ; pop rbp ; ret
```
- In Rust: `let enter: extern "sysv64" fn(*mut CpuState, *const u8) -> u64 = transmute(rx_ptr);`. This is the `void (*run_block)(CPUState*)` cast from the brief, with the block address passed as an argument.
- **Exit code:** `rax = (tb_id << 2) | slot` (D25; originally planned as `tb_ptr | slot`). Slots 0 and 1 are chainable direct exits. Slot 2 means "see `cpu.exit_reason`" (exception, ecall, budget, indirect miss, halt, SMC).
- **`fault_exit`** (third trampoline): the SIGSEGV handler resumes here. It sets `exit_reason = HOST_FAULT` and jumps to `exit_jit` (JIT code never pushes, so RSP is still `enter_jit`'s).
- **Rust helpers called from JIT code:** always `extern "sysv64"` and **must never unwind or panic across JIT frames**. Wrap them in `catch_unwind` → abort, or use `panic = "abort"` in the release profile. Call them through an absolute-address table inside the code buffer (`call [rip+disp32]`, `FF 15`) or `mov rax, imm64; call rax`, because Rust code may be more than 2 GiB away (rel32 range).

---

## 9. Intermediate representation

- Scope: **one translation block (TB)**. It is single-entry and ends at the first control-flow instruction or a block-ending condition (§13.2). It can have several exits (conditional branch = 2).
- Values: `V(u32)` virtual registers, each defined exactly once (trivially SSA, no phis). Width is always 64. W-ops are explicit ops whose result is sign-extended.
- **Ops** (`ir/ops.rs`):
  - Data: `Const{dst,imm}`, `ReadReg{dst,g}`, `WriteReg{g,src}`, `ReadFReg/WriteFReg`
  - ALU: `Bin{op,dst,a,b}` / `BinImm{op,dst,a,imm}`, where op ∈ Add Sub And Or Xor Shl Shr Sar Slt SltU Mul MulH MulHU MulHSU Div DivU Rem RemU, plus the `*W` variants
  - Extensions: `Ext{kind∈Sext8/16/32,Zext8/16/32}`
  - Memory: `Load{dst,addr,off:i32,size,signed,insn}`, `Store{addr,off,val,size,insn}`, `Amo{op,dst,addr,val,size,aq,rl}`, `Lr`, `Sc`
  - Calls: `CallHelper{f,args,dst,sync∈{None,Dirty,Full}}` for CSR ops, FP helpers and interp fallback
  - Barriers and system: `Fence{pred,succ}`, `FenceI`, `SfenceVma`, `Ecall`, `Ebreak`, `Mret`, `Sret`, `Wfi`
  - Markers: `InsnStart{pc,len}` (guest instruction boundary, used for precise exceptions, the icount remainder, the host→guest PC map and `--dump-ir`)
  - Terminators: `Branch{cond,a,b,taken_pc,fall_pc}`, `Jump{pc}`, `JumpInd{target,link}`, `Exit{reason,pc}`
- **Passes** (all linear time, in this order):
  1. **Lift.** `Inst` → IR. Reads of `x0` become `Const 0`, and writes to `x0` are dropped.
  2. **Guest-reg forwarding.** A `ReadReg g` after a `WriteReg g,v` in the same block becomes `v`. Repeated reads are CSE'd.
  3. **Constant folding and propagation.** LUI+ADDI(W) → one Const, AUIPC → Const (pc known), `x op 0` identities, branch on constants → `Jump`.
  4. **Dead write-back elimination.** A `WriteReg g` that is overwritten later in the block is dead, *provided no potentially-faulting op or helper sits in between*. Otherwise the value is kept in the fault site's state map rather than stored (§15).
  5. **Liveness (backward)** produces live intervals `[def, last_use]` plus fixed-register constraints.
- Debug output: `--dump-ir` prints each TB's IR before and after the passes.

## 10. Register allocation (linear scan per block)

- Algorithm: Poletto & Sarkar linear scan over the intervals in start order. Pool: the 7 caller-saved registers (plus RBX in softmmu mode).
- **Fixed constraints:**
  - MUL-high, DIV and REM pin their operands and results to RAX/RDX.
  - Variable shifts without BMI2 need RCX.
  - Helper calls clobber the whole pool.
- **Guest-register caching.** A guest register is loaded ("filled") into a pool register on first use and reused afterwards. Writes mark it dirty. At every exit, dirty registers are written back to their `CpuState` home.
- **Spill heuristic:** evict the interval whose next use is furthest away.
  - Spilling a *clean* guest-register value costs nothing (re-fill from its home).
  - Spilling a *dirty* guest register means storing it home. That store would happen at exit anyway, so it is not wasted.
  - Pure temporaries spill to `cpu.spill[k]`.
- **Zero-cost spill resolution on hot paths.** Slow paths (TLB miss, helper calls, exceptions) live in **cold stubs** at the block tail. Each stub saves exactly the live, dirty, caller-saved registers, calls the helper, restores them, and jumps back. The hot fall-through path contains no spill code for these events.
- `--regalloc=none|pinned|linear`: `none` keeps every guest register in memory (naive baseline), `pinned` uses R12–R15 only, `linear` is the full allocator. Each level is benchmarked separately (§22).

---

## 11. x86-64 emitter reference (`backend/x86/emit.rs`)

### 11.1 Encoding building blocks
- **Instruction layout:** `[legacy prefixes: 66/F2/F3/F0] [REX] opcode(1–3) [ModRM] [SIB] [disp8/32] [imm8/16/32/64]`.
- **REX** = `0100 W R X B`:
  - W = 64-bit operand size
  - R extends ModRM.reg
  - X extends SIB.index
  - B extends ModRM.rm, SIB.base or the opcode register
- **ModRM** = `mod(2) reg(3) rm(3)`:
  - `mod=00`: `[rm]`
  - `mod=01`: `[rm+disp8]`
  - `mod=10`: `[rm+disp32]`
  - `mod=11`: register direct
- **SIB** = `scale(2) index(3) base(3)`.
- Register numbers: RAX0 RCX1 RDX2 RBX3 RSP4 RBP5 RSI6 RDI7 R8–R15 = 8–15 (low 3 bits go in the field, bit 3 goes in REX).
- **Gotchas (all must be covered by tests):**
  1. `rm=100` (RSP/**R12**) as a base **requires a SIB byte** (`index=100` = none).
  2. `mod=00, rm=101` (RBP/**R13**) means RIP+disp32, so `[rbp]`/`[r13]` must be encoded as `mod=01, disp8=0`. The same applies to SIB base=101 with mod=00.
  3. `SIB.index=100` means no index, so RSP can't be an index. R12 *can* be one (with REX.X).
  4. Byte registers SPL/BPL/SIL/DIL need a REX prefix (possibly a bare `0x40`). Without REX, encodings 4–7 mean AH/CH/DH/BH.
  5. 32-bit ops zero-extend into the upper half. `imm32` operands of 64-bit ops are **sign-extended**.
  6. Prefer `mov r32, imm32` (zero-extends) for constants in 0…2³²−1, `mov r/m64, imm32` (`C7 /0`, sign-extends) for small negative constants, `movabs r64, imm64` (`REX.W B8+r`) otherwise, and `xor r32,r32` for zero (it clobbers flags, so emit it before any `cmp`).

### 11.2 Opcode table (the subset we emit)
| Instruction | Encoding |
|---|---|
| MOV r/m64,r64 · MOV r64,r/m64 | `REX.W 89 /r` · `REX.W 8B /r` |
| MOV r32,imm32 · MOV r/m64,imm32 · MOVABS r64,imm64 | `B8+rd id` · `REX.W C7 /0 id` · `REX.W B8+rd io` |
| MOVSXD r64,r/m32 | `REX.W 63 /r` |
| MOVZX r32,r/m8 · r/m16 | `0F B6 /r` · `0F B7 /r` |
| MOVSX r64,r/m8 · r/m16 | `REX.W 0F BE /r` · `REX.W 0F BF /r` |
| store 16-bit / 8-bit | `66 89 /r` / `88 /r` (REX for SIL/DIL) |
| LEA r64,m | `REX.W 8D /r` |
| ALU r/m,r · r,r/m | ADD `01/03`, OR `09/0B`, AND `21/23`, SUB `29/2B`, XOR `31/33`, CMP `39/3B` |
| ALU group-1 imm | `REX.W 83 /digit ib` (imm8) · `REX.W 81 /digit id`, where digit: ADD0 OR1 ADC2 SBB3 AND4 SUB5 XOR6 CMP7 |
| TEST r/m64,r64 | `REX.W 85 /r` |
| Shifts imm · by CL | `REX.W C1 /4 ib` SHL, `/5` SHR, `/7` SAR · `REX.W D3 /n` |
| BMI2 shifts | SHLX `VEX.66.0F38.W1 F7`, SHRX `VEX.F2…F7`, SARX `VEX.F3…F7` |
| IMUL r64,r/m64 | `REX.W 0F AF /r` |
| MUL / IMUL(1-op) / DIV / IDIV r/m64 | `REX.W F7 /4` / `/5` / `/6` / `/7` (RDX:RAX) |
| CQO | `REX.W 99` |
| SETcc r/m8 · CMOVcc r64,r/m64 | `0F 90+cc /0` · `REX.W 0F 40+cc /r` |
| Jcc rel8 · Jcc rel32 | `70+cc cb` · `0F 80+cc cd` |
| JMP rel8 · rel32 · r/m64 | `EB cb` · `E9 cd` · `FF /4` |
| CALL rel32 · r/m64 · [rip+d32] | `E8 cd` · `FF /2` · `FF 15 d32` |
| RET · NOP · multi-byte NOP | `C3` · `90` · `0F 1F /0` (use the recommended 2–9 byte forms for alignment padding) |
| LOCK XADD · LOCK CMPXCHG · XCHG | `F0 REX.W 0F C1 /r` · `F0 REX.W 0F B1 /r` · `REX.W 87 /r` (implicitly locked) |
| MFENCE | `0F AE F0` |
| SSE2 scalar (F2) | ADDSD/SS `F2/F3 0F 58`, SUBSD `5C`, MULSD `59`, DIVSD `5E`, SQRTSD `51`, MINSD `5D`, MAXSD `5F`, UCOMISD `66 0F 2E`, CVTSI2SD `F2 REX.W 0F 2A`, CVTTSD2SI `F2 REX.W 0F 2C`, MOVQ xmm↔r64 `66 REX.W 0F 6E/7E`, ROUNDSD (SSE4.1) `66 0F 3A 0B` |
| STMXCSR / LDMXCSR | `0F AE /3` / `0F AE /2` |

**Condition codes** (`cc` nibble): O0 NO1 **B2 AE3 E4 NE5** BE6 A7 S8 NS9 P A NP B **L C GE D** LE E G F.

**RISC-V branch mapping.** Emit `cmp rs1, rs2` using opcode `39` (computes r/m − reg with rs1 in r/m). Operand order matters: `3B` reverses it.

| RISC-V | x86 cc |
|---|---|
| BEQ | JE (4) |
| BNE | JNE (5) |
| BLT | JL (C) |
| BGE | JGE (D) |
| BLTU | JB (2) |
| BGEU | JAE (3) |

**Relative displacements.** `rel = target − (address of the *next* instruction)`:
- `jcc rel32` is 6 bytes, so `rel = T − (A+6)`.
- `jmp rel32` is 5 bytes, so `rel = T − (A+5)`.
- `rel8` forms are 2 bytes.

Everything inside the code buffer must be within ±2 GiB, so **cap the code buffer at 1 GiB**. The default is 256 MiB.

### 11.3 RISC-V → x86 lowering cheat-sheet
| RISC-V | x86-64 (d = host reg of rd, a/b = rs1/rs2) |
|---|---|
| add / addi | `lea d,[a+b]` / `lea d,[a+imm]` (no flags) or `mov d,a; add d,b` |
| sub | `mov d,a; sub d,b` (careful when d==b: use a scratch or `neg`+`add`) |
| addw/addiw/subw | 32-bit op, then `movsxd d, d32` |
| sll/srl/sra | BMI2 `shlx/shrx/sarx d,a,b`; otherwise `mov rcx,b; shl d,cl` |
| slt / sltu | `xor t,t; cmp a,b; setl/setb t8; mov d,t` |
| lui / auipc | `mov d, const` (folded) |
| mul / mulh / mulhu / mulhsu | `imul d,b` / `mov rax,a; imul b` → rdx / `mul b` → rdx / mulhu fix-up |
| div/divu/rem/remu (+w) | guard for zero and overflow (§7.5), then `cqo; idiv` / `xor edx,edx; div` |
| ld/lw/lwu/lh/lhu/lb/lbu | `mov` / `movsxd` / `mov r32` / `movsx` / `movzx`, on `[rbx+a+imm]` (direct) or the TLB path (§14) |
| sd/sw/sh/sb | `mov [..], r64/r32/r16/r8` |
| jal | `rd = const pc+len`; chainable exit to `pc+imm` |
| jalr | `t = (a+imm) & ~1`; `rd = pc+len`; jump-cache lookup (§13.4) |
| beq…bgeu | `cmp a,b; jcc` (two chain slots) |
| amoadd.d / amoswap.d | `lock xadd` / `xchg` |
| amoand/or/xor/min/max(u) | `lock cmpxchg` loop |
| lr.d / sc.d | store reservation (addr, value) / compare addr, then `lock cmpxchg` |
| ecall / ebreak | exit with `EXIT_ECALL` / `EXIT_EBREAK` (pc = this instruction) |
| csr* | helper call with full sync (hot counters like `rdcycle` may be inlined later) |
| anything else | `helper_interp_one` fallback (D14) |

RISC-V has **no condition flags**, so there is no lazy-flag machinery (unlike x86→ARM translators). Compare-and-branch maps directly onto `CMP`+`Jcc`.

---

## 12. JIT memory: code buffer and W^X

- **Dual mapping (default):**
  1. `fd = memfd_create("bridgev-jit", MFD_CLOEXEC)` and `ftruncate(fd, SIZE)`.
  2. `rw = mmap(NULL, SIZE, PROT_READ|PROT_WRITE, MAP_SHARED, fd, 0)`.
  3. `rx = mmap(NULL, SIZE, PROT_READ|PROT_EXEC, MAP_SHARED, fd, 0)`.
  4. Emit and patch through `rw`, execute through `rx`. **All rel32 math uses `rx` addresses.**
- **Fallback (`--wx=mprotect`):** `mmap(PROT_READ|PROT_WRITE, MAP_PRIVATE|MAP_ANONYMOUS)`, emit, then `mprotect(PROT_READ|PROT_EXEC)`. Chain patching toggles page protection, so it is slow. Keep it for comparison and for hardened kernels.
- **Never create RWX mappings.**
- x86 keeps instruction caches coherent with data writes, so no explicit I-cache flush is needed. A write near code that is currently executing triggers a pipeline "machine clear", so patch only from the dispatcher, never from inside a running block.
- **Allocation:** bump pointer. Each TB starts on a 16-byte boundary. When the buffer is full, **flush everything**: reset the bump pointer, clear `tb_map`, jump caches and page→TB index, and bump a generation counter. This only happens from the dispatcher, when no JIT frame is live.
- Size: `--code-cache=256M` (≤ 1 GiB).

---

## 13. Translation cache, block formation and chaining

### 13.1 `TranslationBlock` metadata (Rust heap, not in the code buffer)
```rust
struct TranslationBlock {
    guest_pc: u64, flags: TbFlags,          // flags: priv, mmu_idx, FS state, misa.C, xlen …
    phys_pages: [u64; 2], n_pages: u8,      // system mode: physical pages spanned (SMC index)
    n_insns: u32, guest_bytes: u32,
    host_off: u32, host_len: u32,           // location in code buffer
    exits: [ExitSlot; 2],                   // { patch_off (rel32 field), target_pc, linked: Option<TbId> }
    incoming: SmallVec<[(TbId, u8); 4]>,    // who jumps into me (for unlinking)
    pcmap: Vec<(u32 host_off, u32 guest_pc_delta)>, // host→guest pc (faults, perf, debug)
}
```
- Lookup: `tb_map: FxHashMap<(guest_pc, TbFlags), TbId>`, plus the JIT-visible `jmp_cache` in `CpuState`, plus a `page_tbs: FxHashMap<phys_page, Vec<TbId>>` index for SMC.

### 13.2 Block formation (translator stops at the first of)
- A branch, JAL, JALR, ECALL, EBREAK, MRET, SRET, WFI, FENCE.I or SFENCE.VMA.
- A CSR write that can change translation or interrupt state (satp, mstatus, sstatus, misa, mie/sie, fcsr with an frm change → ends the block so the flags are re-evaluated).
- **Reaching the end of a guest page** (a block never spans two virtual pages). The exception is a single 32-bit instruction straddling a page boundary: it becomes a one-instruction TB recording both physical pages.
- The instruction limit (`--max-block=128`) or host-size limit.
- An instruction whose fetch faults. Translate the preceding instructions, then exit with the fault at that PC.

### 13.3 Direct block chaining
The exit layout for a conditional branch (hot path, fall-through last):
```
    cmp    a, b
    jcc    rel32   ─── slot 1 (taken)   → initially: stub_1 ; after link: TB(taken_pc).entry
    jmp    rel32   ─── slot 0 (fall)    → initially: stub_0 ; after link: TB(fall_pc).entry
    ...cold stubs...
stub_k:  (dirty regs already written back on the path)
    mov    r11, imm64(target_pc) ; mov [rbp+PC], r11
    mov    rax, imm64(tb_ptr | k)
    jmp    exit_jit
```
- **Linking.** When the dispatcher sees exit `(tb A, slot k)` and finds or creates TB B for `A.exits[k].target_pc`, and `may_link(A,B)` holds, it rewrites the rel32 at `A.exits[k].patch_off` to `B.entry − (slot_addr + insn_len)`, records `B.incoming.push((A,k))`, and sets `A.exits[k].linked = Some(B)`.
- **Alignment rule.** Emit NOP padding so the **rel32 field is 4-byte aligned**. The patch is then a single atomic aligned 32-bit store (safe even against concurrent execution in the MT stretch goal).
- **Unlinking** (B invalidated): for each `(A,k)` in `B.incoming`, reset the rel32 to point at A's own `stub_k`.
- **`may_link(A,B)`** requires:
  - The same `TbFlags`.
  - In system mode, `B.guest_pc` must be on the **same virtual page** as A. The virtual→physical mapping can change without TB invalidation, so a cross-page transfer goes through the jump cache and TLB exec check instead.
  - In user mode (flat mapping), link freely.
- **Prologue of every TB** (D12): `sub qword [rbp+BUDGET-128], n_insns ; jl budget_stub`. It is 2 instructions and keeps chained infinite loops pre-emptible. `budget_stub` exits with `pc = this TB`, having executed nothing.
- `--no-chain` disables linking so the speedup from chaining can be measured.

### 13.4 Indirect branches (JALR) and the jump cache
```
    ; r11 = (a + imm) & ~1   (target guest pc) ; rd already set to pc+len; dirty regs written back
    mov   [rbp+PC-128], r11
    mov   r10, r11
    shr   r10, 1
    and   r10d, 4095
    shl   r10, 4                       ; 16-byte entries
    cmp   r11, [rbp + r10 + JC_OFF-128]
    jne   miss_stub                    ; → exit(slot 2, EXIT_LOOKUP): dispatcher translates + fills entry
    jmp   qword [rbp + r10 + JC_OFF-128 + 8]
```
- Flush the jump cache on: a change of priv or TbFlags, a satp write, SFENCE.VMA, FENCE.I, TB invalidation (remove the matching entries), and a cache flush.
- Stretch: a return-address stack (shadow stack) that predicts `ret` (`jalr x0, 0(ra)`).

---

## 14. Guest memory, the SV39 MMU and the software TLB

### 14.1 User mode, `direct` backend
- Reserve the guest address space: `mmap(NULL, 2^38 + 2*4GiB, PROT_NONE, MAP_PRIVATE|MAP_ANONYMOUS|MAP_NORESERVE)`. `mem_base` = start + 4 GiB, which leaves guard regions below and above. 2³⁸ = 256 GiB, the SV39 user half.
- A guest mapping (ELF segments, brk, mmap, stack) becomes `mmap(mem_base + gaddr, len, prot, MAP_FIXED|...)` inside the reservation.
- Memory access: `[rbx + a + imm]` (base+index+disp32) with **no checks**. The host MMU enforces permissions, and faults arrive as SIGSEGV.
- Documented limitation (same as `qemu-user`): wild guest pointers ≥ 2³⁸+4 GiB could alias host memory. `--mem=direct-checked` adds `cmp`+`ja fault` (2 instructions) for safety.
- **Host SIGSEGV handler** (installed with `SA_SIGINFO|SA_ONSTACK` on a sigaltstack):
  - Fault address inside the reservation, host RIP inside the code buffer, and the page is write-protected because of SMC: invalidate that page's TBs, restore write permission, **return** (the faulting store retries).
  - Otherwise it is a genuine guest fault. Map RIP → TB → `pcmap` → guest PC, then deliver guest SIGSEGV. Default action: print guest PC and registers, exit 139.
  - Guest `mprotect` state is tracked separately from SMC write-protection, so a real write to a read-only page still becomes a guest fault.
- Layout (mirrors Linux on SV39):
  - Static ELF linked at 0x10000
  - brk after `.bss`
  - mmap region top-down from `0x3F_0000_0000`
  - Stack top `0x3F_FFFF_F000`, 8 MiB

### 14.2 System mode, physical memory
- RAM: one host `mmap` (default 512 MiB) at guest physical 0x8000_0000.
- MMIO: a sorted list of `(base, size, &dyn Device)`. Devices implement `read(off, size) -> u64` and `write(off, size, val)`.
- Physical accesses outside RAM and devices raise an access fault (cause 1, 5 or 7).

### 14.3 SV39 translation (priv spec; SV48 = 4 levels, a stretch goal)
- **satp:** `MODE[63:60]` (0 = Bare, 8 = Sv39, 9 = Sv48), `ASID[59:44]`, `PPN[43:0]`.
- **VA (Sv39):** `VPN[2]=va[38:30] VPN[1]=va[29:21] VPN[0]=va[20:12] offset=va[11:0]`. Bits 63:39 must equal bit 38, otherwise it is a page fault.
- **PTE:** `V0 R1 W2 X3 U4 G5 A6 D7 RSW[9:8] PPN0[18:10] PPN1[27:19] PPN2[53:28] reserved[60:54] PBMT[62:61] N[63]`. Nonzero reserved, PBMT or N bits cause a page fault (Svpbmt and Svnapot are not implemented and not advertised).
- **Walk:**
  1. `a = satp.PPN·4096`, `i = 2`.
  2. Read `pte = mem[a + VPN[i]·8]`, which is a physical access that can itself fault.
  3. If `!V` or `(!R && W)`, page fault.
  4. If `R|X`, it is a leaf. Check superpage alignment (`PPN[i-1:0]` must be 0, otherwise fault). Otherwise set `a = pte.PPN·4096` and `i -= 1`. If `i < 0`, fault.
- **Permissions:**
  - Fetch needs X.
  - Load needs R (or X when `mstatus.MXR`).
  - Store and AMO need W.
  - U-mode needs U=1.
  - S-mode may touch U pages only when `SUM=1`, and may **never execute** them.
  - `MPRV=1` makes M-mode loads and stores translate with `MPP` privilege (OpenSBI relies on this).
- **A/D bits:** the walker sets A (and D on stores) itself, atomically, like Svadu. This is legal under the priv spec and saves a trap.
- **Fault causes:** 12 (instruction), 13 (load), 15 (store/AMO) page fault; `xtval` = faulting VA.
- Superpages (1 GiB, 2 MiB) fill the TLB at 4 KiB granularity.

### 14.4 Software TLB and the inline fast path
- `TlbEntry { addr_read, addr_write, addr_code, addend }` is 32 bytes. `addr_*` = virtual page | flag bits, or `u64::MAX` when invalid. `addend = host_ptr(page) − vpage`.
- **Flag bits** live in bits 3–11 of `addr_*`. They sit above the largest alignment mask (7) and below the page offset. Because the compared value always has those bits clear, a set flag **forces the slow path**:
  - `TLB_MMIO`: device page
  - `TLB_CODE`: page contains translated code, used on `addr_write` only (SMC, §16)
  - `TLB_WATCH`: reserved for debug watchpoints
- The permission and privilege checks are folded in at fill time. A read-only page gets `addr_write = MAX`. The dirty bit is also handled at fill time: `addr_write` stays invalid until the walker has set D.
- One TLB per MMU index (U, S, M/bare, plus MPRV variants). The index is part of `TbFlags`, so the TLB offset is a constant baked into the code.

**Fast path for `ld a0, 16(s1)`** as emitted (`lower_ir.rs::tlb_probe`, D48; s1 in RSI, a0's register RDI, TLB_SIZE = 256, MMU index S):
```
    lea   r11, [rsi + 16]                      ; guest vaddr
    mov   r10, r11
    shr   r10, 7                               ; 12 (page) − 5 (log2 entry size)
    and   r10d, 0xFF << 5                      ; entry byte offset
    and   r11, -4096 | 7                       ; page | misalign bits (imm32 0xFFFFF007, sign-extended)
    cmp   r11, [rbp + r10 + TLB_OFF(idx)+0 -128]   ; addr_read (+8: addr_write)
    jne   .Lslow_N                             ; cold stub
    lea   r11, [rsi + 16]                      ; vaddr again (the base register is intact)
    add   r11, [rbp + r10 + TLB_OFF(idx)+24 -128]  ; + addend → host address
    mov   rdi, [r11]                           ; the fault site (state map) is recorded here
.Lret_N:
```
- Recomputing the address instead of borrowing the destination register works the same for stores, where there is no destination.
- The tag check itself (`shr`, `and`, `and`, `cmp`, `jne`) is 4–5 instructions. The whole hit path is about 9 instructions including the access, with no memory round-trips beyond the TLB entry.
- **Measured (P7.9, `tools/tlb-bench.py`):** a hit adds about 3 ns of load-to-use latency over a raw host load and 0.4 ns per access in throughput. An Sv39 miss (walk + fill) costs about 29 ns per access. See `docs/phase-reports/phase-07-privileged-softmmu.md`.
- Misaligned accesses always miss (the low bits are non-zero). The slow path does them byte-wise or split, and they can **never** cross a page on the fast path.
- **Slow path (cold stub per access, D48):**
  1. Save RAX, RCX, RDX, RSI, RDI, R8 and R9 (budget) to `cpu.fault_regs`.
  2. Build the arguments from the saved copies: va, info = size | signed<<4 | store<<5 | mmu_idx<<8, and the store value. Refund the budget of the unretired instructions into `cpu.budget` (D30).
  3. `call helper_mmu_access(cpu, va, info, val)`. This translates through the TLB (walking and filling on a miss), handles MMIO, misalignment and page-crossing, and returns the value or sets `exit_reason = MMU_FAULT` with `exc_cause`/`exc_tval`.
  4. Restore the registers and test `exit_reason`. On a fault, also store R12–R15 and `fault_rip` = the site, and exit (slot 2). The dispatcher applies the site's state map (D37), so the fault is precise without per-site store code. Otherwise move the value into the destination and jump to `.Lret_N`.
- **Flush:**
  - `satp` write: flush all.
  - `SFENCE.VMA`: flush all (per-page/ASID flushing is a later optimization).
  - MXR change: flush all. SUM and MPRV/MPP select separate indices (U, S, S+SUM, M/Bare), so they need no flush (D48).
  - Privilege change: nothing to flush (separate TLB per index).
  - Every flush bumps `cpu.mmu_gen`, which drops the interpreter's decoded blocks and resets the jump cache.
- **Stats:** `--stats` prints the number of TLB fills (walks), `mmu-fault` exits and page-straddling instructions. Hits are not counted (no counter on the hot path). A microbenchmark measures the cost per access on hit and miss (§22, P7.9).

---

## 15. Exceptions, interrupts and precise state

- **Precise exceptions.** Every potentially faulting IR op has a *state map* built at lowering time: the guest PC constant, plus the dirty guest registers and the host registers holding them. The cold stub stores exactly those before calling the helper. Guest state is therefore precise at the faulting instruction, and the hot path never pays for it.
- **Exception causes** (`mcause`/`scause`, interrupt bit 63 clear):

  | Code | Exception |
  |---|---|
  | 0 | instruction address misaligned |
  | 1 | instruction access fault |
  | 2 | illegal instruction (`tval` = instruction bits) |
  | 3 | breakpoint |
  | 4 | load address misaligned (never raised: misaligned accesses are supported transparently) |
  | 5 | load access fault |
  | 6 | store/AMO misaligned |
  | 7 | store/AMO access fault |
  | 8 | ecall from U |
  | 9 | ecall from S |
  | 11 | ecall from M |
  | 12 | instruction page fault |
  | 13 | load page fault |
  | 15 | store/AMO page fault |

- **Interrupt causes** (bit 63 set): 1 SSI, 3 MSI, 5 STI, 7 MTI, 9 SEI, 11 MEI.
  - **Priority:** MEI > MSI > MTI > SEI > SSI > STI.
  - **Enablement:** M-level interrupts are taken if `priv < M` or `mstatus.MIE`. S-level interrupts are taken if (delegated via `mideleg`) and (`priv < S` or (`priv == S` and `SIE`)).
- **Trap entry** to x∈{M,S} (target chosen by `medeleg`/`mideleg`):
  - `xepc = pc`, `xcause`, `xtval`
  - `xPP = priv`, `xPIE = xIE`, `xIE = 0`
  - `priv = x`
  - `pc = xtvec.BASE`, or `BASE + 4·cause` for vectored interrupts
- **MRET/SRET:** `priv = xPP`, `xIE = xPIE`, `xPIE = 1`, `xPP = U` (MRET also clears MPRV when MPP ≠ M), `pc = xepc`.
- **mstatus bits used:** SIE1 MIE3 SPIE5 MPIE7 SPP8 MPP12:11 FS14:13 MPRV17 SUM18 MXR19 TVM20 TW21 TSR22 UXL/SXL=2 SD63.
- **Interrupt delivery.** Only the dispatcher delivers interrupts, between TBs. The budget (D12) bounds latency. The following end the block so pending interrupts are noticed promptly: instructions that can unmask interrupts (csr writes to mstatus/sstatus/mie/sie, MRET, SRET) and WFI. WFI idles the host until the next timer deadline or an async event.
- **Time.** `mtime` runs at 10 MHz (`timebase-frequency = 10000000`), derived from the host monotonic clock. With `--deterministic`, `mtime = icount / K`, which makes runs reproducible for lockstep debugging.

## 16. Self-modifying code (SMC) and cache invalidation

- **Tracking.** When a TB is translated from physical page P, mark P as a *code page*. It is added to `page_tbs[P]` and, in softmmu, every `addr_write` TLB entry mapping P gets the `TLB_CODE` flag. In direct mode, P is `mprotect`ed read-only on the host.
- **On a write to a code page:**
  - softmmu: the store takes the slow path, which sees `TLB_CODE`.
  - direct: the host SIGSEGV handler fires.

  In both cases, **invalidate every TB in `page_tbs[P]`**:
  1. Unlink their incoming chains.
  2. Remove them from `tb_map` and the jump cache.
  3. Clear the code flag and write protection.
  4. Perform the write.

  The dead code bytes stay in the buffer until the next full flush. The currently executing TB is never freed mid-execution.
- **A write that hits the currently executing TB** (the block modifies itself). The helper sets `exit_reason = SMC_SELF` after the store, and the stub exits right after the current instruction. Execution resumes at the next PC with a fresh translation. RISC-V only requires the new code to be visible after FENCE.I, so this is stricter than needed.
- **FENCE.I** ends the TB, flushes the jump cache, and (since eager invalidation already happened) needs nothing else. `--smc=flush-on-fence` instead flushes the whole cache on FENCE.I, as a debug cross-check.
- **User mode:** handle the `riscv_flush_icache` syscall (259) like FENCE.I. Guest JITs and `dlopen` use it.
- **System mode:** Linux issues `fence.i` locally and remotely through SBI RFENCE (`remote_fence_i`) when mapping executable pages. Treat the SBI call as a FENCE.I.

---

## 17. Floating point (F, D)

- **Registers:** `f[32]` holds u64 values. Singles are **NaN-boxed**: the upper 32 bits are all ones. A single-precision read of a register that is not properly boxed yields the canonical NaN.
- **Canonical NaN:** `0x7FF8_0000_0000_0000` (double), `0x7FC0_0000` (single). RISC-V never propagates NaN payloads, whereas x86 does (and its default NaN `0xFFF8…` is negative). **Every NaN result must be canonicalized.**
- **fcsr** = `frm[7:5]` + `fflags[4:0]` (NV4 DZ3 OF2 UF1 NX0).
  - `frm` values: 000 RNE, 001 RTZ, 010 RDN, 011 RUP, 100 RMM, 111 DYN (from fcsr). 101, 110, and DYN when frm itself is invalid, raise illegal instruction.
  - MXCSR mapping:
    - Rounding: RNE→00, RDN→01, RUP→10, RTZ→11. **RMM has no x86 equivalent**, so it always goes through the helper.
    - Flags: IE→NV, ZE→DZ, OE→OF, UE→UF, PE→NX. DE is ignored.
- **x86 semantic differences to handle:**
  - `CVTTSD2SI` returns 0x8000… on NaN or overflow. RISC-V saturates (NaN → max positive) and honours `rm`.
  - `MINSD`/`MAXSD` differ on NaN and ±0. RISC-V `fmin`/`fmax` return the non-NaN operand and treat −0 < +0.
  - Tininess detection for UF must be verified against riscv-tests before trusting MXCSR's UE.
- **FMA:** `fmadd` is fused (one rounding). F1 uses SoftFloat `f64_mulAdd`. F2 uses VFMADD231SD/SS (FMA3) when `cpuid` reports FMA.
- **Rust pitfall:** LLVM assumes the default FP environment. **Never change MXCSR and then do FP math in Rust code.** Non-RNE operations go through SoftFloat, or through inline `asm!` that sets MXCSR and performs the operation in one block.
- **System mode:** `mstatus.FS` is Off, Initial, Clean or Dirty. With FS=Off, any FP instruction is illegal, and Linux depends on this. FS is part of `TbFlags`. FP-writing instructions set FS=Dirty (and SD).

## 18. Atomics (A) and memory ordering

- Single-threaded (phases 1–9): AMOs are plain read-modify-write, and LR/SC compares the reservation address.
- The direct backend uses the **x86 lock-prefixed** forms anyway, so moving to multithreading needs no retranslation:
  - `amoswap` → `xchg`
  - `amoadd` → `lock xadd`
  - and/or/xor/min/max(u) → `lock cmpxchg` loop
  - `sc` → `lock cmpxchg` against the value saved by `lr`. This is value-based, so ABA is accepted (QEMU does the same).
- `.aq`/`.rl` bits are free, because locked x86 operations are full barriers.
- **RVWMO vs x86-TSO:** x86 only reorders store→load. So a FENCE whose predecessor set includes W and successor set includes R becomes `MFENCE` (MT mode only). All other fences are no-ops.
- Multithreaded user mode is a stretch goal (Phase 10):
  - `clone` → one host thread per guest thread, each with its own `CpuState` and jump cache.
  - The translation cache is shared under a mutex, and TB invalidation runs in an "exclusive section" (all vCPUs parked).
  - In direct mode, `futex` passes straight through, because guest addresses map 1:1 to host addresses plus the base.

---

## 19. User-mode Linux emulation

- **ELF checks:** `ELFCLASS64`, `ELFDATA2LSB`, `e_machine == 243` (EM_RISCV), `e_type` ET_EXEC (Phase 1). ET_DYN static-PIE comes later, and PT_INTERP (dynamic, loading `ld-linux-riscv64-lp64d.so.1`) is a stretch goal.
  - `e_flags`: `EF_RISCV_RVC = 0x1`, float ABI `0x6` mask (`0x4` = double).
  - Load `PT_LOAD` segments with their permissions and zero the bss.
  - `.symtab` lookup for `tohost`/`fromhost` (riscv-tests) and for symbolized traces.
- **Initial stack** (16-byte aligned sp), from low to high addresses:
  - `argc`, `argv[]`, `NULL`, `envp[]`, `NULL`
  - auxv pairs: AT_PHDR 3, AT_PHENT 4, AT_PHNUM 5, AT_PAGESZ 6 (4096), AT_BASE 7, AT_ENTRY 9, AT_UID 11, AT_EUID 12, AT_GID 13, AT_EGID 14, AT_HWCAP 16 (bits `1<<(letter−'A')` for I M A F D C), AT_CLKTCK 17, AT_SECURE 23, AT_RANDOM 25 (pointer to 16 random bytes), AT_EXECFN 31, AT_NULL 0
  - then the strings
- **Syscall ABI:** number in a7, arguments in a0–a5, return value in a0 (negative errno on error). ECALL exits the TB, the dispatcher services it, and execution resumes at pc+4.
- **Syscall table** (asm-generic numbers, Phase 1 set):

  | # | Syscall | # | Syscall |
  |---|---|---|---|
  | 17 | getcwd | 29 | ioctl (TCGETS/TIOCGWINSZ minimal) |
  | 48 | faccessat | 56 | openat |
  | 57 | close | 62 | lseek |
  | 63 | read | 64 | write |
  | 65 | readv | 66 | writev |
  | 78 | readlinkat (`/proc/self/exe` → guest path) | 79 | newfstatat |
  | 80 | fstat | 93 | exit |
  | 94 | exit_group | 96 | set_tid_address |
  | 98 | futex | 99 | set_robust_list |
  | 113 | clock_gettime | 124 | sched_yield |
  | 131 | tgkill | 134 | rt_sigaction |
  | 135 | rt_sigprocmask | 139 | rt_sigreturn |
  | 160 | uname (machine = "riscv64") | 169 | gettimeofday |
  | 172 | getpid | 178 | gettid |
  | 214 | brk | 215 | munmap |
  | 220 | clone (stretch) | 222 | mmap |
  | 226 | mprotect | 233 | madvise |
  | 259 | riscv_flush_icache | 261 | prlimit64 |
  | 278 | getrandom | 291 | statx |
  | 293 | rseq (→ −ENOSYS) | | |

  Anything unknown logs once and returns −ENOSYS.
- **Struct translation is mandatory.** The guest riscv64 `struct stat` uses the asm-generic layout (128 bytes: dev, ino, mode u32, nlink u32, uid, gid, rdev, pad, size, blksize i32, pad, blocks, a/m/ctime with nsec). It differs from x86-64's 144-byte layout. Never pass guest structs straight through to host syscalls. The same applies to `sigaction`, `rlimit` (identical but check), `utsname` (identical sizes), `timespec` (identical on 64-bit).
- **Signals** (stretch; needed by some programs, not by CoreMark):
  - Guest handlers get an rt_sigframe on the guest stack: siginfo + ucontext with `sc_regs` (pc, x1…x31) + FP state. `rt_sigreturn` restores it.
  - Host async signals set a pending flag, which the dispatcher delivers within one slice.

## 20. System mode (full-system emulation, Milestone B)

### 20.1 Machine memory map (QEMU `virt` compatible)
| Base | Size | Device |
|---|---|---|
| 0x0000_1000 | 0x1000 | Boot ROM: reset vector sets `a0 = hartid`, `a1 = &dtb`, jumps to firmware or kernel |
| 0x0010_0000 | 0x1000 | syscon/test finisher: write 0x5555 = poweroff/pass, 0x3333 = fail, 0x7777 = reset |
| 0x0200_0000 | 0x10000 | **CLINT**: msip @+0x0, mtimecmp @+0x4000, mtime @+0xBFF8 |
| 0x0C00_0000 | 0x600000 | **PLIC**: priority @+0x0 (4 B/src), pending @+0x1000, enable @+0x2000 (+0x80/context), threshold @+0x200000 & claim/complete @+0x200004 (+0x1000/context). Context 0 = hart0 M, context 1 = hart0 S. `riscv,ndev = 53`. |
| 0x1000_0000 | 0x100 | **UART 16550A**, IRQ 10, `clock-frequency = 3686400`. Registers: RBR/THR/DLL 0, IER/DLM 1, IIR/FCR 2, LCR 3, MCR 4, LSR 5 (DR bit0, THRE bit5, TEMT bit6), MSR 6, SCR 7. Host stdin is read by a thread (raw tty mode) → RX FIFO → IRQ. |
| 0x1000_1000 | 8×0x1000 | virtio-mmio (stretch: virtio-blk), IRQs 1–8 |
| 0x8000_0000 | `--ram` (default 512 MiB) | DRAM |

### 20.2 Boot flows
- **Built-in SBI (D15, first):**
  - Load the kernel `Image` at **0x8020_0000**. RV64 kernels must be 2 MiB aligned. The Image header has `text_offset` at +0x08, magic `"RISCV\0\0\0"` at +0x30 and `"RSC\x05"` at +0x38.
  - Put the DTB at the top of RAM, 2 MiB aligned. Put the initrd below it if it is separate.
  - Hart 0 starts in **S-mode** at 0x8020_0000 with `a0 = 0`, `a1 = dtb`, `satp = 0`.
  - `medeleg` = all synchronous causes except 9 and 11; `mideleg` = SSI|STI|SEI (0x222); `mcounteren = 0x7`.
  - An S-mode `ecall` is intercepted by the dispatcher and serviced in Rust:

    | Extension | EID | Functions |
    |---|---|---|
    | legacy | 0x00–0x08 | set_timer, console_putchar, console_getchar, … |
    | BASE | 0x10 | spec version 2.0, probe_extension, mvendorid/marchid/mimpid |
    | TIME | 0x54494D45 | set_timer: arms the S-timer deadline; when `mtime ≥ deadline`, set `mip.STIP` |
    | IPI | 0x735049 | |
    | RFENCE | 0x52464E43 | remote_fence_i → FENCE.I semantics; remote_sfence_vma → TLB flush |
    | HSM | 0x48534D | hart_start/stop/status (single hart) |
    | SRST | 0x53525354 | system reset/shutdown → exit |
    | DBCN | 0x4442434E | debug console write/read |

    Calling convention: a7 = EID, a6 = FID, a0–a5 = arguments. Returns a0 = error (0 = success, −2 = not supported), a1 = value.
- **OpenSBI (later validation):** load `fw_jump.bin` at 0x8000_0000 (FW_JUMP_ADDR = 0x8020_0000) and start in M-mode at the boot ROM. This needs the complete M-mode CSR set, the `pmpcfg*`/`pmpaddr*` CSRs (accept writes, always permit), `mcounteren`, `menvcfg`, MPRV, and misaligned-access emulation paths.

### 20.3 Devicetree (generated by `system/fdt.rs`; `--dtb` overrides)
- Root: `compatible = "riscv-virtio"`, `#address-cells = #size-cells = 2`.
- `/cpus`: `timebase-frequency = 10000000`.
  - `cpu@0`: `device_type = "cpu"`, `compatible = "riscv"`, `riscv,isa = "rv64imafdc_zicsr_zifencei"`. Also emit `riscv,isa-base = "rv64i"` and `riscv,isa-extensions = [...]` for 6.7+ kernels. `mmu-type = "riscv,sv39"`, `status = "okay"`.
  - A child `interrupt-controller` with `compatible = "riscv,cpu-intc"` and `#interrupt-cells = 1`.
- `/memory@80000000`.
- `/soc`: `clint@2000000` (`"sifive,clint0"`, `"riscv,clint0"`), `plic@c000000` (`"sifive,plic-1.0.0"`, `"riscv,plic0"`, `riscv,ndev = 53`, `interrupts-extended` → hart context M(11) and S(9)), `serial@10000000` (`"ns16550a"`, interrupts = 10), `test@100000` (`"sifive,test1"`, `"sifive,test0"`, `"syscon"`) plus `poweroff` and `reboot` syscon nodes.
- `/chosen`: `bootargs = "console=ttyS0 earlycon=sbi"`, `stdout-path = "/soc/serial@10000000"`, and `linux,initrd-start`/`-end` when the initrd is separate.
- The FDT blob format is: header (magic 0xd00dfeed, big-endian), memory-reservation block, structure block (BEGIN_NODE 1, END_NODE 2, PROP 3, END 9), strings block.

### 20.4 Guest software build (`guest/linux/build.sh`)
- Toolchain: `riscv64-linux-gnu-gcc` (apt).
- Kernel: Linux 6.6 LTS. `make ARCH=riscv CROSS_COMPILE=riscv64-linux-gnu- defconfig`, then merge `guest/linux/bridgev.config`:
  - `CONFIG_INITRAMFS_SOURCE`
  - `CONFIG_SERIAL_8250=y`, `CONFIG_SERIAL_8250_CONSOLE=y`, `CONFIG_SERIAL_OF_PLATFORM=y`
  - `CONFIG_HVC_RISCV_SBI=y`
  - `CONFIG_FPU=y`
  - Disable unneeded drivers to speed up boot. SMP can stay enabled, because the DT describes one hart.
- BusyBox 1.36.x: `defconfig` + `CONFIG_STATIC=y`, installed into the initramfs skeleton.
  - `/init`: mount proc/sys/devtmpfs, then `exec /bin/sh`, or use `/etc/inittab` with `::respawn:-/bin/sh` and `ttyS0`.
- Build time is about 15–30 min on 4 vCPU. Cloud containers are ephemeral, so **cache the built `Image` and `rootfs.cpio`**, for example as a GitHub Actions artifact or release asset fetched by `tools/fetch-guest-images.sh`. **Never commit large binaries to git.**
- **Success criterion:** the boot log reaches `/ #`, and an automated test sends `uname -a; cat /proc/cpuinfo; ls /` over the emulated UART and checks the output. Report time-to-shell and MIPS during boot.

---

## 21. Verification strategy (discipline: nothing is "done" without evidence)

1. **Decoder unit tests:** every instruction, including RVC, checked against `llvm-mc` golden encodings, plus illegal and reserved encodings.
2. **Emitter golden tests:** every emitter function and register combination (all 16 registers as base, index and reg, with disp8/disp32 boundaries) is encoded, then decoded with `iced-x86` and compared as text. Must cover the R12/R13/RSP/RBP/SIL/DIL gotchas from §11.1.
3. **Interpreter = golden model.** It must first pass **riscv-tests** (`rv64ui/um/ua/uf/ud/uc-p-*`, later `rv64mi/si-p-*` and the `-v-` virtual-memory variants). Pass means the test writes 1 to `tohost`; `(n<<1)|1` means test n failed.
   - The `p` environment needs minimal M-mode: mhartid, mtvec, mepc, mcause, mstatus, medeleg/mideleg, mie, satp=0, pmp writes, MRET and ECALL → trap.
4. **Lockstep differential testing** (`--engine=lockstep`): run the JIT one TB at a time, run the interpreter over the same range, and compare x, f, pc, fcsr and memory writes (via a checksum or a write log). Report the first divergence with the TB's guest disassembly, IR and x86 dump.
5. **Random fuzzing (proptest):** generate random valid RV64GC straight-line sequences, with loads and stores confined to a scratch page, and random initial registers. Execute them under the interpreter and the JIT and compare. Run it in CI with a fixed seed and, on a schedule, with random seeds.
6. **Reference comparison:** in user mode, compare stdout and exit code against `qemu-riscv64`. Optionally compare commit logs against Spike (`spike --log-commits`, built from source).
7. **Programs:** static C tests (hello, printf with floats, malloc/free, qsort, setjmp/longjmp, string ops) compiled with `-O0` and `-O2`, and `-march=rv64gc` and `rv64imafd` (no C).
8. **MMU unit tests:** hand-built page tables covering superpages, misaligned superpages, A/D updates, U/SUM/MXR permutations and non-canonical addresses.
9. **Invariants in debug builds:** assert the block-boundary ABI (§8.3) with an optional `--check-abi` that verifies the pinned registers against a shadow copy.
10. **Every bug fix adds a regression test.** Every new instruction needs decoder + interpreter + JIT (lockstep) coverage.

## 22. Benchmarking methodology and targets

- **Workloads:**
  - CoreMark (EEMBC, `PORT_DIR=linux`, `-O2 -march=rv64gc -static`, iterations sized for ≥ 10 s). The run is valid only if it reports "Correct operation validated".
  - Dhrystone.
  - riscv-tests `benchmarks/` (median, qsort, towers, mm, …).
  - A Linux boot to shell (system mode).
- **Configurations**, compared side by side:
  - `interp`
  - `jit --regalloc=none --no-chain` (naive)
  - `+chain`
  - `+pinned`
  - `+linear`
  - `--mem=softmmu` (user mode)
  - `qemu-riscv64` 8.2
  - native x86-64 build of the same source (slowdown vs native)
- **Metrics:**
  - Guest MIPS = `icount / wall time`
  - Speedup vs `interp`
  - Translation time share
  - Chain hit ratio
  - Jump-cache hit ratio
  - TLB miss rate
  - Code-cache size
- **TLB microbenchmark:** a guest loop of N loads over a working set that fits in the TLB, versus one that is larger than it (forcing walks). Measure host cycles per access with `rdtsc` and report both. This is the data behind the "45 → 4 cycles" claim.
- **Procedure:**
  - `taskset -c 2`, 1 warm-up run plus 5 measured runs, report the median and min/max.
  - Record the host CPU model, commit hash, flags and date.
  - The cloud VM is noisy, so say so next to every result.
- **Results go in `docs/BENCHMARKS.md` only if the harness produced them. Never invent or extrapolate numbers.**
- Targets (hypotheses, to be confirmed or refuted by measurement):

  | Config | Target |
  |---|---|
  | interpreter (pre-decoded) | baseline, probably 50–200 MIPS |
  | naive JIT | ≥ 3× interp |
  | + chaining | ≥ 1.5× naive |
  | full JIT, user direct | ≥ 500 MIPS on CoreMark, ≥ 5–10× interp |
  | full JIT, softmmu / system | ≥ 120 MIPS (the resume claim) |
  | TLB hit | ≤ 5 cycles vs walk ≥ 40 cycles |

## 23. CLI, debugging and observability

```
bridgev run   [--mode=user] [--engine=interp|jit|lockstep] [--mem=direct|direct-checked|softmmu]
              [--no-chain] [--regalloc=none|pinned|linear] [--pin=x2,x1,x10,x15] [--max-block=N]
              [--tlb-size=256] [--code-cache=256M] [--wx=dualmap|mprotect] [--smc=eager|flush-on-fence]
              [--stats[=regs]] [--trace=insn|block] [--dump-ir] [--dump-x86] [--perf-map] [--deterministic]
              [--profile-jit] [--profile-tbs] <elf> [guest args…]
bridgev boot  --kernel Image [--firmware fw_jump.bin] [--dtb x.dtb] [--initrd rootfs.cpio]
              [--ram 512M] [--append "console=ttyS0"] [--engine …] [--stats] [--deterministic]
bridgev disasm <elf>                 # decoder + disassembler check
tools/bench.py [--suite ...] [--quick] # runs the §22 matrix; markdown + JSON (D43)
```
- Logging: the `BRIDGEV_LOG=debug|trace` environment variable.
- `--stats`: guest instructions, TBs translated, bytes of code, chain patches/unlinks, jump-cache hits/misses, TLB hits/misses, exits by reason, time split (translate vs execute), MIPS.
- `--dump-x86`: writes each TB's host bytes to a file for `objdump -D -b binary -m i386:x86-64 --adjust-vma=<rx addr>`. Tests use `iced-x86` for in-process disassembly.
- `--perf-map`: appends `"<hex start> <hex size> tb_<guest_pc>"` lines to `/tmp/perf-<pid>.map` for `perf report`.
- GDB remote stub (stretch): single-step through the interpreter, so it works regardless of the engine.

## 24. Roadmap (phases, acceptance criteria)

This table summarizes the roadmap. **The detailed, authoritative plan is `docs/ROADMAP.md`**, with per-task deliverables, tests, acceptance criteria and risks.

Each phase ends with:
- all tests green
- `cargo fmt` + `clippy -D warnings` clean
- CLAUDE.md §0 updated
- **a phase report `docs/phase-reports/phase-NN-<name>.md` written from `TEMPLATE.md`** (D19)
- everything committed and pushed

| Phase | Scope | Acceptance |
|---|---|---|
| **0 Setup** | Cargo skeleton, CI workflow, `tools/setup.sh` (apt deps), `guest/` build scripts, riscv-tests + CoreMark submodules | `cargo test` runs; `tools/build-guests.sh` produces rv64 test ELFs; CI green |
| **1 Front end + interpreter** | ELF loader, full RV64GC decoder (incl. RVC), disassembler, pre-decoded interpreter, minimal M-mode for riscv-tests `p` env, user-mode loader/stack/auxv, Phase-1 syscalls, FP via SoftFloat | all `rv64u{i,m,a,f,d,c}-p-*` pass; static glibc "hello" + printf("%f") run; decoder golden tests pass |
| **2 Naive JIT** | x86 emitter + golden tests, dual-mapped code buffer, enter/exit trampolines, dispatcher, TB cache, per-instruction lowering with all guest regs in memory, interp-helper fallback, precise exits | same suites pass under `--engine=jit`; lockstep clean; first MIPS number vs interp recorded |
| **3 Chaining** | exit slots, rel32 patching with alignment, incoming lists/unlink, budget prologue, jump cache for JALR | suites + lockstep pass; `--no-chain` vs chain speedup recorded |
| **4 IR + regalloc** | IR, lift, const-fold/forwarding/dead-writeback, liveness, pinned R12–R15, linear scan, cold stubs | fuzz + lockstep clean over ≥1e6 random blocks; per-level speedup recorded |
| **5 Milestone A** | CoreMark + Dhrystone in user mode, `tools/bench.py`, `docs/BENCHMARKS.md` | CoreMark validates under all engines; report table: interp vs JIT vs qemu vs native |
| **6 FP in JIT (F2)** | inline SSE fast paths, NaN canonicalization, MXCSR→fflags, FMA3 | rv64uf/ud pass under JIT; FP-heavy bench speedup recorded |
| **7 Privileged + SoftMMU** | M/S/U, CSRs, traps/interrupts, delegation, SV39 walker, inline TLB fast path, cold slow path, `--mem=softmmu` in user mode | `rv64mi-p-*`, `rv64si-p-*`, `rv64ui-v-*` pass under interp and JIT; TLB microbench recorded |
| **8 SMC** | code-page tracking, TLB_CODE / mprotect+SIGSEGV, TB invalidation + unlink, FENCE.I, riscv_flush_icache | dedicated SMC tests (self-patching loop, JIT-in-guest) pass in both memory backends |
| **9 Milestone B** | CLINT, PLIC, UART, syscon, built-in SBI, FDT generator, boot ROM, WFI idle, kernel/busybox build + artifact caching | Linux 6.6 boots to `/ #`; automated UART test passes; boot MIPS recorded |
| **10 Stretch** | OpenSBI boot, SV48, multithreaded user mode (clone/futex, shared cache), MT CoreMark, dynamic ELF/ld.so, guest signals, virtio-blk, return-address stack, superblocks/traces, GDB stub, SMP guest | per-feature tests |

## 25. Coding conventions

- `cargo fmt`, and `cargo clippy --all-targets -- -D warnings` before every commit. No `#[allow]` without a comment explaining it.
- `unsafe` is allowed only in `jit/code_mem.rs`, `jit/trampoline.rs`, `jit/dispatch.rs` (the call into JIT code), `mem/direct.rs`, `user/signal.rs` and FFI shims. Every `unsafe` block needs a `// SAFETY:` comment.
- ABI-visible structs are `#[repr(C)]`, and every offset the JIT uses is asserted with `const _: () = assert!(offset_of!(..) == ..)`.
- JIT-called helpers are `extern "sysv64"`, never panic, and never unwind. Release profile: `panic = "abort"`, `lto = "thin"`, `codegen-units = 1`, `debug = 1` (symbols for profiling).
- Guest addresses are `u64` newtypes: `GuestVirt`, `GuestPhys`. Host pointers are never mixed with guest addresses.
- Emitter API is typed (`Reg`, `Mem { base, index, scale, disp }`, `Cond`). No raw byte pushing outside `emit.rs`.
- Errors: `anyhow` in the CLI and loaders. Hot paths return small enums. There is no `Result` in the execute loop's hot path.
- Comments explain *why* (spec clause, encoding rule). Cite spec sections, e.g. `// Priv spec §12.3.2 (Sv39 walk step 4)`.
- Commit messages follow `area: imperative summary`, e.g. `jit: patch rel32 exits for block chaining`, and end with the session attribution lines.

## 26. Working in this repo as Claude (cloud-session workflow)

- The project is developed in **Claude Code cloud sessions on a limited budget (about $100 of credit)**. Be economical:
  - Read only the files you need.
  - Pipe long outputs through `tail`/`grep`.
  - Run targeted tests (`cargo test <name>`) before the full suite.
  - Don't re-derive decisions recorded in §3. Change them only by appending a decision-log entry, with a reason.
- **The container is ephemeral.** Commit and push after every meaningful, green step to the session's designated branch (`git push -u origin <branch>`). Uncommitted work is lost when the container is reclaimed.
- At the start of every session, run `tools/setup.sh` (once it exists) to install the apt packages from §4. It can be wired into the cloud environment's setup script or a SessionStart hook.
- Don't commit build outputs: guest ELFs, kernel images, cpio archives and `target/` go in `.gitignore`.
- Don't claim a phase is complete, or quote a performance number, without the command output that proves it.
- Work phase by phase (§24). Don't start a later phase while an earlier phase's acceptance criteria are failing.
- When a design question comes up that isn't covered here, decide it, record it in §3 (next D#), and move on. Ask the user only when the choice changes scope or goals.

## 27. Keeping this file current

- Update §0 (status) at every phase boundary.
- Append decisions to §3, and never silently rewrite them.
- When code diverges from a spec in this file (offsets, register map, encodings), update the file **in the same commit**.
- Tick the task checkboxes in `docs/ROADMAP.md` as tasks complete. When scope changes, update the roadmap in the same commit.
- **After every phase**, write the phase report (D19). It must be detailed and factual: what was built and why, a worked example, the test evidence, measured numbers, bugs found, deviations, limitations, how to reproduce, and next steps.
- Keep `docs/PROJECT_EXPLAINED.md` in sync when the architecture or the measured results change.

---

## 28. Resume bullets and interview presentation

### 28.1 Resume bullets (targets: replace the numbers with measured values from `docs/BENCHMARKS.md` before use)
- Engineered a 64-bit RISC-V to x86_64 dynamic binary translator supporting RV64IMAFD instruction extensions, executing compiled Linux binaries at over **120M instructions/sec**. *(Measured, Phase 5: CoreMark at 4.8 billion guest instructions/s in user mode, 26.5× the interpreter and 1.52× `qemu-riscv64`; Phase 7: CoreMark under `--mem=softmmu`, every access through the software TLB, runs at 2.7 billion guest instructions/s.)*
- Eliminated dispatcher context switching by developing a runtime basic-block chaining mechanism that hot-patches native branch targets directly in executable cache memory.
- Implemented an inline software TLB and SV39 virtual memory engine, reducing memory translation overhead from **45 cycles to 4 cycles** on cached hits. *(Measured, Phase 7: a TLB hit adds < 1 cycle per access in throughput and about 5–7 cycles of load-to-use latency; a miss with a full Sv39 walk costs about 59 nominal cycles (28 ns). Rewrite the bullet with these numbers.)*
- Authored a custom JIT code emitter and register allocator mapping 32 guest registers to host x86_64 registers with zero-cost spill resolution for hot execution paths.

### 28.2 Architecture walkthrough (2 minutes)
1. Load the ELF (or kernel + DTB).
2. The dispatcher looks up PC → TB. On a miss, it decodes the basic block, lifts it to IR, optimizes it, allocates registers, emits x86 into the RW view, and publishes it through the RX view.
3. `enter_jit` runs the block. Exits either return an exit code, or have been patched to jump straight into the next TB (chaining). JALR goes through an inline jump cache.
4. Memory accesses use either host base+offset (user mode) or an inline TLB tag compare with a cold-stub page-walk slow path (system mode).
5. SMC: code pages are write-protected, and a write invalidates that page's TBs and unlinks their chains.
6. Precise exceptions come from per-fault-site state maps in the cold stubs.

### 28.3 Whiteboard: translation cache layout
```
 Code buffer (memfd, dual-mapped)          RW view @ 0x7f..A000  (emit/patch)
                                           RX view @ 0x7f..B000  (execute; all rel32 math here)
 ┌───────────────────────────────────────────┐ B+0
 │ enter_jit / exit_jit trampolines           │
 │ helper address table (call [rip+d32])      │
 ├───────────────────────────────────────────┤
 │ TB#0  guest 0x10074                         │
 │   sub [rbp+BUDGET],n ; jl budget_stub       │ ← prologue (preemption)
 │   ...hot body (fall-through, no spills)...  │
 │   cmp r14,rsi ; jne rel32 ───────────────────┼──► TB#7 entry (patched, slot 1)
 │   jmp rel32 ─────────────┐                  │    (slot 0 unpatched → stub_0)
 │   cold: TLB-miss stubs, helper calls         │
 │   stub_0: pc=0x10078 ; rax=TB#0|0 ; jmp exit │◄┘
 │   stub_1: pc=0x100b4 ; rax=TB#0|1 ; jmp exit │
 ├───────────────────────────────────────────┤
 │ TB#1 ...                                    │
 ├───────────────────────────────────────────┤ ← bump pointer
 │ free … (full → flush all, generation++)     │
 └───────────────────────────────────────────┘
 Rust-side metadata:
   tb_map[(pc,flags)] → TbId        page_tbs[phys_page] → [TbId]   (SMC)
   TB{exits[2]{patch_off,target,linked}, incoming[(TbId,slot)], pcmap}
   CpuState.jmp_cache[4096]{pc,host}   (JIT-visible, JALR)
```

### 28.4 Self-modifying code answer
- Translating from a page marks it as a code page and write-protects it (a TLB_CODE flag in the softmmu write tag, or host `mprotect` read-only in direct mode).
- A guest store to that page misses the fast path (or raises SIGSEGV). The handler invalidates all TBs on the page: it removes them from the hash map and jump cache and **unpatches every incoming chained jump back to its exit stub**. Then it drops the protection and completes the store.
- If the store hit the currently executing block, we exit right after the store and retranslate.
- RISC-V formally only requires coherence after `FENCE.I`, and we honour that too. Eager invalidation makes FENCE.I cheap: it just ends the TB and flushes the jump cache.

### 28.5 Raw byte encoding of an x86 conditional branch (worked example)
- **`jne rel32`** = `0F 85` + rel32 (little-endian), 6 bytes.
  - At host address 0x…0010 targeting 0x…0200: rel = 0x200 − (0x010 + 6) = 0x1EA → **`0F 85 EA 01 00 00`**.
  - The short form `jne rel8` = `75 cb` has range −128…+127 from the next instruction.
- **`cmp r14, rsi`** (guest `bne a0, a1`, a0 pinned in R14, a1 allocated to RSI) = `REX.W|B = 0x49`, opcode `39` (CMP r/m64, r64), ModRM `11 110 110` = `0xF6` → **`49 39 F6`**.
- **Chain patch:** a `jmp rel32` at 0x…1040 targeting a TB at 0x…2000 has rel = 0x2000 − 0x1045 = 0x0FBB → **`E9 BB 0F 00 00`**.
  - The rel32 field starts at 0x1041. That is not 4-byte aligned, so in the real layout we pad 3 NOPs first, giving the E9 at 0x1043 and rel32 at 0x1044, and recompute rel = T − 0x1048.
- Condition nibble cheat-sheet: E=4, NE=5, L=C, GE=D, B=2, AE=3 (`jcc rel32 = 0F 80+cc`, `jcc rel8 = 70+cc`).

### 28.6 Likely follow-up questions (have answers ready)
- **Why does chaining beat returning to the dispatcher?**
  - Returning goes through one shared indirect `jmp`/`ret` whose target changes on every block. The BTB mispredicts, costing about 15–20 cycles each time, and the return-stack buffer gets polluted.
  - Direct `E9` jumps are statically predictable.
- **Why pin the registers in callee-saved R12–R15?** Their values survive helper calls into Rust at no cost, and they persist across chained blocks without loads or stores.
- **How are exceptions precise if registers are lazily written back?** Per-site state maps in the cold stubs, plus `pcmap` for host-RIP → guest-PC in the signal handler.
- **Why is RISC-V → x86 easier than x86 → ARM?**
  - RISC-V has no flags, so no lazy-flag emulation.
  - x86 TSO is stronger than RVWMO, so fences are mostly free.
  - x86 has fewer registers, which is the hard part: hence pinning plus linear scan.
- **Why can't a TB span pages, and why only link within a page in system mode?** Virtual→physical mappings can change without code changes (context switches, satp), so cross-page targets are re-validated through the TLB exec path.
- **What are the x86 encoding pitfalls?** R12/RSP need a SIB byte, R13/RBP need a disp8, SIL/DIL need REX, and imm32 is sign-extended.
- **Why a direct-mapped TLB?** A single compare fits in about 5 instructions with no associativity search. Conflict misses are cheap because the walk is in Rust and walks are cached. The size is a tunable, benchmarked parameter.

---

## 29. References

- RISC-V Unprivileged ISA spec (RV64I, M, A, F, D, C, Zicsr, Zifencei) and Privileged spec (M/S/U, Sv39/Sv48, CSRs, traps), latest ratified versions. RISC-V psABI (calling convention, ELF).
- RISC-V SBI specification v2.0. Linux `Documentation/arch/riscv/boot.rst`. Devicetree Specification v0.4.
- Intel® 64 and IA-32 SDM Vol. 2 (instruction encoding). AMD64 APM Vol. 3. System V AMD64 psABI.
- F. Bellard, "QEMU, a Fast and Portable Dynamic Translator", USENIX ATC 2005. QEMU TCG sources (`accel/tcg/cputlb.c`, `translate-all.c`, `tcg/i386/tcg-target.c.inc`) for comparison only; do not copy code (license).
- M. Poletto & V. Sarkar, "Linear Scan Register Allocation", TOPLAS 1999.
- Spike (riscv-isa-sim), riscv-tests, riscv-arch-test, OpenSBI, Berkeley SoftFloat 3e.
- Prior art: Rosetta 2, box64, FEX-Emu, rvemu, Dynamo (HP), DynamoRIO.
