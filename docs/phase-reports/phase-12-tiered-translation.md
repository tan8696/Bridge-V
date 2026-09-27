# Phase 12 report: tiered translation (interpreter tier for cold code)

| Field | Value |
|---|---|
| Phase | 12: Tiered translation, a post-roadmap addition (`docs/ROADMAP.md` §12, decision D63) |
| Status | Complete |
| Dates | 2026-09-27 → 2026-09-28 |
| Branch / final commit | `claude/tiered-translation`: code in `75c9053` (tier, off by default) and `2e498ee` (default 32), plus the commit adding this report |
| Sessions used | 1, from a Windows PC without Linux: every build, test and measurement ran on GitHub Actions (`ci` and the new `bench` workflow) |

## 1. Summary
The Phase 9 report found that about 40% of the Linux boot went into translating 66,669 small blocks, most of which run only a few times. Phase 12 adds an **interpreter tier**: the JIT engine runs a block in the reference interpreter until it has run `--tier N` times (default **32**), and translates it on the next run. Code that runs only a few times is never translated.

- **Measured first.** In a boot with every block interpreted, 31% of the 65.7 k blocks ran exactly once and 64% fewer than 16 times.
- **Default chosen by measurement.** A threshold sweep shows the expected U-shape, with the optimum between 16 and 64. The default, 32, is also near the ski-rental break-even of translation cost against interpretation cost (§4).
- **Most important result.** On a GitHub runner (AMD EPYC 7763, 7 interleaved boots), time to shell went from **1.30 s to 1.06 s (−18%)**, from 1.08× to **1.33× `qemu-system-riscv64`** (1.41 s). The JIT translated **72% fewer blocks** into **72% less host code** (43.5 → 12.3 MiB).
- **No cost elsewhere.** CoreMark, Dhrystone and fpbench are unchanged within the runner's noise (+0.1%, +1.1%, −1.2%), and their translation time drops 5–7×.

## 2. Planned vs delivered
| Task ID | Task | Status | Notes |
|---|---|---|---|
| P12.1 | Measure how often blocks run during a boot | done | `--stats` histogram of still-cold blocks; a boot with `--tier 1000000000` interprets everything (§7.1) |
| P12.2 | Interpreter tier in the JIT dispatcher (`--tier N`) | done | `src/jit/dispatch.rs` (§3) |
| P12.3 | Tests: tiered riscv-tests, guest programs, icount, Linux boots | done | §6 |
| P12.4 | Choose the default from a boot-time sweep | done | 32 (§7.2, §7.3) |
| P12.5 | Check user-mode benchmarks for regressions | done | §7.4 |
| P12.6 | Benchmarks without a local Linux machine | done | `.github/workflows/bench.yml`, `tools/boot-bench.py` tier configs and interleaving |
| P12.7 | Documentation | done | CLAUDE.md (D63, §0, §5, §6, §23, §24, §28), ROADMAP, BENCHMARKS, PROJECT_EXPLAINED, README |

## 3. What was built

### 3.1 The tier (`src/jit/dispatch.rs`)
- **`JitOptions::tier`** and **`DEFAULT_TIER = 32`** feed both the library default and the CLI (`--tier N` on `run` and `boot`). `--tier 0` is the old behaviour.
- **`Next::Cold(i)`** is a new answer of `select()`: "interpret cold block `i`".
- **`cold_run(key)`**, called in `select()` where a missing translation would be created:
  - it counts a run of every block that has no TB;
  - while the count is at most N, it returns the block's index, and clears `last_exit`, because there is no code to chain to;
  - on run N+1 it frees the decoded copy and returns `None`, and `select()` translates as before.
- **`exec_cold(i)`**:
  - on a block's first run (or after its page was written) it decodes the block with `build_block_soft`/`build_block_max` at `--max-block`, so a cold block has the same boundaries as its TB;
  - it registers the block's pages in `cold_pages` and marks them as code pages;
  - it then runs the block with the interpreter's own `exec_block`.
- **State:** `cold: FxHashMap<TbKey, u32>` (the index), `cold_blocks: Vec<Cold>` (`key`, `runs`, `block: Option<Block>`), and `cold_pages: FxHashMap<u64, Vec<u32>>`. A `Vec` plus an index keeps a cold run to one hash lookup in the cold table.
- **SMC:** `drain_smc` also drops the decoded copy of every cold block from a written page. `flush_all` clears the three tables.
- **`code_pages(key, soft, bytes)`:** the D49 page computation, factored out of `tb_for_key`, now shared by TBs and cold blocks.
- **Lockstep** (`src/jit/lockstep.rs`) creates its `Jit` with `tier: 0`, so every block is still translated and compared.
- **`--stats`** (`tier_counters`) adds: blocks and guest instructions interpreted, blocks translated after warm-up, and the still-cold blocks by run count (1, 2–3, 4–7, …).

### 3.2 Tools
- **`tools/boot-bench.py`:**
  - `<engine>-tierN` configurations pass `--tier N`;
  - `BRIDGEV_BIN` selects another build (as in `bench.py`);
  - `--detail` prints each configuration's statistics line;
  - configurations now **take turns run by run**, so host drift affects them alike (previously all runs of one configuration ran back to back).
- **`tools/bench.py`:** a `jit-tier0` configuration for A/B runs against the default.
- **`.github/workflows/bench.yml`:** on a push to `bench/**`, a GitHub-hosted runner runs the boot sweep, a fully interpreted boot for the histogram, and CoreMark/Dhrystone/fpbench (results also uploaded as an artifact). It was added because this phase had no Linux machine. It also lets anyone measure from any computer.

## 4. Design decisions made
**D63** (CLAUDE.md §3): interpreter tier, default 32, cold blocks keyed and invalidated like TBs, lockstep untiered.

**Why a threshold, and why about 32.** Costs measured on the runner:
- **T**, translating a block: 561 ms / 65,634 TBs = **8.5 µs**.
- **I**, a run in the interpreter tier: **0.16 µs** (the fully interpreted boot: 13.5 s / 82.6 M block runs, dynamic average 10.9 instructions; shorter cold blocks cost less).
- **J**, a run of translated code: about 1 ns per guest instruction.

A block that runs k times costs T + kJ if translated at once and kI if never translated. The two meet at k = T / (I − J) ≈ **55–90 runs**: 55 with the measured average I, up to 90 for short cold blocks (I ≈ 0.1 µs).

A block's future is unknown when it first runs, which makes this the **ski-rental problem**. Interpreting until the interpretation cost has reached the translation cost, and then translating, is 2-competitive: no block costs more than about twice what the best choice in hindsight would have cost. The measured optimum was flat from 16 to 64. 32 was the fastest configuration in the confirmation run (§7.3), just below the break-even estimate.

**Alternatives considered:**
- **Speed up the translator.** It would help every mode, but it can only shrink, never remove, the cost of translating code that runs once. Profiling the translator is a possible later phase.
- **A persistent translation cache across runs** (as Rosetta 2 does ahead of time). This removes translation from repeated boots, including for hot code. But translated code would need relocation records (helper addresses, trampolines, TB ids) and validation of the guest bytes. That is much more code and risk, and it only helps the second run.
- **A baseline JIT tier (the Phase 3 naive back end) for cold code.** Its block-boundary ABI differs (no pinned registers, budget in memory, D46), and in softmmu it sends every load and store through `helper_interp_one` (D48). Mixing it with the IR back end would need both changes.
- **Reuse the interpreter engine's block cache.** It is keyed by the virtual pc and emptied at every TLB flush (D48). Linux flushes very often, so cold blocks would be decoded again and again: decoding (about 0.5 µs) costs more than several interpreted runs. Keying cold blocks by the `TbKey` makes them survive flushes as TBs do.

## 5. How it works: worked example
A kernel block B at virtual pc V, physical page P, run by the JIT engine with the default tier 32:

1. **Run 1.**
   - `select()` translates V through the TLB (`fetch_page` → P) and builds the key K = (V, FP variant, MMU flags, P).
   - `cold_run(K)` finds no TB and no cold entry. It appends `Cold { key: K, runs: 0 }`, counts the run (runs = 1 ≤ 32) and returns `Next::Cold(i)`.
   - `exec_cold(i)` decodes B, records `cold_pages[P] += i`, marks P as a code page and runs B with `exec_block`. The run loop then delivers the exit (ECALL, trap or continue) as for a TB.
2. **Runs 2–32.** The same, except that the decoded copy is reused.
3. **Run 33.**
   - `cold_run` counts runs = 33 > 32, frees the decoded copy and returns `None`.
   - `tb_for_soft` translates B, and `exec` runs it.
   - From now on `cold_run` returns at `cache.lookup_key(&K).is_some()`, and exits into B are chained as usual.
4. **P is written** (a store, DMA, another hart's store):
   - `drain_smc` invalidates B's TB through `page_tbs` and would drop a decoded cold copy through `cold_pages`.
   - B's counter stays at 33, so its next run translates it again at once, like any invalidated TB.

**The real `--stats` tail** of a default boot (commit `2e498ee`):
```
tier 32: 850398 blocks (5330215 guest insns) interpreted, 17960 blocks then translated,
47217 still cold (by runs: 1: 20587, 2-3: 7766, 4-7: 5322, 8-15: 7925, 16-31: 5444, 32: 173)
```

## 6. Tests and verification
**New or changed tests:**

| Test | What it checks |
|---|---|
| `riscv_tests::phase1_suites_pass_tiered` | all 244 riscv-tests under the JIT at tiers 1, 3 and the default (32) |
| `user_programs::guest_programs_match_qemu_reference_tiered` | every guest program at tier 2, with direct and softmmu memory, byte-identical to qemu-riscv64 (includes `smc.c`: self-modifying code with cold blocks) |
| `user_programs::…_jit` | the default (tiered) JIT and `--tier 0`, both against QEMU |
| `cli::icount_is_exact_under_every_engine` | exact instruction count at the default tier, at `--tier 0` (with and without chaining, tiny code cache) and at `--tier 3` |
| `cli::tier_translates_only_hot_blocks` | tier 2 translates fewer TBs than tier 0; tier 10⁹ translates none; instruction counts identical |
| `linux_boot::*_jit` | every JIT boot (built-in SBI, OpenSBI, Sv48, virtio disk, 4 harts, OpenSBI + 4 harts) now runs the default tier |
| `linux_boot::linux_boots_translating_every_block_jit` | a boot with `--tier 0`, so full translation stays covered outside lockstep |

**Tests that would have silently changed meaning.** With the tier on by default, several tests of translated code would still have passed, but by testing the interpreter:
- `jit_lowering.rs` compares each ALU instruction's translation against the interpreter, using one-run programs;
- the `--dump-x86` test expects dumped TBs of `hello`;
- the host-fault test expects a SIGSEGV inside JIT code.

These now request `--tier 0`: the `jit_lowering` rig always does, as do the JIT-only riscv-tests and guest-program configurations (`untiered()`, `run_all_translated`).

**CI evidence** (GitHub Actions, `ci` workflow):

| Run | Commit | Result |
|---|---|---|
| 36350287344 | `d35fff5` (tier off by default; tiered boots at 16 on 1 and 4 harts) | Linux boot job: all boots pass. The test job failed only at `cargo fmt` (one chain layout), fixed in `82f2d8c` |
| 36350504155 | `53f9030` | fmt, clippy (both feature sets), guest builds, QEMU reference checks, `cargo test`: **138 passed, 0 failed, 10 ignored** (the boot tests, run by the boot job); boot job: pass |
| 36351081526 | `2e498ee` (default 32) | `cargo test`: **138 passed, 0 failed, 10 ignored**; boot job: **10 passed, 0 failed**; riscv-tests reference job: pass |

**Acceptance:**
- ✅ Every suite passes with the tier on by default and with it off (§6 table and runs above).
- ✅ Lockstep still checks every translation (it forces `--tier 0`; its riscv-tests, guest-program and Linux-boot runs pass).
- ✅ The Linux boot is faster: 1.30 s → 1.06 s (§7.3).
- ✅ The user-mode benchmarks do not regress beyond noise (§7.4).

## 7. Performance
**Method.** GitHub-hosted runner `ubuntu-24.04`: AMD EPYC 7763, 4 vCPU, a shared cloud VM; `taskset -c 2`. Boots: 1 warm-up per configuration, then 5 or 7 runs with the configurations taking turns, median (min–max). This is a different host from the Phase 11 numbers (Intel Xeon @ 2.10 GHz), so compare only within each table. Raw output: [`docs/bench/2026-09-27-tier/`](../bench/2026-09-27-tier/).

### 7.1 How often blocks run during a boot
`--tier 1000000000`: every block stays in the interpreter tier. The boot took 13.4 s (66 MIPS). The histogram covers 65,690 distinct blocks (commit `53f9030`; the rerun at `2e498ee` gave 65,666 with the same shape):

| runs | blocks | share | cumulative |
|---|---:|---:|---:|
| 1 | 20,659 | 31.4% | 31.4% |
| 2–3 | 7,952 | 12.1% | 43.6% |
| 4–7 | 5,356 | 8.2% | 51.7% |
| 8–15 | 7,926 | 12.1% | 63.8% |
| 16–31 | 5,382 | 8.2% | 72.0% |
| 32–63 | 4,659 | 7.1% | 79.1% |
| 64–127 | 3,109 | 4.7% | 83.8% |
| 128–2047 | 8,655 | 13.2% | 97.0% |
| 2048 and more | 1,992 | 3.0% | 100% |

### 7.2 Threshold sweep (`53f9030`, 5 runs)
| `--tier` | time to shell | TBs translated | host code | translate time | blocks interpreted |
|---:|---:|---:|---:|---:|---:|
| 0 | 1.32 s (1.28–1.33) | 65,544 | 44,453 KiB | 514.6 ms | 0 |
| 1 | 1.20 s (1.18–1.24) | 44,939 | 30,831 KiB | 363.0 ms | 65,282 |
| 4 | 1.16 s (1.13–1.18) | 34,621 | 23,770 KiB | 297.0 ms | 186,341 |
| 16 | 1.11 s (1.09–1.12) | 23,167 | 16,214 KiB | 200.6 ms | 525,589 |
| 64 | 1.11 s (1.09–1.17) | 13,341 | 9,277 KiB | 115.8 ms | 1,340,203 |
| 256 | 1.32 s (1.29–1.35) | 7,452 | 5,223 KiB | 70.5 ms | 3,204,448 |
| qemu-system-riscv64 8.2.2 | 1.41 s (1.40–1.47) | | | | |

(Counters are from each configuration's last run, `--detail`.)

### 7.3 Confirmation of the default (`2e498ee`, 7 runs)
| `--tier` | time to shell | TBs translated | host code | translate time |
|---:|---:|---:|---:|---:|
| 0 | 1.30 s (1.28–1.50) | 65,634 | 44,501 KiB | 560.7 ms |
| 16 | 1.07 s (1.05–1.08) | 23,163 | 16,219 KiB | 196.8 ms |
| **32** | **1.06 s (1.05–1.09)** | **18,023** | **12,631 KiB** | **154.2 ms** |
| 64 | 1.09 s (1.07–1.10) | 13,391 | 9,310 KiB | 118.9 ms |
| qemu-system-riscv64 8.2.2 | 1.41 s (1.37–1.43) | | | |

- **Boot time:** −18% (1.30 → 1.06 s).
- **Against QEMU:** from 1.08× to 1.33× (QEMU 1.41 s).
- **Translation:** −72% TBs, −72% host code, −72% translate time.
- **What is left of the old translation cost:** about 5.3 M guest instructions run in the interpreter tier, and 154 ms of translation for the 18 k blocks that proved hot.

### 7.4 User mode (`2e498ee`, `tools/bench.py --configs jit+linear,jit-tier0 --runs 3`)
| workload | default (tier 32) | `--tier 0` | difference | translate time |
|---|---:|---:|---:|---:|
| CoreMark (it/s) | 11,847 (11,794–11,858) | 11,833 (11,831–11,843) | +0.1% | 2.1 ms vs 10.3 ms |
| Dhrystone (/s) | 16,710,005 (16,675,336–16,723,168) | 16,524,448 (16,242,056–16,543,770) | +1.1% | 1.2 ms vs 8.5 ms |
| fpbench (units/s) | 2,079 (2,072–2,101) | 2,105 (2,105–2,105) | −1.2% | 1.4 ms vs 9.0 ms |

- **Why the scores don't move.** A hot block pays 32 interpreted runs, a few µs in total, before it is translated, and then runs as before.
- **The differences are within the runner's noise.** `bench.py` runs one configuration's runs back to back, so drift between configurations of about 1% is expected.
- **Translation still drops 5–7×.** Most of each program's startup (glibc initialisation) runs only a few times.

## 8. Bugs found and fixed
| Symptom | Root cause | Fix (commit) | Regression test |
|---|---|---|---|
| `cargo fmt --check` failed in CI | a 61-character method chain (`chain_width` is 60); no rustfmt on the development PC | `82f2d8c` | CI's fmt step |
| The first `bench` run failed to build | the workflow checked out without the SoftFloat submodule | `53f9030` | the `bench` workflow itself |
| (Avoided) tests of translated code would have tested the interpreter under the new default | default-changing change; one-run test programs never reach the tier | explicit `--tier 0` in those tests (`2e498ee`) | the tests keep their meaning; `cli::tier_translates_only_hot_blocks` checks that `--tier 0` translates |

No code bugs were found: the tiered engine passed every suite, including the Linux boots, the first time it compiled.

## 9. Deviations from the plan / spec
- **Not in the original roadmap.** It is recorded as Phase 12 in `docs/ROADMAP.md` and as D63.
- **Measured on GitHub Actions**, not the project's cloud container, because this phase had no Linux machine. The Phase 11 Xeon numbers remain the numbers of record for the other configurations. Phase 12's numbers compare configurations on the same runner only.
- **CLAUDE.md §13/§5 wording.** "The dispatcher translates on a miss" now reads "translates a block after `--tier` interpreted runs" (§5 main loop).

## 10. Known limitations and technical debt
- **The cold path costs more per run than the interpreter engine's own loop.** The fully interpreted boot averaged 0.16 µs per block run: `select()` with its TLB fetch translation, two hash lookups, then `exec_block`. A cheaper cold path, for example skipping the TB lookup for blocks with a cold entry, would move the optimum to higher tiers.
- **Per engine.** SMP harts and guest threads each have their own engine (D55, D60), so each warms up its own counters.
- **Counters restart after a full flush** (a full code cache, or a user-mode mapping of executable memory). Hot code then waits 32 interpreted runs again.
- **The threshold is one global number.** It is not adapted per block (for example by block length) or per workload.
- **Seen in the statistics, not caused by this phase:** about 210,000 page-straddling instructions per boot go one by one through `interpret_one` (decoded each time). Caching their decoded form is a separate small follow-up.

## 11. How to reproduce
```sh
cargo build --release && tools/fetch-guest-images.sh
python3 tools/boot-bench.py --configs jit-tier0,jit-tier16,jit-tier32,jit-tier64,qemu --runs 7 --detail
python3 tools/boot-bench.py --configs jit-tier1000000000 --runs 1 --detail     # the histogram of §7.1
tools/build-bench.sh && python3 tools/bench.py --configs jit+linear,jit-tier0 --runs 3
cargo test --test cli tier_ && cargo test --test riscv_tests tiered && cargo test --test user_programs tiered
cargo test --release --test linux_boot -- --ignored --test-threads 1
```
Without a Linux machine, push the commit to `bench/<name>`. The `bench` workflow runs the first four lines and puts the tables in its job summaries.

## 12. Next steps
1. **Profile and trim the cold path** (one hash lookup instead of two, cheaper key computation). Then re-sweep the tier.
2. **Cache decoded page-straddling instructions** (§10).
3. **A persistent translation cache** for repeated boots. The tier makes it smaller (only hot blocks are translated), but it needs relocatable TBs.
4. **Parallel guest execution** remains the largest item (Phase 11 report §12).
