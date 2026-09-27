# Bridge-V benchmarks

Only numbers produced by the benchmark harness (`tools/bench.py`, P5.2) appear here, each with its commit, host and date. Ad-hoc numbers from earlier phases are in the phase reports.

## Tiered translation (Phase 12, D63, 2026-09-27, commits `53f9030` and `2e498ee`)

`tools/boot-bench.py` and `tools/bench.py`, run by the `bench` workflow on a GitHub-hosted runner, because this phase had no Linux machine. Raw data and GitHub Actions run numbers: [`bench/2026-09-27-tier/`](bench/2026-09-27-tier/). Analysis: [`phase-reports/phase-12-tiered-translation.md`](phase-reports/phase-12-tiered-translation.md) §7.

Host: AMD EPYC 7763 (4 vCPU, pinned to CPU 2), shared cloud VM. **A different host from the Phase 11 results below; compare only within these tables.** Boots: configurations take turns run by run, 1 warm-up each, median (min–max).

**Linux boot to a BusyBox shell, by `--tier`** (`2e498ee`, 7 runs; the default is 32):

| config | time to shell | TBs translated | host code | translate time |
|---|---:|---:|---:|---:|
| jit `--tier 0` (before Phase 12) | 1.30 s (1.28–1.50) | 65,634 | 44,501 KiB | 560.7 ms |
| jit `--tier 16` | 1.07 s (1.05–1.08) | 23,163 | 16,219 KiB | 196.8 ms |
| **jit `--tier 32` (default)** | **1.06 s (1.05–1.09)** | **18,023** | **12,631 KiB** | **154.2 ms** |
| jit `--tier 64` | 1.09 s (1.07–1.10) | 13,391 | 9,310 KiB | 118.9 ms |
| qemu-system-riscv64 8.2.2 | 1.41 s (1.37–1.43) | | | |

The wider sweep at `53f9030` (5 runs): tier 0 1.32 s, 1 1.20 s, 4 1.16 s, 16 1.11 s, 64 1.11 s, 256 1.32 s; QEMU 1.41 s. In a fully interpreted boot (`--tier 1000000000`), 31% of the 65.7 k blocks ran once and 64% fewer than 16 times.

**User mode** (`2e498ee`, `--configs jit+linear,jit-tier0 --runs 3`, 1 warm-up):

| workload | default (tier 32) | `--tier 0` | translate time |
|---|---:|---:|---:|
| CoreMark (it/s) | 11,847 (11,794–11,858) | 11,833 (11,831–11,843) | 2.1 ms vs 10.3 ms |
| Dhrystone (/s) | 16,710,005 (16,675,336–16,723,168) | 16,524,448 (16,242,056–16,543,770) | 1.2 ms vs 8.5 ms |
| fpbench (units/s) | 2,079 (2,072–2,101) | 2,105 (2,105–2,105) | 1.4 ms vs 9.0 ms |

The user-mode differences (+0.1%, +1.1%, −1.2%) are within the runner's noise between back-to-back configurations.

## Final results (Phase 11, 2026-09-27, commit `567255a`)

`python3 tools/bench.py --suite coremark,dhrystone,fpbench --configs interp,jit+linear,softmmu,qemu,native` at the Phase 10 commit. Raw data: [`bench/2026-09-27-567255a-final/`](bench/2026-09-27-567255a-final/).

Host: Intel(R) Xeon(R) Processor @ 2.10GHz (4 vCPU, pinned to CPU 2), kernel 6.18.44-fc-v37, rustc 1.94.1, qemu-riscv64 8.2.2. 5 measured runs after 1 warm-up, median (min–max). Shared cloud VM: expect noise of several percent.

| workload | interp | **jit** (`jit+linear`) | softmmu | qemu-riscv64 | native x86-64 |
|---|---:|---:|---:|---:|---:|
| CoreMark (iterations/s) | 454 (436–470) | **13,409** (12,854–13,746) | 7,526 (7,303–7,620) | 8,988 (8,652–9,336) | 25,531 (25,354–25,617) |
| Dhrystone (Dhrystones/s) | 472,238 | **20,728,869** (11,798 DMIPS) | 10,115,192 | 4,758,387 | 45,562,668 |
| fpbench (units/s) | 118 (115–122) | **3,711** (3,659–3,873) | 2,654 (2,527–2,752) | 636 (611–644) | 16,589 (16,278–17,379) |

| workload | jit guest MIPS | jit vs QEMU | jit vs native | softmmu vs QEMU | softmmu vs jit (direct) | jit vs interp |
|---|---:|---:|---:|---:|---:|---:|
| CoreMark | 4,807 | 1.49× | 0.525 | 0.84× | 0.56 | 29.5× |
| Dhrystone | 6,955 | 4.36× | 0.455 | 2.13× | 0.49 | 43.9× |
| fpbench | 2,600 | 5.83× | 0.224 | 4.17× | 0.72 | 31.5× |

- **The interpreter at `567255a` had a code regression, since fixed** ([below](#interpreter-regression-fix-2026-09-27)). Two Phase 8 SMC checks ran on every instruction and every store, costing about 10% on Dhrystone and 3–5% on CoreMark. The same day's host was also slower than in the Phase 7 session, so the gap looked larger than it was (CoreMark 520 → 454–471 it/s; [`interp-ab.md`](bench/2026-09-27-567255a-final/interp-ab.md)).
  - The "vs interp" ratios in the tables above use the pre-fix interpreter and are up to about 10% high (Dhrystone).
  - Phase 5's 26.5× (CoreMark) and 39.6× (Dhrystone) are the conservative figures.
- The JIT matches its earlier measurements (CoreMark 13,498 at Phase 5, 13,789–14,144 in the Phase 7 sessions). The spread between sessions is host variance.

**Linux boot to a BusyBox shell** (`python3 tools/boot-bench.py --configs jit,interp,qemu --runs 5`, same code as `567255a`; [`bench/2026-09-27-567255a-boot/`](bench/2026-09-27-567255a-boot/)):

| config | time to shell (s) | guest instructions | MIPS |
|---|---:|---:|---:|
| bridgev jit | 1.48 (1.34–1.50) | 926,085,442 | 592 |
| bridgev interp | 10.49 (9.74–10.82) | 895,416,385 | 85 |
| qemu-system-riscv64 | 1.54 (1.40–1.93) | — | — |

- The JIT is slower here than in the Phase 9 measurement (1.25 s). A same-batch A/B of the Phase 9 build against this one shows the same speed (median 1.57 s vs 1.54 s; [`ab.md`](bench/2026-09-27-567255a-boot/ab.md)), so the difference is the host, not the code.

## Interpreter regression fix (2026-09-27)

Two sessions fixed the regression in parallel, with complementary changes, and both were merged (D61):
- **`f7f41aa`, the store path:** `check` returns the pages' permission bits, and a cold `store_slow` holds the code-page and write-log work.
- **`3869c44`, the interpreter loop:** only instructions that write memory check for code-page writes (`Flow::Smc`), and `Interp::run` drains code pages when it starts.

**Final, merged** (`python3 tools/bench.py --suite coremark,dhrystone,fpbench --configs interp`, the merged build and the Phase 7 build back to back; raw data in [`bench/2026-09-27-interp-fix/merged/`](bench/2026-09-27-interp-fix/merged/) and [`phase7-batch3/`](bench/2026-09-27-interp-fix/phase7-batch3/)):

| build | CoreMark (iterations/s) | Dhrystone (Dhrystones/s) | fpbench (units/s) |
|---|---:|---:|---:|
| `2da741e` (Phase 7) | 485 (483–496) | 507,877 (480,341–516,820) | 124 (123–137) |
| **merged fix** | **502** (481–521) | **517,701** (493,575–580,019) | **127** (117–138) |

- **Interleaved A/B** (5 rounds, 2 batches, user CPU seconds, median; Dhrystone 1.5 M runs / CoreMark 800 iterations):

  | build | Dhrystone (s) | CoreMark (s) |
  |---|---|---|
  | merged | 3.035 / 2.991 | 1.711 / 1.636 |
  | `3869c44` alone | 3.208 / 2.981 | 1.690 / 1.626 |
  | `f7f41aa` alone | 3.380 / 3.250 | 1.854 / 1.780 |
  | Phase 7 | 3.083 / 3.263 | 1.849 / 1.675 |

- The per-change details and the earlier batches are in [`bench/2026-09-27-interp-fix/README.md`](bench/2026-09-27-interp-fix/README.md).

**First measurement of `f7f41aa` alone** (its own session):

`python3 tools/bench.py --suite coremark,dhrystone --configs interp`, run back to back for the fix and for the Phase 7 build (`2da741e`, same harness). Raw data and same-batch A/B runs: [`bench/2026-09-27-f7f41aa-interp/`](bench/2026-09-27-f7f41aa-interp/) ([`ab.md`](bench/2026-09-27-f7f41aa-interp/ab.md)).

Host: Intel(R) Xeon(R) Processor @ 2.10GHz (4 vCPU, pinned to CPU 2), rustc 1.94.1. 5 measured runs after 1 warm-up, median (min–max). Shared cloud VM: expect noise of several percent.

| build | CoreMark (iterations/s) | Dhrystone (Dhrystones/s) |
|---|---:|---:|
| `2da741e` (Phase 7) | 499 (483–521) | 507,247 (492,038–530,089) |
| **`f7f41aa` (fix)** | **487** (452–507) | **501,304** (486,392–520,319) |
| `567255a` (Phase 11 final, above) | 454 (436–470) | 472,238 |

That session's notes follow.

- Cause: Phase 8's SMC bookkeeping (D49) on the interpreter's store path. `DirectMem::store` looked each page up twice (once to check permissions, once for the code mark), and the larger body kept it out of line with six callee-saved register saves per store.
- Fix: `check` returns the OR of the pages' permission bytes; code-page and lockstep write-log handling moved to a cold `store_slow`. No behaviour change.
- The fix and `2da741e` are within noise of each other (ranges overlap; same-batch CPU-time medians differ by −1.6% to +2.2%).

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
| `softmmu` | `--mem=softmmu`: the full JIT with every access through the software TLB (Phase 7) |
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

## SoftMMU: direct vs `--mem=softmmu` (Phase 7, 2026-09-27, commit `6c95332`)

Raw data: [`bench/2026-09-27-6c95332-softmmu/results.json`](bench/2026-09-27-6c95332-softmmu/results.json). Same host, method and noise caveat as above. The JSON records a dirty tree only because docs were being edited; the binary was built from `6c95332`. `softmmu` = the full JIT with every load and store through the inline software TLB (D48).

| workload | direct (`jit+linear`) | softmmu | softmmu / direct | softmmu guest MIPS |
|---|---:|---:|---:|---:|
| CoreMark (iterations/s) | 13,789 (13,512–14,074) | 7,498 (7,440–7,574) | 0.54 | 2,688 |
| Dhrystone (Dhrystones/s) | 21,803,943 (21,598,316–22,869,068) | 10,856,731 (10,704,928–11,454,179) | 0.50 | 3,642 |

**Against QEMU:** for reference, `qemu-riscv64` (direct host mapping) measured 9,242 CoreMark it/s and 5,142,221 Dhrystones/s in the same session at `2da741e` ([`bench/2026-09-27-2da741e-softmmu/`](bench/2026-09-27-2da741e-softmmu/)). That puts softmmu at 0.81× QEMU on CoreMark and 2.1× on Dhrystone.

**TLB microbenchmark** (`tools/tlb-bench.py`, bare-metal Sv39 in S-mode + user-mode twin, 5 runs, median):
- A TLB hit costs 4.15 ns of load-to-use latency (a raw host load: 1.01 ns) and 0.34 ns per access in throughput.
- A miss with an Sv39 walk costs 28.1 ns per access.
- Details: [`bench/2026-09-27-2da741e-softmmu/tlb.md`](bench/2026-09-27-2da741e-softmmu/tlb.md) and the Phase 7 report §7.2.

## Linux boot to a BusyBox shell (Phase 9, 2026-09-27, commit `c24bc9c`)

`python3 tools/boot-bench.py --configs jit,interp,qemu,qemu-bvdtb --runs 5`. Host: Intel Xeon @ 2.10 GHz (noisy shared VM); `taskset -c 2`, 1 warm-up run, then 5 measured runs; median (min–max).
- Guest: Ubuntu 24.04 riscv64 kernel 6.8.0-60 + busybox-static 1.36.1 initramfs (`tools/fetch-guest-images.sh`), 512 MiB, 1 hart.
- Time to shell = process start until `/ # ` appears on the console. Instructions and MIPS cover the whole run, up to `poweroff -f` typed at the prompt.
- QEMU 8.2.2 TCG uses its bundled OpenSBI and its own devicetree (`qemu`), or bridgev's devicetree (`qemu-bvdtb`).

| config | time to shell (s) | guest instructions | MIPS |
|---|---:|---:|---:|
| bridgev jit | 1.25 (1.21–1.42) | 930,751,294 | 701 |
| bridgev interp | 9.99 (9.82–10.21) | 895,355,098 | 89 |
| qemu-system-riscv64 | 1.50 (1.36–1.56) | — | — |
| qemu-system-riscv64 + bridgev DTB | 1.62 (1.25–1.72) | — | — |

Raw output, a full boot log and one run's `--stats`: [`bench/2026-09-27-c24bc9c-boot/`](bench/2026-09-27-c24bc9c-boot/). Before the system-mode chaining work (D51), at `ee9f7a6`: jit 1.80 s (3 runs), QEMU 1.48 s ([`bench/2026-09-27-ee9f7a6-boot/`](bench/2026-09-27-ee9f7a6-boot/)); after D51, at `a08a562`: jit 1.30 s ([`bench/2026-09-27-a08a562-boot/`](bench/2026-09-27-a08a562-boot/)). Analysis: Phase 9 report §3.6 and §7.

## Earlier harness runs
- [`bench/2026-09-26-3099969-baseline.md`](bench/2026-09-26-3099969-baseline.md): the same matrix before the Phase 5 tuning (budget in memory). `jit+linear` CoreMark 13,041, Dhrystone 18.4 M. The before/after of the tuning itself was measured in a same-batch A/B run: Phase 5 report §7.3.

## Rules (CLAUDE.md §22)
- Only numbers produced by the benchmark harness go in this file. Each entry records the exact command, commit, host CPU, compiler flags and date.
- Method: `taskset -c 2`, then 1 warm-up run and 5 measured runs. Report the median, min and max.
- The development machine is a shared cloud VM, so treat results as noisy.
- Targets in CLAUDE.md §22 are hypotheses. They never appear here as results.
