# Phase 10: return-address stack and superblocks (not pursued)

| Field | Value |
|---|---|
| Items | Return-address stack; superblocks/traces (Phase 10 stretch goals 7 and 8) |
| Status | Not implemented. The analysis below says neither would meet its "measurable speedup" criterion with a reasonable amount of work. |
| Date | 2026-09-27 |

## Return-address stack
**The idea.** Predict `ret` (`jalr x0, 0(ra)`) targets with a shadow stack pushed on calls. That saves the jump-cache lookup and, more importantly, the host branch mispredictions of a polymorphic indirect jump.

**Why it would not pay here:**
- Returns already go through the inline jump cache (D8/D31): six instructions and one indirect `jmp`. The hit rate exceeds 90% on the call-heavy fib (`tests/cli.rs::fib_jump_cache_hit_rate_and_dispatcher_entries`).
- A software RAS in `CpuState` still ends in an indirect `jmp` to a host address. The host predicts it no better than the jump cache's, so the only saving is the hash and compare, a few cycles per return.
- The real win is a host `call`/`ret` pair, so the host's own return-stack buffer predicts guest returns (as FEX-Emu and box64 do). That conflicts with the block-boundary ABI (§8.3: RSP is fixed at every block boundary, and the budget and fault handling rely on it). It would need a separate host stack switched on every TB entry and exit. That is a large, risky change for a gain that D45's experience suggests CoreMark and Dhrystone would not show.

## Superblocks / traces
**The idea.** Translate hot paths across TB boundaries so the allocator and optimizer see larger regions.

**Why not now:**
- D45 tried the closest cheap variant, keeping registers resident across a self-loop's iterations. It doubled a synthetic loop but moved neither CoreMark (−0.4%) nor Dhrystone (−1.4%).
- Chained TBs already run back to back with no dispatcher involvement. The remaining per-TB costs are the budget check (2 instructions) and the write-back of dirty registers at exits.
- Trace formation needs profiling counters in the hot path, side exits with state maps, and invalidation of traces that span pages (SMC, D49). That is a large design change without evidence of a win on the benchmarks.

## What would be measured first
Before building either, `--profile-tbs` (D44) on CoreMark and Dhrystone should show a substantial share of time in return sequences or TB transitions. The Phase 5 profiles did not.
