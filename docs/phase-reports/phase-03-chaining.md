# Phase 03 report: Block chaining and jump cache

| Field | Value |
|---|---|
| Phase | 03: Block chaining and jump cache (see `docs/ROADMAP.md`) |
| Status | **Complete** (deviations noted in §9) |
| Dates | 2026-09-26 → 2026-09-26 |
| Branch / commits | `claude/compassionate-babbage-ul3prl`: `f656271` (chaining, budget, jump cache, tests), `b8f85d5` (patch log, `fib` bound, decisions), plus the commit containing this report |
| CI | Run #14 on `f656271` (all Phase 3 code): **success**, both jobs, <https://github.com/tan8696/Bridge-V/actions/runs/36253261132>; run #15 on `b8f85d5` and the report commit follow |
| Sessions used | 1 (continued) |

## 1. Summary
Translated blocks now jump **directly into each other**. Every direct exit is a `jmp`/`jcc rel32` with a 4-byte-aligned rel32 field. The first time an exit returns to the dispatcher, its field is patched with one atomic 32-bit store to point at the successor's code. Indirect jumps (JALR, i.e. every function return) go through a 4096-entry **inline jump cache** and jump straight to the target block on a hit. Chained loops stay preemptible through a **budget prologue**, which also doubles as the exact instruction counter.

Results (medians of 5 pinned runs, §7):
- CoreMark: **7,260 iterations/s**, up from 942 unchained (**7.7×**) and 591 for the interpreter (**12.3×**). That is 76% of `qemu-riscv64` on the same host (9,586 it/s).
- Dispatcher entries fall from 180,759 to **10 per million guest instructions**.
- The jump-cache hit rate is **99.999%** on CoreMark and 99.98% on recursive `fib`.

Every suite passes in every configuration: 110 riscv-tests × 7 configs and 35 programs × 7 configs. The configurations cover lockstep, `--no-chain`, 3-instruction slices, and a 64 KiB code cache that forces repeated flushes. The retired-instruction count is identical to the interpreter's in all of them.

## 2. Planned vs delivered
| Task | Planned | Delivered | Status |
|---|---|---|---|
| P3.1 | Exit slot layout, 4-byte-aligned rel32, `ExitSlot` | `lower.rs` `exit_direct`/`exit_cond` (`Asm::align(4, opcode_len)`); `cache::ExitSlot {patch_at, stub, target_pc, linked}`; assert at translation; test over all alignment cases | Done |
| P3.2 | `link`, `unlink_all_incoming`, `may_link`, `--no-chain`, tests | `jit/chain.rs` (`link`, `unlink_incoming`), `CodeMem::patch_rel32` (atomic aligned store), `Jit::next_tb`/`invalidate_pc`; unlink test, §28.5 arithmetic test | Done (`may_link` is trivially true until system mode, §9) |
| P3.3 | Budget prologue, exact icount, preemption test | `sub [budget],n; jl budget_stub` in every TB; icount derived from the budget with refunds (D30); preemption test with slices 1/7/1000/100 000; icount equality tests across engines | Done |
| P3.4 | Jump cache, flush rules, fib test | `CpuState.jmp_cache[4096]` + inline lookup (§13.4); tag-based lazy invalidation (D31); `guest/c/fib.c`; hit rate 99.98% (>90% asserted in `tests/cli.rs`) | Done |
| P3.5 | Statistics | links, unlinks, jump-cache fills and misses, budget exits, dispatcher entries per M instructions; JALR count and hit rate with `--profile-jit` | Done |

## 3. What was built

### 3.1 Exit layout (`src/backend/x86/lower.rs`)
A TB now looks like this. The real bytes, from `--dump-x86` of `loop.elf`'s loop block `addi t1,t1,1; bne t1,t0,-2`, are:
```
0x…110: 48 83 ad 88 00 00 00 02   sub  qword ptr [rbp+88h], 2      ; prologue: charge 2 insns
0x…118: 0f 8c 26 00 00 00         jl   budget_stub                 ; slice used up → exit, nothing run
0x…11e: 48 8b 45 b0               mov  rax, [rbp-50h]              ; t1
0x…122: 48 83 c0 01               add  rax, 1
0x…126: 48 89 45 b0               mov  [rbp-50h], rax
0x…12a: 48 8b 45 b0               mov  rax, [rbp-50h]
0x…12e: 48 8b 4d a8               mov  rcx, [rbp-58h]              ; t0
0x…132: 48 39 c8                  cmp  rax, rcx
0x…135: 90                        nop                              ; pad: rel32 field at …138 (4-aligned)
0x…136: 0f 85 2f 00 00 00         jne  stub_1                      ; slot 1 (taken)
0x…13c: 0f 1f 00                  nop  [rax]                       ; pad: rel32 field at …140
0x…13f: e9 3c 00 00 00            jmp  stub_0                      ; slot 0 (fall-through)
budget_stub:  add [budget],2 ; pc = 0x111b2 ; exit_reason = BUDGET ; eax = (1<<2)|2 ; jmp exit_jit
stub_1 (…16b): mov qword ptr [rbp+80h], 111B2h ; mov eax, 5 ; jmp exit_jit
stub_0 (…180): mov qword ptr [rbp+80h], 111B8h ; mov eax, 4 ; jmp exit_jit
```
Compared with Phase 2 (report §5):
- Every TB starts with the 14-byte budget prologue.
- Exits are aligned, patchable `jmp`/`jcc rel32` instead of `jmp +0` to an adjacent stub.
- Stubs no longer touch `icount`.
- JALR ends with the jump-cache lookup:
```
mov  r10, rax ; shl r10, 3 ; and r10d, 0xFFF0          ; (pc >> 1 & 4095) * 16
cmp  rax, [rbp + r10 + 0x280]                          ; jmp_cache[i].pc  (0x300 - 128)
jne  miss_stub                                         ; pc = rax, exit LOOKUP
jmp  qword ptr [rbp + r10 + 0x288]                     ; jmp_cache[i].host
```

### 3.2 Chaining (`src/jit/chain.rs`, `code_mem.rs`, `cache.rs`, `dispatch.rs`)
1. `exec` decodes the exit code `(tb << 2) | slot`. A plain exit through slot 0 or 1 is remembered as `last_exit` along with the cache generation.
2. `next_tb(pc)` looks up or translates the successor and calls `chain::link(from, slot, to)`, unless a flush happened in between (the generation changed).
3. `link` computes the rel32 against RX addresses, writes it through the RW view with an aligned `AtomicU32` store (`CodeMem::patch_rel32`; in `--wx=mprotect` mode the page is toggled), updates `exits[slot].linked` and appends `(from, slot)` to `to.incoming`.
4. `unlink_incoming(to)` repoints every recorded predecessor at its own stub. `Jit::invalidate_pc` uses it (together with `TbCache::invalidate` and a jump-cache version bump), ready for Phase 8's self-modifying-code handling.
5. `--dump-x86` also appends every patch to `links.txt`.

### 3.3 Budget and exact instruction counting (D30)
- The prologue charges the whole TB. The dispatcher sets `budget = budget_ref = max(min(slice, limit − icount), n_first)` and afterwards adds `budget_ref − budget` to `icount`.
- Refunds:

  | Exit path | Refund |
  |---|---|
  | ECALL | 1 (not retired) |
  | Host fault at instruction `idx` | `n − idx` (dispatcher) |
  | Helper call at `k` | `n − k` before the call, re-charged after a normal return |
  | Budget stub | `n` |

- `helper_interp_one` folds `budget_ref − budget` into `icount` first, so `rdcycle`, `rdinstret` and `minstret` writes see exact values.
- A chained infinite loop returns to the dispatcher every slice (default 100,000 instructions). That is where `tohost`, instruction limits and, later, interrupts get checked.

### 3.4 Jump cache (D31)
- `CpuState.jmp_cache` sits at 0x300: 4096 × `{pc, host}`, where an odd `pc` means empty. The dispatcher fills the entry of every TB it enters.
- `CpuState.jc_tag = jit_id << 48 | version`. The version is bumped by every flush or invalidation, and a mismatch clears the cache before the next entry. This is how `Engine::flush` (which has no `CpuState`) and multiple `Jit`s per process stay safe.

### 3.5 Lockstep with chaining (D32)
Lockstep enters each TB with `budget = n`. A linked exit or jump-cache hit reaches the successor's prologue, which exits (BUDGET) before executing anything. Linked paths are therefore exercised under lockstep while each comparison still covers exactly one TB. Snapshots copy only architectural state (`ArchState`), not the 64 KiB jump cache.

### 3.6 CLI and stats
- New flags:
  - `--no-chain`: no links and no jump-cache fills (D33)
  - `--profile-jit`: counts JALRs for the hit rate
- `--stats` now reports:
  - dispatcher entries, also per million guest instructions
  - exits by reason, including budget and jump-cache miss
  - chain links and unlinks
  - jump-cache fills

## 4. Design decisions made (appended to CLAUDE.md §3)
- **D30:** instruction counting via the budget, with refunds. New exit reasons are BUDGET and LOOKUP. The first TB of a dispatch always runs whole.
- **D31:** jump-cache layout, plus ownership and version tags with lazy clearing.
- **D32:** lockstep uses `budget = n` so chained code is exercised one TB at a time.
- **D33:** `--no-chain` disables linking and jump-cache fills; `--profile-jit` is an opt-in JALR counter.

## 5. How it works: one real patch
From `links.txt` for `loop.elf` (`--dump-x86`, `--features disasm` build):
```
tb_111a8 slot 1 @0x7ee5a16000aa: 0f 85 60 00 00 00 -> tb_111b2 @0x7ee5a1600110 (stub was 0x7ee5a16000df)
tb_111b2 slot 1 @0x7ee5a1600136: 0f 85 d4 ff ff ff -> tb_111b2 @0x7ee5a1600110 (stub was 0x7ee5a160016b)
tb_111b2 slot 0 @0x7ee5a160013f: e9 5c 00 00 00    -> tb_111b8 @0x7ee5a16001a0 (stub was 0x7ee5a1600180)
```
The second line is the loop's back-edge:
- Before: `jne rel32` at `…136` with rel32 field `…138` (4-byte aligned) = `0x2f`, so the target is `…13c + 0x2f = …16b`, its stub `pc = 0x111b2; eax = (1 << 2) | 1 = 5; jmp exit_jit`.
- The first time the branch is taken, the dispatcher sees exit code 5 (TB 1, slot 1), finds TB 1 (itself) for `0x111b2`, and writes `rel = 0x…110 − (0x…138 + 4) = −0x2c = 0xffffffd4` with one aligned store.
- The instruction now reads `0f 85 d4 ff ff ff`. From then on each iteration runs prologue → body → `jne` back to the prologue, with no dispatcher round trip.
- Every 50,000 iterations (100,000 instructions) the prologue's `jl` fires and the dispatcher regains control. The stats show 1,999 budget exits for 2×10⁸ instructions.

## 6. Tests and verification
| Suite | Configurations | Result |
|---|---|---|
| `tests/riscv_tests.rs` (110 `rv64u{i,m,a,f,d,c}-p-*`) | interp; jit; lockstep; jit + lockstep `--no-chain`; jit slice = 3; lockstep `--wx mprotect --no-host-features --max-block 3` | **7 × 110 pass** |
| `tests/user_programs.rs` (35 programs incl. new `fib` ×4 builds, byte-exact vs qemu) | interp; jit; lockstep; jit + lockstep `--no-chain`; jit `--code-cache 64K` (5 full flushes on `printf_float-O0`); jit mprotect/baseline/max-block 7 | **7 × 35 pass** |
| `tests/jit_lowering.rs` (new) | `exit_slots_are_aligned_and_target_their_stubs` (18 exits, every alignment case), `link_then_invalidate_unlinks` (links, bytes, incoming list, dispatcher-free second run, unlink restores stub, retranslated B relinked), `infinite_chained_loop_is_preempted` (slices 1/7/1000/100 000, exact icount), `jump_cache_calls_and_returns_survive_flushes` (1 miss per round; 4 KiB cache with forced flushes; `--no-chain` misses every return) | pass |
| `tests/cli.rs` (new) | `icount_is_exact_under_every_engine` (6 programs × 4 JIT configs == interpreter), `fib_jump_cache_hit_rate_and_dispatcher_entries` (>90%, >100× fewer entries) | pass |
| `chain.rs` unit tests | §28.5 worked example: `jmp` padded to `+0x1043`, field `+0x1044`, patched to `+0x2000` = `E9 B8 0F 00 00` (both W^X modes); unaligned patch rejected | pass |
| Lockstep, release, CLI | `loop.elf` (10⁸ TBs), `fib-O2`, `fib-O0` | identical |

Totals: `cargo test` has **88 tests, 0 failed**. `cargo fmt --check`, `clippy -D warnings` (with and without `--features disasm`) are clean, and `tools/ref-check.sh` reports 35/35.

## 7. Performance (ad-hoc, not a Phase 5 harness measurement)
Host: Intel Xeon @ 2.10 GHz (shared cloud VM, noisy). Commit `f656271`, release build, `taskset -c 2`, 1 warm-up + 5 measured runs, median (min–max). All configurations were measured in one batch. CoreMark ran with fixed iteration counts sized for ≥ 10 s (6,000 / 14,000 / 110,000), and **all 18 CoreMark runs report "Correct operation validated"**.

| Workload | interp | jit `--no-chain` | jit (chained) | chain vs no-chain | chain vs interp |
|---|---|---|---|---|---|
| `loop.elf` (2-insn TB loop), MIPS | 146.5 (139.1–152.5) | 148.8 (136.8–160.5) | **3,357** (3,095–3,978) | 22.6× | 22.9× |
| `fib-O2.elf 32`, MIPS | 264.8 (246.4–280.1) | 560.6 (501.2–635.9) | **3,733** (3,292–3,808) | 6.7× | 14.1× |
| CoreMark, iterations/s | 591.1 (575.2–592.0) | 941.7 (893.8–954.1) | **7,260** (7,019–7,451) | **7.7×** | **12.3×** |
| CoreMark, guest MIPS | 212.0 | 337.8 | 2,604 | | |

- `qemu-riscv64` 8.2.2 on the same host, same binary, 110,000 iterations, 3 runs: **9,586** it/s (9,556–9,618). Bridge-V is at **76%** of QEMU.
- The `fib` chained runs last only about 45 ms, so treat that row as a rough figure.
- `loop.elf` at 3.4 G guest instructions per second is real. The exit code (t1 >> 8 & 0xff = 225) proves all 10⁸ iterations ran, the icount matches the interpreter exactly, and external wall-clock timing agrees (53 ms for `fib` including process start).
- Chaining statistics (`--stats`, 20,000 CoreMark iterations):

  | Metric | `--no-chain` | chained |
  |---|---|---|
  | Dispatcher entries | 1,297,207,632 (**180,759 per M insns**) | 73,662 (**10 per M**) |
  | Exits | 1.25 G none + 42.8 M jump-cache "misses" (every JALR) | 1,627 unlinked, 71,761 budget, 258 jump-cache misses, 16 ECALL |
  | Links | — | 1,627 links, 1,587 jump-cache fills, 0 unlinks |
  | JALR (`--profile-jit`) | — | 42,751,159 JALRs, **258 misses → 99.9994% hit rate** |
- `fib-O2` (default bound): 106,695 → 79 dispatcher entries per M. With bound 32: 991,257 JALRs (GCC turns one of the two recursive calls into a loop), hit rate 99.98%.
- **Unchained is slower than Phase 2's naive JIT**: on CoreMark 942 vs 1,121 it/s in the Phase 2 batch, and on `loop.elf` 149 vs 178 MIPS. The interpreter itself measured 591 here vs 510 then, so batches differ by about 15%, but the direction is expected. Every TB now pays for the budget read-modify-write, NOP padding and larger code (31.4 vs 22.9 host bytes per guest instruction). Every dispatch also checks the jump-cache tag. `--no-chain` is a measurement mode; chaining more than repays the overhead.
- Roadmap hypothesis "+chaining ≥ 1.5× naive" (§22): **met** (7.7× on CoreMark).

## 8. Bugs found and fixed
| # | Symptom | Root cause | Fix | Guard |
|---|---|---|---|---|
| 1 | New test `jump_cache_…` failed (icount 200 ≠ 150) | Test bug: two padding NOPs put `f` one instruction later, so each call ran a NOP | Test layout fixed | — |
| 2 | Same test: all 150 returns missed | Test-expectation bug: 50 distinct call sites and a fresh `CpuState` (empty jump cache) per round means every return is a first-time miss. The JIT was right | Rewrote the test as one looping call site (encodings checked with `llvm-mc`): exactly 1 miss per round | The test |
| 3 | clippy `manual_inspect` on the alignment assert | Style | Restructured | clippy |
| 4 | Suspicion: 3.4 G guest instructions/s looked too fast | Verified, not a bug: guest-visible results (exit code 225, `fib(32) = 2178309`), exact icount, external wall-clock timing | — | `icount_is_exact_under_every_engine` |

No JIT bug surfaced this phase. Every suite passed on the first run of the chained engine, and lockstep (which now exercises linked exits and jump-cache hits, D32) found no divergence.

## 9. Deviations from the plan / spec
- **`may_link`** is trivially true. User and bare mode map memory flat and there are no TbFlags yet (D28). The same-virtual-page rule arrives with system mode (Phase 7).
- **`--no-chain` also disables jump-cache fills** (D33), so the unchained configuration really returns to the dispatcher on every TB.
- `ExitSlot` stores RX addresses rather than offsets. That is simpler, and TBs never move.
- `fib.c` takes an optional upper bound so benchmark runs are long enough. The reference output uses the default.
- `invalidate_pc` exists and is tested, but only Phase 8 will call it from real code paths.
- New files: `tests/data/expected/fib.{out,code}`, `guest/c/fib.c`, and `links.txt` output under `--dump-x86`.

## 10. Known limitations and technical debt
- **No return-address stack.** Returns rely on the jump cache (99.98–99.999% hit rate here). A RAS is a Phase 10 stretch goal.
- **Any invalidation clears the whole jump cache** (via the version tag). This is fine until Phase 8 makes invalidation frequent; precise per-entry removal can come then.
- **Every TB pays the budget read-modify-write**, including self-loops. Later optimizations could check the budget only on back-edges, or with a register-resident counter (Phase 4 has free registers).
- **The budget checks happen only between TBs.** `tohost` detection in bare mode, and instruction limits, are delayed by up to one slice. Pass/fail is unaffected.
- **Host code grew** to 31.4 bytes per guest instruction. Phase 4 (register allocation, constant folding) should shrink it.
- **Single-threaded.** Patches are atomic (D7), but TB metadata is not shared across threads (Phase 10).

## 11. How to reproduce
```
tools/setup.sh && tools/build-guests.sh && tools/build-riscv-tests.sh
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
cargo build --release
B=target/release/bridgev
taskset -c 2 $B run --engine jit --stats guest/build/loop.elf
taskset -c 2 $B run --engine jit --no-chain --stats guest/build/loop.elf
taskset -c 2 $B run --engine jit --profile-jit --stats guest/build/fib-O2.elf 32
taskset -c 2 $B run --engine jit --stats guest/build/bench-coremark.bin 0x0 0x0 0x66 110000
# patch log + disassembly:
cargo build --release --features disasm --target-dir target/disasm
target/disasm/release/bridgev run --engine jit --dump-x86 /tmp/tbs guest/build/loop.elf; cat /tmp/tbs/links.txt
```
(`bench-coremark.bin` is the ad-hoc build from the Phase 1 report §7.)

## 12. Next steps
Phase 4, the IR and register allocator:
- **P4.1–P4.4:** IR, lifting, constant folding / forwarding / dead write-back elimination, liveness.
- **P4.5–P4.6:** pinned R12–R15 (sp, ra, a0, a5) plus linear scan.
- **P4.7:** cold stubs.
- **P4.8:** proptest fuzzing (≥ 10⁶ random blocks) under lockstep.
- Then measure the `none → pinned → linear` levels. Targets: shrink the 31 bytes per guest instruction and pass 1 G instructions/s on CoreMark.
