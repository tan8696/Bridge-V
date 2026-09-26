# Bridge-V benchmarks

**No measurements yet.** Bridge-V cannot execute guest code until Phase 1.

Rules (CLAUDE.md §22):
- Only numbers produced by the benchmark harness go in this file. Each entry records the exact command, commit, host CPU, compiler flags and date.
- Method: `taskset -c 2`, then 1 warm-up run and 5 measured runs. Report the median, min and max.
- The development machine is a shared cloud VM, so treat results as noisy.
- Targets in CLAUDE.md §22 are hypotheses. They never appear here as results.
