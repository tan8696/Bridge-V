# Interpreter regression fix: same-batch A/B (2026-09-27)

Host: Intel(R) Xeon(R) Processor @ 2.10GHz (4 vCPU), `taskset -c 2`, shared cloud VM (noise of several percent).
Builds: `2da741e` (Phase 7) in its own worktree, `567255a`-equivalent HEAD `47fd507` (code unchanged since), and the fix `f7f41aa`.
Metric: user + system CPU seconds of `bridgev run --engine interp`, runs interleaved across builds (lower is better).

Dhrystone, 1,500,000 runs, 9 rounds:

| build | min | median |
|---|---:|---:|
| 2da741e (Phase 7) | 2.688 | 2.899 |
| 47fd507 (before fix) | 2.845 | 3.197 |
| f7f41aa (fix) | 2.853 | 2.964 |

Second batch (9 rounds Dhrystone, 7 rounds CoreMark `0x0 0x0 0x66 1500`):

| build | Dhrystone median | CoreMark median |
|---|---:|---:|
| 2da741e | 2.911 | 3.104 |
| f7f41aa | 2.865 | 3.117 |

Variants tried and dropped:
- Skipping `exec_block`'s per-instruction `smc_pages` check for register-only instructions (`matches!` on the opcode): slower (Dhrystone median 3.20, CoreMark 3.37), so the check stays as it was.
- Removing that check entirely (experiment only, not a valid change): no clear gain on top of the fix (Dhrystone 2.955, CoreMark 2.98).

Harness runs (`tools/bench.py --suite coremark,dhrystone --configs interp`, 5 runs after 1 warm-up, run back to back): `fix/` and `base-2da741e/` (the current `bench.py` run against the `2da741e` binary).

| build | CoreMark it/s | Dhrystones/s |
|---|---:|---:|
| 2da741e | 499 (483–521) | 507,247 (492,038–530,089) |
| f7f41aa | 487 (452–507) | 501,304 (486,392–520,319) |
| 567255a (Phase 11 final, for reference) | 454 (436–470) | 472,238 |
