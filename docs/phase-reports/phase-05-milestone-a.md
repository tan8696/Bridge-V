# Phase 5 report: Milestone A (benchmarks and the speedup report)

| Field | Value |
|---|---|
| Phase | 5: Milestone A (see `docs/ROADMAP.md`) |
| Status | Complete |
| Dates | 2026-09-26 → 2026-09-26 |
| Branch / final commit | `claude/compassionate-babbage-ul3prl` @ the commit that adds this report (measured code: `668723f`) |
| Sessions used | 1 |

## 1. Summary
Milestone A is met. CoreMark and Dhrystone build reproducibly for rv64gc and for native x86-64 from the same sources (`tools/build-bench.sh`). A harness (`tools/bench.py`) runs the §22 matrix: the interpreter, four JIT levels, `qemu-riscv64` and native. Every run is validated, and the harness writes markdown and JSON. `tools/demo-milestone-a.sh` reproduces the headline from a fresh clone in about a minute.

Headline, all 70 measured runs valid, same batch:
- **CoreMark:** the JIT runs at **13,498 iterations/s**. That is **26.5× the interpreter**, **1.52× `qemu-riscv64`** and 52% of native.
- **Dhrystone:** **21.9 M Dhrystones/s** (12,443 DMIPS). That is 39.6× the interpreter, 4.55× QEMU and 49% of native.

The tuning pass (P5.3) was profile-driven. A new sampling profiler (`--profile-tbs`) showed 99.6% of CoreMark time in translated code, concentrated in small loops. Two changes were tried:
- **Kept: the instruction budget moved from memory into R9 (D46).** In a same-batch A/B run, CoreMark gained +10.5% and Dhrystone +22.4%.
- **Reverted: loop-resident registers for self-looping TBs (D45).** They doubled a micro-loop but did nothing for CoreMark or Dhrystone.

## 2. Planned vs delivered
| Task ID | Task | Status | Notes |
|---|---|---|---|
| P5.1 | Benchmark builds | done | CoreMark via its Makefile. Dhrystone from riscv-tests (netlib is blocked by the network policy). Native builds. `BUILDINFO.txt` with compilers, flags, source commits and SHA-256. |
| P5.2 | Benchmark harness | done | `tools/bench.py` rather than `bridgev bench` (D43). The `bench` CLI stub is removed. |
| P5.3 | Profiling and tuning | done | `--profile-tbs` (D44). Translation-time share and code size per guest instruction are in the harness tables. Jump-cache hit rate comes from `--profile-jit`. Tuning: D46 kept, D45 reverted. |
| P5.4 | Results documentation | done | `docs/BENCHMARKS.md` (tables, method, host, noise caveat), `docs/bench/*/results.json`, `tools/demo-milestone-a.sh` (tested from a fresh GitHub clone). |

## 3. What was built

### 3.1 Benchmark builds (`tools/build-bench.sh`, `guest/bench/dhrystone/`)
- **CoreMark:**
  - Source: EEMBC CoreMark, submodule `third_party/coremark` @ `1f483d5`.
  - Build: its own Makefile, `make PORT_DIR=linux CC=<cc> XCFLAGS="<arch> -static -DPERFORMANCE_RUN=1" OPATH=<tmp>/ link`, which gives `PORT_CFLAGS=-O2`. `ITERATIONS=0` is compiled in and the count comes from argv (`0x0 0x0 0x66 <n>`), so the harness can size each run.
  - Built twice: rv64gc (`-march=rv64gc -mabi=lp64d`) and native (host gcc; both compilers are gcc 13.3.0).
- **Dhrystone:**
  - Source: `third_party/riscv-tests/benchmarks/dhrystone` @ `bcffa2b` (its version string is "C, Version 2.2"; BSD license from riscv-tests).
  - The source files are compiled **unmodified**. Their bare-metal assumptions are met by a shim directory placed first on the include path.
  - `util.h` replaces riscv-tests' bare-metal header, which `dhrystone_main.c` includes right after `dhrystone.h`. It redefines `NUMBER_OF_RUNS` (taken from argv[1] by a glibc constructor in `shim.c`), the timer (`clock_gettime(CLOCK_MONOTONIC)` µs, because the riscv-tests default reads `mcycle`, which user mode can't) and `setStats`.
  - Build details found on the way (§8): `HZ` must be `long` or `HZ * runs` overflows; `debug_printf` is renamed in `dhrystone_main.c` only (riscv-tests' copy is empty and would hide Weicker's self-check); `-DPASS2` on `dhrystone.c` avoids a duplicate `time_info` under GCC's `-fno-common`.
- Outputs: `guest/build/bench/{coremark,dhrystone}-{rv64.elf,native}` and `BUILDINFO.txt`.

### 3.2 The harness (`tools/bench.py`)
**Matrix:**
- `interp`
- `jit-naive` (`--regalloc none --no-chain`)
- `jit+chain` (`--regalloc none`)
- `jit+pinned` (`--regalloc pinned`)
- `jit+linear` (default)
- `softmmu`: shown as n/a until Phase 7
- `qemu` (`qemu-riscv64`)
- `native`

**Per cell:**
1. **Calibration:** grow the work until a run lasts ≥ 0.5 s, then size it to the target from the program's own rate. The target is 13 s for CoreMark and half that for Dhrystone.
2. **Warm-up** run. It recalibrates the work size, because the short calibration run underestimates the steady rate (cold block decoding). This was a real failure, §8.
3. **5 measured runs**, `taskset -c 2`.
4. **Validation of every run:**
   - **CoreMark:** its own CRC checks must pass ("ERROR!" lines), and "Correct operation validated" must appear, which also requires ≥ 10 s. A run that fails only the 10 s rule is kept in the JSON as discarded and redone with 25% more work, at most 3 times. The mechanism was exercised deliberately; the final batch needed no redos.
   - **Dhrystone:** every "value / should be" pair of Weicker's final-value check must match (20 values). `Arr_2_Glob[8][7]` must equal runs + 10.
5. **Records:** the score and bridgev's `--stats` (guest instructions, MIPS, translation time, host bytes per guest instruction, dispatcher entries).

**Output:**
- `results.json`: all raw runs, host info (CPU model, kernel, rustc, QEMU, commit and whether the tree is dirty, date, `BUILDINFO.txt`).
- `results.md`: tables with "vs interp", "vs native", DMIPS, guest MIPS (QEMU's is estimated from bridgev's instructions per unit of work), and valid-run counts.
- Exit status 1 if any cell has an invalid run.
- `--quick` gives a 23-second smoke test with CoreMark's 10 s rule off (the output says it is not a measurement).

### 3.3 Sampling profiler (`--profile-tbs`, D44)
- **Sampling:** `setitimer(ITIMER_PROF)` fires SIGPROF on process CPU time. The handler in `src/user/signal.rs` only does atomic stores of the interrupted RIP into a 2^17-entry static buffer, so it is async-signal-safe. `SA_RESTART` keeps guest syscalls working.
- **Attribution** at the end of the run (`--stats`): each sample is assigned to a TB via `TbCache::find_host`, to the trampolines, or to "elsewhere" (dispatcher, translator, helpers, kernel), and the 12 hottest TBs are listed.
- **Rate:** 1 kHz is requested; the kernel delivers at its tick rate, about 250 Hz here (2,735 samples in a 150,000-iteration CoreMark run).

### 3.4 Budget in R9 (D46)
- **What changed:** the IR back end (`pinned`, `linear`) keeps the D12/D30 instruction budget in R9.
  - Prologue `sub r9, n; jl budget_stub`; stubs `add r9, refund`.
  - `enter_jit` loads R9 from `CpuState.budget` and `exit_jit` stores it back, so the dispatcher, the fault path (`fault_exit` → `exit_jit`) and lockstep all see `CpuState.budget` as before.
  - A helper call (`Interp`) stores R9 before the call, because `helper_interp_one` syncs icount from `CpuState.budget`, and reloads it after.
  - R9 leaves the allocation pool (6 registers). The naive back end (`none`) keeps the memory budget, and `Trampolines::generate` takes the budget register as a parameter.
- **Why:** the old `sub qword [rbp+88h], n` was a read-modify-write of the same memory word in every TB prologue. That makes one long store-to-load dependency chain through all of execution.

### 3.5 Other changes
- **Fixed-register ops** (MULH/DIV/REM, and shifts without BMI2) copy their operands straight into R10/R11/RCX (`Alloc::copy_to`) instead of first allocating pool registers for them.
- **Pinned-value handoff.** After `mv s0, a4` with both registers pinned, overwriting a4 no longer copies the old value to a pool register, because s0's pinned register still holds it (`write_pinned`).
- **Fuzzer:**
  - One generated block in four now loops to its own start (a branch or JAL to CODE).
  - Every block runs twice per configuration: once for one TB execution, once with its self-targeting exits linked (`Jit::link_self_exits`) and a 3-execution budget.
  - The reference is the interpreter repeating the block while control returns to CODE. This covers chained self-loops and jump-cache re-entries.

## 4. Design decisions made (appended to CLAUDE.md §3)
- **D43: benchmark builds and a Python harness.** Alternatives: a Rust `bridgev bench` subcommand (more code for process control and statistics, and benchmark logic inside the translator binary); vendoring Dhrystone from netlib (unreachable, and it would duplicate code already in a submodule).
- **D44: `--profile-tbs`.** `perf` isn't installed, and this needs no change to the generated code.
- **D45: loop-resident registers.** Tried and **not adopted** (§7.3).
- **D46: budget in R9.** Kept (§7.3). Amends D30/§13.3 for the IR back end. CLAUDE.md §8.2 and §8.4 are updated.

## 5. How it works: worked example
**One harness cell** (`coremark` × `jit+linear` in the final batch):
1. **Calibration:** 20 iterations are too fast to time, so the harness goes ×100, then grows until a run lasts ≥ 0.5 s. The program's rate gives n for 13 s.
2. **Warm-up:** the long warm-up run reports its rate, and n is recomputed to 174,070 iterations.
3. **Measured runs:** five runs follow, each under `taskset -c 2`. Each reported its "Iterations/Sec" (13,445–13,872), "Correct operation validated" and a total time over 10 s, plus bridgev's `--stats` line.
4. **Summary:** median 13,498 it/s. Guest MIPS (median) is 4,839, and translation was 9.2 ms of a 12.9 s run (0.071%).

**The D46 change on CoreMark's hottest TB** (list reversal in `core_bench_list`, 12.7% of samples):
```
before (3099969)                              after (668723f)
sub  qword ptr [rbp+88h],5   ; budget RMW     sub  r9,5          ; budget in a register
jl   budget_stub                              jl   budget_stub
mov  rax,[rbp-10h]           ; a4             mov  rax,[rbp-10h]
mov  rcx,[rbx+rax]           ; ld a4,0(s0)    mov  rcx,[rbx+rax]
mov  rdx,[rbp-18h]           ; a3             mov  rdx,[rbp-18h]
mov  [rbx+rax],rdx           ; sd a3,0(s0)    mov  [rbx+rax],rdx
mov  [rbp-40h],rax           ; s0 write-back  mov  [rbp-40h],rax
mov  [rbp-18h],rax           ; a3 write-back  mov  [rbp-18h],rax
mov  [rbp-10h],rcx           ; a4 write-back  mov  [rbp-10h],rcx
test rcx,rcx                 ; bnez a4        test rcx,rcx
jne  <this TB, chained>                       jne  <this TB, chained>
```
The body is identical. What disappears is the memory read-modify-write that every TB execution chained on its predecessor's store (this loop re-enters itself once per list node).

## 6. Tests and verification
**Inventory.** 104 tests (Phase 4: 103). New or extended:
- `tests/cli.rs::profile_tbs_attributes_samples`.
- `tests/jit_lowering.rs::infinite_chained_loop_is_preempted` now runs at every regalloc level. The IR levels use the register budget, and preemption and exact icount are checked at slices 1/7/1000/100000.
- `tests/fuzz_blocks.rs`: self-loop blocks and the 3-execution mode (§3.5).
- `tests/common/rvgen.rs`: the self-loop block generator, which `ir_passes` also uses.

**Commands and results** (final code):
```
$ cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo clippy --all-targets --features disasm -- -D warnings
(clean)
$ PROPTEST_RNG_SEED=20260926 BRIDGEV_REQUIRE_GUESTS=1 cargo test          # debug, exactly as CI
test result: ok. 37 (lib) · 12 (cli) · 2 · 3 · 23 · 1 (fuzz_blocks) · 1 (ir_passes) · 9 (jit_lowering) · 8 (riscv_tests) · 8 (user_programs); 0 failed
$ PROPTEST_CASES=1000000 target/release/deps/fuzz_blocks-*
test result: ok. 1 passed; 0 failed; ... finished in 330.25s
$ python3 tools/bench.py                  # final batch, 70 measured runs
(tables in §7; exit status 0 = every run valid)
$ git clone --recurse-submodules -b claude/compassionate-babbage-ul3prl https://github.com/tan8696/Bridge-V clean
$ cd clean && tools/demo-milestone-a.sh
CoreMark (validated): interpreter 516.6 it/s, JIT 13,351.7 it/s -> JIT speedup 25.8x over the interpreter
real 0m54.490s
```
The 10⁶-block fuzz run covers 5 configurations × 2 modes: 10⁷ JIT executions checked against the interpreter.

**Mutation checks for the loop-resident implementation, before it was reverted.** Omitting the resident stores on the loop exit was caught within 3,000 blocks. Charging one instruction too few on the back-edge was also caught. Both failures reproduced in "1 execution" mode, and the self-loop blocks drove them.

**Acceptance criteria:**
- ✅ CoreMark validates under every engine and configuration: 5/5 runs in each of the 7 cells.
- ✅ `docs/BENCHMARKS.md` has the full table.
- ✅ The demo script works from a clean checkout (above; `tools/setup.sh` packages were already installed in this container).

## 7. Performance
Final batch: commit `668723f`, 2026-09-26, Intel Xeon @ 2.10 GHz (4 vCPU shared cloud VM, pinned to CPU 2), Linux 6.18, rustc 1.94.1, QEMU 8.2.2. Method in §3.2. Raw data is in `docs/bench/2026-09-26-668723f/results.json`.

### 7.1 Results
Host: Intel(R) Xeon(R) Processor @ 2.10GHz (4 vCPU, pinned to CPU 2), kernel 6.18.44-fc-v37, rustc 1.94.1 (e408947bf 2026-03-25), qemu-riscv64 version 8.2.2 (Debian 1:8.2.2+ds-0ubuntu1.18). Commit `668723f`, 2026-09-26T18:43:22+00:00. 5 measured run(s) after 1 warm-up, median (min–max). Shared cloud VM: expect noise of several percent.

**CoreMark**

| config | iterations/s | vs interp | vs native | guest MIPS | iterations/run | valid |
|---|---:|---:|---:|---:|---:|---:|
| interp | 510 (508–520) | 1.0× | 0.020 | 183 | 6,732 | 5/5 |
| jit-naive | 791 (752–817) | 1.6× | 0.030 | 284 | 9,857 | 5/5 |
| jit+chain | 7,024 (6,674–7,251) | 13.8× | 0.271 | 2,519 | 86,775 | 5/5 |
| jit+pinned | 10,168 (9,987–10,242) | 19.9× | 0.392 | 3,646 | 122,604 | 5/5 |
| jit+linear | 13,498 (13,445–13,872) | 26.5× | 0.520 | 4,839 | 174,070 | 5/5 |
| qemu | 8,898 (8,736–8,936) | 17.5× | 0.343 | 3,193 (est.) | 117,476 | 5/5 |
| native | 25,962 (24,956–26,570) | 50.9× | 1.000 | — | 341,472 | 5/5 |
| softmmu | n/a: Phase 7 (--mem=softmmu) |  |  |  |  |  |

| config | translate time (share of run) | host bytes / guest insn | dispatcher entries per M insns |
|---|---:|---:|---:|
| jit-naive | 2.7 ms (0.022%) | 31.3 | 180,762 |
| jit+chain | 2.5 ms (0.020%) | 31.3 | 10 |
| jit+pinned | 5.3 ms (0.044%) | 28.0 | 10 |
| jit+linear | 9.2 ms (0.071%) | 25.0 | 10 |

**Dhrystone**

| config | Dhrystones/s | vs interp | vs native | DMIPS | guest MIPS | runs/run | valid |
|---|---:|---:|---:|---:|---:|---:|---:|
| interp | 552,303 (549,409–569,102) | 1.0× | 0.012 | 314 | 186 | 3,805,165 | 5/5 |
| jit-naive | 856,901 (809,934–865,922) | 1.6× | 0.019 | 488 | 288 | 5,812,469 | 5/5 |
| jit+chain | 10,796,150 (10,763,270–11,083,702) | 19.5× | 0.241 | 6,145 | 3,625 | 70,384,470 | 5/5 |
| jit+pinned | 16,382,707 (15,676,967–17,150,622) | 29.7× | 0.365 | 9,324 | 5,499 | 105,228,260 | 5/5 |
| jit+linear | 21,861,687 (21,627,933–22,678,936) | 39.6× | 0.487 | 12,443 | 7,337 | 135,786,502 | 5/5 |
| qemu | 4,803,021 (4,612,027–4,855,974) | 8.7× | 0.107 | 2,734 | 1,614 (est.) | 32,575,524 | 5/5 |
| native | 44,867,922 (41,737,267–47,248,605) | 81.2× | 1.000 | 25,537 | — | 296,767,166 | 5/5 |
| softmmu | n/a: Phase 7 (--mem=softmmu) |  |  |  |  |  |  |

| config | translate time (share of run) | host bytes / guest insn | dispatcher entries per M insns |
|---|---:|---:|---:|
| jit-naive | 1.8 ms (0.027%) | 30.9 | 175,606 |
| jit+chain | 1.8 ms (0.028%) | 30.8 | 10 |
| jit+pinned | 3.8 ms (0.059%) | 27.3 | 10 |
| jit+linear | 5.5 ms (0.088%) | 24.4 | 10 |

### 7.2 Analysis: where time goes and why each level helps
- **Translation is negligible.** It takes 9.2 ms in a 13 s CoreMark run (0.07%), and less than 0.11% for every JIT configuration on both workloads. Dispatcher entries drop from 180,762 per million guest instructions (`jit-naive`) to 10 once chaining is on. `--profile-tbs` puts 99.6% (CoreMark) and 99.7% (Dhrystone) of all samples in translated code. Improving performance therefore means better code, not a faster translator.
- **`jit-naive` → `jit+chain` (8.9× on CoreMark, 12.6× on Dhrystone).** Returning to the Rust dispatcher after every TB (5.6 guest instructions per TB on average, statically, for CoreMark) costs more than the translated code itself. Chaining and the jump cache remove it. The jump-cache hit rate (`--profile-jit`) is 100.00% to two decimals on both workloads: 320.6 M JALRs in CoreMark, 1.68 G in Dhrystone.
- **`jit+chain` → `jit+pinned` (1.45× / 1.52×).** sp, ra, a0 and a5 stay in R12–R15 across all TBs. The IR drops x0 reads and the Phase 3 per-instruction RAX shuffling. The budget moves to R9 (D46).
- **`jit+pinned` → `jit+linear` (1.33× / 1.33×).** Constant folding and dead-write removal, and each other guest register loaded and stored at most once per TB. Host code shrinks from 28.0 to 25.0 bytes per guest instruction.
- **Remaining gap to native (1.9× on CoreMark, 2.1× on Dhrystone):**
  - Guest registers outside the pinned four live in `CpuState` between TBs. In the list-reversal loop above, a4/a3/s0 are loaded and stored every iteration.
  - Each TB pays the budget check.
  - Code generated from RISC-V instruction sequences is less dense than code compiled for x86 from the same C source.
  - Returns go through the jump cache (an indirect `jmp`) instead of `ret`, so the host's return-stack predictor is unused.
- **Hottest code** (`--profile-tbs`, `jit+linear`):

  | CoreMark TB | share | insns | function | | Dhrystone TB | share | insns | function |
  |---|---:|---:|---|---|---|---:|---:|---|
  | 0x10fce | 12.7% | 5 | core_bench_list (list reversal) | | 0x10736 | 10.2% | 8 | main (loop) |
  | 0x115dc | 7.1% | 13 | matrix_mul_matrix | | 0x10d40 | 8.5% | 28 | Proc_8 |
  | 0x11654 | 7.1% | 18 | matrix_mul_matrix_bitextract | | 0x10d38 | 6.7% | 4 | Proc_7 |
  | 0x10fc0 | 4.5% | 3 | core_bench_list | | 0x10c5c | 6.2% | 30 | Proc_1 |
  | 0x1111e | 4.5% | 3 | core_bench_list | | 0x10c38 | 5.2% | 6 | Proc_2 |
  | 0x120fe | 3.2% | 12 | crc16 | | 0x1c170 | 4.6% | 9 | strcmp (glibc) |
  | 0x120d0 | 3.0% | 12 | crc16 | | 0x1068e | 4.3% | 8 | main |
  | 0x12010 | 2.2% | 12 | crcu32 | | 0x10d8c | 4.3% | 3 | Func_1 |

### 7.3 Tuning experiments (P5.3)
All A/B runs used `taskset -c 2`, 1 warm-up + 5 runs, CoreMark at 150,000 iterations, Dhrystone at 10⁸ runs, every CoreMark run validated. Median (min–max):

| Experiment | CoreMark it/s | Dhrystone/s | Verdict |
|---|---|---|---|
| **A. Upper bound: no budget check at all** (measurement hack, never committed; 6 runs each, same batch) | 12,738 → **15,115** (+18.7%) | — | the budget RMW is worth attacking |
| **B. Budget in R9 (D46)** vs `3099969` (built in a worktree), interleaved in one batch | 12,873 (12,615–13,003) → **14,223** (13,553–14,607): **+10.5%** | 17.73 M (17.37–18.59) → **21.70 M** (19.95–22.79): **+22.4%** | **kept** |
| **C1. Loop-resident registers, first layout** (back-edge = `jne` + `jmp`, two taken branches) | 12,577 → 12,502 (−0.6%) | 18.79 M → 17.78 M (−5.4%) | no win |
| **C2. Loop-resident registers, one taken branch per iteration** | 12,847 → 12,726 (−0.9%) | 18.83 M → 19.56 M (+3.8%) | no win (inside noise) |
| **C3. Loop-resident registers on top of B** | 14,223 → 14,172 (−0.4%) | 21.70 M → 21.39 M (−1.4%) | **reverted (D45)** |
| (C on `loop.elf`: `addi t1,t1,1; bne t1,t0`) | 3,170 → 6,225 MIPS (1.96×) | | works for tiny loops |

Why C did not help, from the profile:
- **The data:** its target loops (list reversal, matrix inner loops, CRC) made up 34% of CoreMark samples. After the change, their share did not fall; with the first layout the list loop measured 10.6% without it and 13.5% with it on otherwise equal runs.
- **Likely cause (not proven):** loads of a just-stored `CpuState` slot are cheap on this CPU (store-to-load forwarding hides most of the round trip), while residency shrinks the allocation pool and adds entry and exit code.
- **Why B helped:** the budget is different. It is a read-modify-write of a single word in *every* TB, a serial dependency chain that forwarding can't hide.
- **What was kept from the C work:** the fuzzer's self-loop coverage, the pinned-value handoff and `copy_to`.

### 7.4 Honest comparison with QEMU
- **Where Bridge-V wins:** it is faster than `qemu-riscv64` on both workloads, 1.52× on CoreMark and 4.55× on Dhrystone.
- **Why the Dhrystone lead is so large** (my reading of QEMU's design, not something I measured inside QEMU): QEMU resolves each indirect jump with a call into its C helper `lookup_tb_ptr`, while Bridge-V's inline jump cache is a handful of instructions (§13.4). Dhrystone is dominated by calls and returns (1.68 G JALRs for 1.2·10⁸ runs, 14 per run).
- **This is not an apples-to-apples comparison of translators.** QEMU does much more that Bridge-V doesn't yet:
  - precise self-modifying-code handling (Bridge-V: Phase 8)
  - multi-threaded guests
  - guest signals
  - a complete syscall layer
  - FP instructions translated inline (Bridge-V's go through helpers until Phase 6; both workloads are integer-only)
- **Noise:** QEMU was the noisiest cell. One batch saw it range from 7,158 to 9,485 it/s, and earlier batches measured 9,562 and 9,676. The table's 8,898 (8,736–8,936) is from the final batch.

## 8. Bugs found and fixed
| Symptom | Root cause | Fix | Regression test / evidence |
|---|---|---|---|
| Dhrystones/s negative | `HZ * Number_Of_Runs` computed in `int` (the riscv-tests code assumes a small HZ) | `#define HZ 1000000L` in the shim | outputs compared with native/QEMU; harness checks score > 0 |
| No self-check output from Dhrystone | riscv-tests' `debug_printf` in `dhrystone.c` is an empty function | `-Ddebug_printf=bridgev_dhry_printf` for `dhrystone_main.c` only | harness requires ≥ 15 checked values |
| Native Dhrystone link error `multiple definition of time_info` | `dhrystone.h` defines it in every file unless `PASS2`; GCC 10+ defaults to `-fno-common` | `-DPASS2` on `dhrystone.c` | build |
| Harness: every quick CoreMark run "invalid" | CoreMark's 10 s rule counts as an error and suppresses "Correct operation validated" | validate via CoreMark's own CRC error lines; require the line only when the 10 s rule applies | `--quick` smoke run |
| Harness: native calibration failed | 20 iterations took 0 ticks and printed no rate | ×100 and retry on a zero-time run | `--quick` |
| Harness: interpreter CoreMark runs at 9.7 s (invalid) | the 0.5 s calibration run underestimates the steady rate (cold block decoding) | recalibrate from the full-length warm-up run | final batch: all cells valid |
| Harness: 2 of 5 QEMU CoreMark runs at 9.5 s | VM speed changed mid-batch by ±15% | 13 s target; a run failing only the 10 s rule is recorded as discarded and redone with 25% more work | deliberate 9.5 s target test (redo recorded in JSON) |
| (during D45) `regalloc: allocatable register` panic in `muldiv` | with 4 resident registers the pool was RAX/RCX/RDX, and `muldiv` allocated its operands avoiding RAX/RDX | operands copied straight to R10/R11 (`copy_to`); kept after the revert | `icount_is_exact_under_every_engine` found it |
| (process) my own shell killed | `pkill -f tools/bench.py` matched the invoking shell's command line (the same pitfall as Phase 2) | kill by PID or `pgrep` with bracketed patterns | — |
| (process) a clippy failure was pushed (`3010642`) | the commit chain checked `grep -c` output instead of failing on warnings | fixed in `3099969`; clippy now runs as its own step before commits | CI |

## 9. Deviations from the plan / spec
- **Dhrystone version.** The roadmap says "Dhrystone 2.1"; the available source identifies itself as "C, Version 2.2" (riscv-tests). netlib is blocked by the container's network policy.
- **The harness is `tools/bench.py`, not `bridgev bench`** (D43). CLAUDE.md §23 is updated.
- **"Top TBs by execution count"** became top TBs by sampled *time* (`--profile-tbs`). Time is the quantity that matters for tuning, and execution counters would change the generated code.
- **riscv-tests `benchmarks/`** (median, qsort, towers, mm, …; listed in §22 as workloads) are not in the matrix. They are bare-metal programs that print through HTIF syscalls, which the `--mode bare` harness does not implement. Deferred.
- **Chain hit ratio and TLB miss rate** (§22 metrics) are not separate columns. Chain effectiveness shows as dispatcher entries per M instructions (10). There is no TLB until Phase 7.
- **CoreMark is not built with `ITERATIONS=`.** The count is a run-time argument (CoreMark supports it), so one binary serves every configuration.

## 10. Known limitations and technical debt
- **The VM is noisy.** Same-batch comparisons are reliable to a few percent; cross-batch ones are not (QEMU moved ±15% within one batch).
- **The pinned set is still the Phase 1 default** (x2, x1, x10, x15; D42). A cross-block liveness profile might find a better set.
- **Indirect jumps don't use the host's return-address predictor.** A return-address stack (stretch goal) would help call-heavy code such as Dhrystone.
- **FP benchmarks aren't in the matrix yet.** They arrive with Phase 6 (inline FP).
- **The demo script assumes the `tools/setup.sh` packages** (cross gcc, QEMU for `--all`).

## 11. How to reproduce
```
tools/setup.sh
cargo build --release
tools/build-bench.sh                      # guest/build/bench/*, BUILDINFO.txt
python3 tools/bench.py                    # about 17 min; writes target/bench/results.{md,json}
tools/demo-milestone-a.sh                 # about 1 min after the build: CoreMark interp vs JIT
target/release/bridgev run --engine jit --profile-tbs --stats guest/build/bench/coremark-rv64.elf 0x0 0x0 0x66 150000
target/release/bridgev run --engine jit --profile-jit --stats guest/build/bench/dhrystone-rv64.elf 120000000
# Budget A/B: build 3099969 in a worktree (symlink third_party/berkeley-softfloat-3), then run both
# binaries interleaved with taskset -c 2 on the same CoreMark/Dhrystone arguments.
```

## 12. Next steps
Phase 6, FP in the JIT:
- **P6.1:** FP registers in XMM.
- **P6.2:** inline RNE add/sub/mul/div/sqrt with NaN canonicalization, and FMA3.
- **P6.3:** fflags via MXCSR.

It needs an FP-heavy benchmark added to this harness (for example the FP parts of riscv-tests' `benchmarks/` once HTIF printing works, or a small FP C kernel), so that the speedup can be measured the same way.
