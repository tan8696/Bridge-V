# Bridge-V benchmarks

**No harness measurements yet.** The benchmark harness (`bridgev bench`, P5.2) arrives in Phase 5. Until then, ad-hoc baseline numbers (same method, run by hand) are recorded in the phase reports: interpreter in `phase-reports/phase-01-interpreter.md` §7, naive JIT in `phase-reports/phase-02-naive-jit.md` §7, chaining and jump cache in `phase-reports/phase-03-chaining.md` §7.

Rules (CLAUDE.md §22):
- Only numbers produced by the benchmark harness go in this file. Each entry records the exact command, commit, host CPU, compiler flags and date.
- Method: `taskset -c 2`, then 1 warm-up run and 5 measured runs. Report the median, min and max.
- The development machine is a shared cloud VM, so treat results as noisy.
- Targets in CLAUDE.md §22 are hypotheses. They never appear here as results.
