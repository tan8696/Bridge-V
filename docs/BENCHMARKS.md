# Bridge-V benchmarks

Only numbers produced by the benchmark harness (`tools/bench.py`, P5.2) appear here, each with its commit, host and date. Ad-hoc numbers from earlier phases are in the phase reports.

## Milestone A: CoreMark and Dhrystone, user mode (2026-09-26, commit `668723f`)

Raw data (every run, host info, build hashes): [`bench/2026-09-26-668723f/results.json`](bench/2026-09-26-668723f/results.json).

Host: Intel(R) Xeon(R) Processor @ 2.10GHz (4 vCPU, pinned to CPU 2), kernel 6.18.44-fc-v37, rustc 1.94.1 (e408947bf 2026-03-25), qemu-riscv64 version 8.2.2 (Debian 1:8.2.2+ds-0ubuntu1.18). Commit `668723f`, 2026-09-26T18:43:22+00:00. 5 measured run(s) after 1 warm-up, median (min–max). Shared cloud VM: expect noise of several percent.

### CoreMark

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

### Dhrystone

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

**Headline:**
- **CoreMark:** Bridge-V's full JIT (`jit+linear`) reaches **13,498 iterations/s**. That is 26.5× the interpreter, **1.52× `qemu-riscv64`** and 52% of the same source compiled natively for x86-64.
- **Dhrystone:** 21.9 M Dhrystones/s (12,443 DMIPS). That is 39.6× the interpreter, 4.55× QEMU and 49% of native.

## FP benchmark (Phase 6, 2026-09-26, commit `b312367` + `--no-inline-fp`)

Raw data: [`bench/2026-09-26-fp-b312367/results.json`](bench/2026-09-26-fp-b312367/results.json). Same host, method and noise caveat as above. The binary was `b312367` plus the `--no-inline-fp` switch and a stats fix, which were committed right after the run (the JSON records a dirty tree).

Workload: `guest/bench/fp/fpbench.c`. One unit is:
- 1000 nbody steps (double: add, sub, mul, div, sqrt, FMA)
- a 24×24 single-precision sgemm with row norms
- 4096 int↔FP conversions

Every unit is validated against integer or published references, and every run printed "FP validated". Runs were sized for about 6.5 s. `jit-helper-fp` is the full JIT with `--no-inline-fp`: every FP instruction goes through the interpreter helper, which is the pre-Phase-6 behaviour.

| config | units/s | vs interp | vs native | guest MIPS | units/run | valid |
|---|---:|---:|---:|---:|---:|---:|
| interp | 130 (124–134) | 1.0× | 0.008 | 91 | 863 | 5/5 |
| jit-naive | 55 (54–58) | 0.4× | 0.003 | 38 | 356 | 5/5 |
| jit+chain | 58 (56–61) | 0.4× | 0.003 | 41 | 368 | 5/5 |
| jit+pinned | 3,686 (3,629–3,919) | 28.4× | 0.215 | 2,584 | 23,804 | 5/5 |
| jit+linear | 3,508 (3,469–3,654) | 27.1× | 0.205 | 2,458 | 22,531 | 5/5 |
| jit-helper-fp | 58 (55–59) | 0.4× | 0.003 | 40 | 376 | 5/5 |
| qemu | 629 (606–687) | 4.9× | 0.037 | 442 (est.) | 4,175 | 5/5 |
| native | 17,107 (16,920–17,700) | 132.0× | 1.000 | — | 112,757 | 5/5 |
| softmmu | n/a: Phase 7 (--mem=softmmu) |  |  |  |  |  |

**Headline:**
- Inline FP (D47) makes the full JIT **60× faster than the same JIT with FP through the helper** (3,508 vs 58 units/s).
- It is **5.6× faster than `qemu-riscv64`** and runs at 21% of native.
- The Phase 2/3 back end (`jit-naive`, `jit+chain`) never inlines FP and is slower than the interpreter on this workload. See `docs/phase-reports/phase-06-fp-jit.md` §7.

**Configurations** (CLAUDE.md §22):

| config | what it is |
|---|---|
| `interp` | the reference interpreter (pre-decoded blocks) |
| `jit-naive` | `--regalloc none --no-chain`: the Phase 2 translator, every TB returns to the dispatcher |
| `jit+chain` | `--regalloc none`: + block chaining and the jump cache (Phase 3) |
| `jit+pinned` | `--regalloc pinned`: + IR, four guest registers pinned to R12–R15 (Phase 4), budget in R9 (D46, Phase 5) |
| `jit+linear` | default: + optimizer passes and the linear-scan allocator with lazy write-back (Phase 4) |
| `jit-helper-fp` | `--no-inline-fp`: the full JIT with every FP instruction through the interpreter helper (fpbench only) |
| `softmmu` | not implemented yet (Phase 7) |
| `qemu` | `qemu-riscv64` 8.2.2 (Ubuntu), same guest binary |
| `native` | the same C sources compiled with the host gcc 13.3 for x86-64 (`-O2 -static`) |

**Workloads** (`tools/build-bench.sh`, D43):
- **CoreMark:** EEMBC CoreMark (submodule `third_party/coremark`), `make PORT_DIR=linux`, `-O2 -static -DPERFORMANCE_RUN=1`, rv64gc. Iterations are sized per configuration so a run lasts about 13 s (≥ 10 s is required for a valid score).
- **Dhrystone:** "C, Version 2.2" from `third_party/riscv-tests/benchmarks/dhrystone` (unmodified), built with a Linux shim (`guest/bench/dhrystone`), `-O2 -static`, runs sized for about 6.5 s. DMIPS = Dhrystones/s ÷ 1757.
- Compiler: gcc 13.3.0 for both the guest (`riscv64-linux-gnu-gcc`) and native builds.

**Method:**
- Each cell runs with `taskset -c 2`: 1 calibration run, 1 warm-up run (which recalibrates the work size), then 5 measured runs. The tables give the median (min–max).
- Every measured run is validated:
  - CoreMark must print "Correct operation validated" and run ≥ 10 s. A run that falls under 10 s is kept in the JSON as discarded and redone with 25% more work; none were needed in this run.
  - Dhrystone must match every "should be" value of Weicker's self-check.
- Scores are the programs' own: CoreMark iterations/s, and Dhrystones/s over the timed loop.
- Guest MIPS for bridgev comes from `--stats` (exact instruction count divided by wall time, including startup). For QEMU it is estimated from bridgev's instructions per iteration.

**Host:** Intel Xeon @ 2.10 GHz (4 vCPU cloud VM, shared), Linux 6.18, rustc 1.94.1. **This is a noisy shared VM:** run-to-run spread is several percent, and one earlier batch saw QEMU vary by ±15%. Compare configurations within one batch, not across batches.

**Reproduce:** `tools/setup.sh && cargo build --release && tools/build-bench.sh && python3 tools/bench.py`. `tools/demo-milestone-a.sh` does the interpreter-vs-JIT CoreMark subset (about 1 minute).

## Earlier harness runs
- [`bench/2026-09-26-3099969-baseline.md`](bench/2026-09-26-3099969-baseline.md): the same matrix before the Phase 5 tuning (budget in memory). `jit+linear` CoreMark 13,041, Dhrystone 18.4 M. The before/after of the tuning itself was measured in a same-batch A/B run: Phase 5 report §7.3.

## Rules (CLAUDE.md §22)
- Only numbers produced by the benchmark harness go in this file. Each entry records the exact command, commit, host CPU, compiler flags and date.
- Method: `taskset -c 2`, then 1 warm-up run and 5 measured runs. Report the median, min and max.
- The development machine is a shared cloud VM, so treat results as noisy.
- Targets in CLAUDE.md §22 are hypotheses. They never appear here as results.
