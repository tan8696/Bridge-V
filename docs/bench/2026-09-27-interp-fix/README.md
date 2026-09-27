# Interpreter slowdown: cause and fix (2026-09-27, after Phase 11)

## Cause
Two Phase 8 (SMC, D49) additions ran on every interpreted instruction or store:
1. **`exec_block` checked `mem.smc_pages.is_empty()` after every retired instruction** (a load and a branch per instruction). Only stores can write code pages.
2. **`DirectMem::store` called `uncode_range` after its permission check.** That made two more page-table lookups per store, only to find the CODE bit that the permission lookup had just read.

## Fix
- **Stores:** store instructions (`Store`, `FStore`, AMO/SC) return the new `Flow::Smc` when their store hit a code page. Under softmmu, loads do the same, because their page walk may set A/D bits in a code page. The JIT's `helper_mmu_access` also stops after such a load, which keeps lockstep exact. `exec_block` ends the block only on `Flow::Smc`, so other instructions pay nothing.
- **Store path:** `DirectMem::store` takes the CODE bit from the same lookup that checks W (`check_bits`), and calls `uncode_range` only when it is set.
- **Draining:** `Interp::run` drains `smc_pages` when it starts, as well as after every block. Code written between runs (a syscall such as `read`, device DMA, another hart) is therefore decoded again before anything executes. The old per-instruction check let the first stale instruction run. Regression test: `interp::tests::code_written_between_runs_is_redecoded`, which fails without the entry drain (a0 = 1, the stale instruction).

## Evidence
**Harness runs** (`tools/bench.py --suite coremark,dhrystone,fpbench --configs interp`). `BRIDGEV_BIN` selects the build (new option).

| batch | build | CoreMark it/s | Dhrystones/s | fpbench units/s |
|---|---|---:|---:|---:|
| 1 | Phase 7, `2da741e` | 465 (459–479) | 475,051 (462,671–505,006) | 122 (120–123) |
| 1 | before the fix, `567255a` ([`before-fix-567255a/`](before-fix-567255a/)) | 453 (420–455) | 429,099 (425,166–431,447) | 117 (108–124) |
| 1 | first version of the fix (stores only) | 457 (456–476) | 472,100 (469,204–481,909) | 118 (113–123) |
| 2 | **final fix** ([`fixed/`](fixed/); uncommitted tree, committed right after) | **503** (493–513) | **483,244** (460,016–489,757) | **120** (119–125) |
| 2 | Phase 7, `2da741e` ([`phase7-2da741e/`](phase7-2da741e/)) | 473 (458–492) | 496,761 (479,890–508,115) | 123 (121–132) |

Batch 2 ran the two builds back to back. The raw files of batch 1's Phase 7 and first-fix runs were superseded by batch 2; their numbers are kept in this table.

**Interleaved A/B** (`taskset -c 2 bridgev run --engine interp`, 5 rounds, user CPU seconds, median). Workloads: Dhrystone with 1,500,000 runs; CoreMark with 800 iterations.

| build | Dhrystone | CoreMark |
|---|---:|---:|
| Phase 7 `2da741e` | 3.005–3.013 | 1.608–1.642 |
| before the fix | 3.228–3.379 | 1.711–1.724 |
| first version of the fix | 3.023–3.025 | 1.621–1.648 |
| final fix | 3.034 | 1.562 |
| upper bound: both checks deleted (incorrect, measurement only) | 3.080 | 1.699 |

- Ranges are the medians of two separate 5-round batches.
- **softmmu interpreter** (`--mem softmmu`, Dhrystone 800,000 runs, 3 interleaved rounds): before the fix 1.93–2.08 s, final fix 1.83–2.16 s. No slowdown.

**Conclusion:** the fix restores Phase 7 interpreter speed, within noise. The Phase 11 notes had put the loss at about 10% by comparing sessions, but the host itself was slower that day: the Phase 7 binary measured 520 CoreMark it/s in its own session and 465–473 here. The code regression was about 10% on Dhrystone and 3–5% on CoreMark.

**Tests (final fix):**
- `cargo test`: 135 passed, 9 ignored (the new regression test included).
- The 9 Linux boot tests (`--ignored`, including both lockstep boots): all pass.
- `PROPTEST_CASES=200000 cargo test --release --test softmmu`: SMC through a code alias, interpreter vs JIT, clean.
