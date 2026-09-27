Interpreter slowdown since Phase 7 (Phase 11 investigation, 2026-09-27).

1. `interp-rerun.md`: the interp cells of `results.md` re-run with nothing else on the machine
   (`python3 tools/bench.py --suite coremark,dhrystone --configs interp`): CoreMark 471 it/s,
   Dhrystone 465,530/s (Phase 7 at `2da741e`: 520 and 517,037).
2. Same-batch A/B, `taskset -c 2 bridgev run --engine interp guest/build/bench/dhrystone-rv64.elf 1500000`,
   wall seconds, runs interleaved:

| build | runs (s) |
|---|---|
| 2da741e (Phase 7) | 2.99 2.84 2.92 |
| HEAD | 3.30 3.48 3.13 |

   Second batch, five builds interleaved:

| build | runs (s) |
|---|---|
| f488d24 (end of Phase 8) | 2.92 3.09 3.13 |
| 41fbfee (end of Phase 9) | 3.14 3.17 3.21 |
| 6b5a7c2 (OpenSBI, Sv48) | 3.10 3.16 3.40 |
| 25c9ffd (threads, signals, dynamic ELF) | 3.25 3.38 3.38 |
| HEAD | 3.00 3.25 3.29 |

Conclusion: the interpreter is about 10% slower than at Phase 7, lost a few percent at a time over
Phases 8–10 rather than in one commit. The JIT is unaffected (CoreMark 13,409 vs 13,498 at Phase 5).
Ratios "vs interp" at `567255a` are therefore about 10% higher than they would be against the
Phase 7 interpreter.

**Update (same day): fixed.** The regression came from two Phase 8 SMC checks: a `smc_pages` test after every interpreted instruction, and duplicated page lookups on every store. Both are gone (D61). Same-session harness runs of the Phase 7 build, the pre-fix build and the fixed build ([`../2026-09-27-interp-fix/`](../2026-09-27-interp-fix/README.md)) show the fixed interpreter at Phase 7 speed. They also show that about half of the gap measured above was the host: the Phase 7 binary scored 465 CoreMark it/s there, against 520 in its own session.
