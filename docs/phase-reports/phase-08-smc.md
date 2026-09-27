# Phase 08 report: Self-modifying code

| Field | Value |
|---|---|
| Phase | 8: Self-modifying code (see `docs/ROADMAP.md`) |
| Status | Complete |
| Dates | 2026-09-27 |
| Branch / final commit | `claude/compassionate-babbage-ul3prl` @ the commit that adds this report (code: `4715280`; the MMIO fix of §8 is in the same commit as this report) |
| Sessions used | 1 |

## 1. Summary

Before this phase, the only protection against stale translations was FENCE.I, which flushed the entire translation cache. A program that patched code without FENCE.I (x86-style) kept running stale translations. The new SMC test program shows it: 4 of its 5 cases printed wrong results before the change, e.g. a self-patching loop summed to 199 instead of 5050.

**What changed:**
- **Tracking:** every page an engine decodes or translates from is marked as a code page.
  - Direct mode write-protects it on the host.
  - Softmmu tags its TLB write entries with `TLB_CODE`.
  - Every other write path checks the mark.
- **On a write:** both engines stop right after the store. The dispatcher invalidates exactly the TBs of that page and unlinks every chain into them.
- **FENCE.I:** with nothing stale left, it now only resets the jump cache.

**Evidence:**
- All five SMC cases match `qemu-riscv64` byte for byte under every engine, both memory backends and every regalloc level.
- The softmmu fuzzer, extended with a writable alias of the code page, is clean at 1,000,000 cases. The 1M run found one more inconsistency, in MMIO store values (§8).
- CoreMark sees no code-page writes at all, and its speed is unchanged (§7).

## 2. Planned vs delivered

| Task ID | Task | Status | Notes |
|---|---|---|---|
| P8.1 | Code-page tracking | done | `Jit::page_tbs: page → [TbId]` (physical page with softmmu), filled at translation. `DirectMem` CODE bit per page (`mark_code`). The interpreter marks its decoded blocks' pages too. |
| P8.2 | SoftMMU write detection | done | `TLB_CODE` on `addr_write` at fill (`mmu::fill`) and for existing entries when a page becomes code (`tlb::set_code_flag`). The store slow path sees the mark in `DirectMem::store`. |
| P8.3 | Direct-mode write protection | done, differently | A guest-writable code page is host read-only. A JIT store faults like any host fault, and the dispatcher rebuilds the state at the store and retires the store in the interpreter. The plan was to "return and retry" from the signal handler; that would let the rest of a possibly stale TB run (§9). |
| P8.4 | Invalidation | done | `Jit::drain_smc`: for each written page, unlink every incoming chain of each TB, invalidate the TB, stale the jump cache. Both engines stop right after *any* store to a code page, not only one hitting the running TB, so nothing after the store can be stale. |
| P8.5 | FENCE.I and flush syscall | done | FENCE.I and `riscv_flush_icache` (`Engine::fence_i`) reset the jump cache only. `--smc=flush-on-fence` restores the full flush as a cross-check. |
| P8.6 | SMC test programs | done | `guest/c/smc.c` cases (a)–(e) (§6). |

## 3. What was built

### 3.1 The code-page mark (`src/mem/direct.rs`)
`DirectMem`'s per-page permission byte gains a CODE bit (0x40, next to MAPPED 0x80).

`mark_code(addr)`:
- sets the bit;
- if the guest may write the page, `mprotect`s it read-only on the host (direct-mode JIT stores then fault);
- returns whether the page was newly marked.

Every write path drops the mark before writing and reports the page in `DirectMem::smc_pages`:
- `store` (interpreter, helpers, softmmu slow paths)
- `slice_mut` (syscalls such as `read` into code)
- `write_bytes` (loader)
- `map`/`unmap`/`protect` (remapping or re-protecting code)

Dropping the mark restores host write access. Reads are unaffected.

In softmmu mode the "page" is the physical page. Aliases (two virtual pages mapping one physical code page) are therefore caught: the fuzzer maps a writable alias of the block's own code page (§6).

### 3.2 Who marks pages
- **JIT:** `tb_for_key` records each new TB's page(s) in `page_tbs` and `new_code`. A direct-mode TB can end with an instruction straddling into the next page, so both pages are recorded. `mark_new_code` runs at the end of `select` and at the start of `exec`, so the page is protected before the TB (or lockstep's reference run) executes. With softmmu it also sets `TLB_CODE` on existing write entries for the page.
- **Interpreter:** `Interp::block` calls `mark_block_code` for every block it caches. Otherwise the interpreter engine would itself run stale decoded blocks (it did, before this phase: §1).

### 3.3 Stopping after the store
A store to a code page must not let the rest of its TB run: that TB, or a TB chained after it, may be what was just overwritten. Four places stop:

| Where the store executes | How it stops |
|---|---|
| interpreter (`exec_block`) | after an instruction, `mem.smc_pages` is non-empty → end the block (`pc` = next) |
| `helper_interp_one` (AMOs, CSRs, FP stores in the slow variant, the naive back end) | `Flow::Next` with `smc_pages` non-empty → retire, `pc` = next, exit reason 9 `SMC` |
| softmmu inline store | `TLB_CODE` → slow path → `helper_mmu_access` performs the store, sets exit reason 10 `SMC_STORE` → the stub's fault exit (with `fault_rip` = the site). The dispatcher applies the site's state map (D37), sets `pc` = site pc + length, and counts the store as retired |
| direct-mode inline store | host SIGSEGV on the read-only page → `fault_exit` → `exec` sees `HOST_FAULT` on a guest-writable code page (`is_smc_fault`) → `smc_host_fault` rebuilds the state at the store and runs that one instruction through the interpreter (which drops the mark and writes) |

### 3.4 Invalidation (`Jit::drain_smc`)
It runs at the start of `select` and after every `exec`. For each page in `smc_pages`, it takes the page's TB list, and for each still-valid TB:
- `chain::unlink_incoming` points every linked exit back at its own stub;
- `TbCache::invalidate` removes the TB from the map;
- `last_exit` is dropped if it pointed at the TB.

Then it bumps the jump-cache version, clears the page's `TLB_CODE` flags and empties the list. The dead code stays in the buffer until the next full flush, and a running TB is never freed. A page is re-marked when something is translated from it again.

### 3.5 FENCE.I (D49)
Everything stale was invalidated when it was written, so FENCE.I only has to reset the jump cache. That matters for Phase 9: Linux executes `fence.i` (and SBI remote fence.i) every time it maps executable pages, and a full flush would throw away every translation each time.

## 4. Design decisions made

- **D49 (new):** the mechanism above, with its exit reasons 9 and 10.
- **Stop after every code-page store, not only a self-hit (§16 said "the currently executing TB").** Knowing whether a later instruction of the running TB, or a chained successor, is on the written page needs per-TB bookkeeping in the hot path. Stopping after any code-page store costs one dispatcher round trip per such store, and those are rare: CoreMark has none.
- **Retire the faulting store in the interpreter** instead of returning from the signal handler (P8.3). Returning would resume the possibly stale TB. Going through the dispatcher reuses the precise-fault machinery and needs no new signal-handler logic.
- **The interpreter tracks code too.** As the reference engine, it must see code writes the same way, and lockstep compares the two, so both stop at the same instruction.
- **Lockstep:** the reference run drops the marks of pages it writes, and the JIT run must find them marked. Lockstep re-marks those pages (and their `TLB_CODE` flags) after undoing the reference run's writes.

## 5. How it works: worked example (case c, a self-patching loop)

The guest code at `buf+256` (in an RWX mmap):
```
  c[0]  addi a0, zero, 0
  c[1]  addi t0, zero, 100
  c[2]  addi a0, a0, 1        <- the immediate is patched every iteration
  c[3]  lw   t1, 8(a1)        ; a1 = &c[0]: loads c[2]
  c[4]  lui  t2, 0x100        ; +1 in the I-immediate
  c[5]  add  t1, t1, t2
  c[6]  sw   t1, 8(a1)        ; store into c[2]: the loop's own code
  c[7]  addi t0, t0, -1
  c[8]  bne  t0, zero, c[2]
  c[9]  ret
```

**Iteration 1**, direct-mode JIT:
1. The dispatcher translates the TB at `c[2]` (`c[2]..c[8]`). `mark_new_code` makes its page read-only on the host.
2. The TB runs until `sw` (`c[6]`), which faults on the host.
3. `exec` sees `HOST_FAULT` on a guest-writable code page. `smc_host_fault` applies the state map at the `sw` (a0, t0, t1, t2 go home), sets `pc = c[6]` and refunds the budget of `c[6..8]`.
4. The interpreter executes the `sw`: `DirectMem::store` drops the mark (the page is writable again), reports the page and writes `addi a0, a0, 2` into `c[2]`. `pc = c[7]`.
5. `drain_smc` invalidates every TB on the page. That is the loop TB, plus the TB at `c[0]` from the call, the `buf+0` TBs of cases (a)/(b) and case (d)'s TB if they share the page. It unlinks the loop TB's self-link.
6. The dispatcher translates `c[7]..c[8]` (the page is marked again), and `bne` exits to `c[2]`. A **new** TB is translated from the new bytes, which adds 2.

**Iteration 2 onwards:** the same thing happens every iteration. The result is 1 + 2 + … + 100 = 5050, and 15050 for the second call, which continues from 101.

Before this phase, the chained self-loop kept executing the first translation (`+1`) until the budget expired. The program printed `c: 199 199`.

`--stats` for the whole program (both backends run the same 255 SMC events):
```
direct : ... host-fault 255, smc 0   ... SMC: 255 code-page writes, 456 TBs invalidated
softmmu: ... host-fault 0,   smc 255 ... SMC: 255 code-page writes, 456 TBs invalidated
```
The 29 FENCE.I-equivalent calls (`__builtin___clear_cache` → `riscv_flush_icache`) now only reset the jump cache: `0 full ... flushes`.

## 6. Tests and verification

| Test | What | Result |
|---|---|---|
| `guest/c/smc.c` in `tests/user_programs.rs` | (a) write, flush, call, rewrite, flush, call; (b) the same without any flush, 1000 calls per version (translated and chained); (c) the self-patching loop above, twice; (d) a guest JIT generating `f_k(x) = kx + 3k` 50 times at one address (flush on even k only), 20 calls each, with a per-call check; (e) A on page 1 jumps (`jal`) to B on page 2, 1000 calls; B's page rewritten, 1000 more. Byte-exact against the recorded `qemu-riscv64` output, in 4 builds (-O0/-O2 × rv64gc/rv64imafd) under interp, jit, lockstep, no-chain, 64 KiB code cache, mprotect W^X, regalloc none/pinned, lockstep unpinned, and `--mem=softmmu` with interp/jit/lockstep/regalloc none | pass |
| same program, also by hand | `--smc=flush-on-fence` | same output |
| same program, pre-Phase-8 binary (`87c35f3`) | shows the tests catch stale code | `b: 43000 43000 c: 199 199 d: 240000 500 e: 1000 1000` (wrong under every engine) |
| `tests/jit_lowering.rs::link_then_invalidate_unlinks` (rewritten) | A (page 1) chained to B (page 2); `write_bytes` into B's page reports it; the next run invalidates only B, unlinks A's exit (A stays valid), A exits through its stub and gets linked to the new B; `invalidate_pc` still works | pass |
| `tests/softmmu.rs` + `Kind::CodeAlias` | a data page that is a writable alias of the block's own physical code page: stores through it are SMC, both engines stop after them, the warm round re-decodes the modified code; code page contents compared too. 1,938 invalidating runs per 3,000 cases (measured once) | 1,000,000 cases clean after the MMIO fix (§8) |
| everything else | `BRIDGEV_REQUIRE_GUESTS=1 cargo test --release` (244 riscv-tests × 8 configs, fuzzers, …) | all pass |

**Acceptance criterion:** ✅ all SMC programs pass under the direct and softmmu backends, jit and lockstep, with chaining enabled.

## 7. Performance

SMC tracking is free when code and data live on separate pages. `--stats` on CoreMark shows 0 code-page writes and 0 host faults in both backends, so the only cost is one `mprotect` per code page when it is first translated. Harness numbers for CoreMark/Dhrystone at the Phase 8 commit (same batch as `jit+linear` at the last Phase 7 commit):

| workload | config | Phase 7 (`6c95332`) | Phase 8 (`4715280` + MMIO fix) | change |
|---|---|---:|---:|---:|
| CoreMark (it/s) | direct (`jit+linear`) | 13,789 (13,512–14,074) | 13,923 (13,672–14,242) | +1.0% |
| | softmmu | 7,498 (7,440–7,574) | 7,750 (7,712–7,948) | +3.4% |
| Dhrystone (runs/s) | direct | 21,803,943 | 21,777,166 (21,562,873–21,963,635) | −0.1% |
| | softmmu | 10,856,731 | 10,343,653 (10,280,872–10,706,242) | −4.7% |

These are two separate batches, and differences of a few percent in either direction are within this VM's run-to-run noise (compare the two direct CoreMark rows of the Phase 7 report: 14,144 vs 13,789 for identical code). Nothing on the hot path changed:
- The direct-mode store is still one `mov`; code pages are protected by the host MMU.
- The softmmu store probe is unchanged; `TLB_CODE` only changes which tag a code page gets.

Raw data: [`../bench/2026-09-27-4715280-smc/`](../bench/2026-09-27-4715280-smc/). The JSON says "dirty tree" because of this report and the not-yet-committed MMIO fix.

## 8. Bugs found and fixed

| Symptom | Root cause | Fix | Regression test |
|---|---|---|---|
| `smc.c` cases b–e wrong under every engine before this phase | no invalidation without FENCE.I (and the interpreter cached decoded blocks forever) | this phase | `tests/user_programs.rs` |
| `link_then_invalidate_unlinks` failed after the change | the test wrote B's new code through `write_bytes` into the same page as A: now both are invalidated automatically | test rewritten for the new behaviour, with B on its own page (case e at unit level) | itself |
| softmmu fuzzer at 1M: device log `jit (…, 0xe4)` vs `interp (…, 0xffffffffffffffe4)` for a byte MMIO store | the interpreter passed the whole register to `Mmio::write`, the JIT slow path a size-truncated constant | `DirectMem::mmio_write` masks the value to the access size | `tests/softmmu.proptest-regressions` |

## 9. Deviations from the plan / spec

- **P8.3:** no return from the signal handler; the store is retired in the interpreter (§4).
- **§16's `SMC_SELF`:** replaced by stopping after every code-page store, with exit reasons 9/10 (§4).
- **`src/mem/smc.rs`:** now only documents where the pieces live. The state belongs to `DirectMem` (marks), the TLB (flags) and the `Jit` (`page_tbs`).
- **Granularity:** the unit is the page, not the TB's byte range, so a data write to a page that also holds code invalidates that page's TBs. QEMU does the same.

## 10. Known limitations and technical debt

- **Mixed pages ping-pong.** A page holding both hot code and frequently written data alternates between write-protected (after translation) and SMC stops. Each costs an `mprotect` pair in direct mode. Small hand-written guests can do this. Compilers and linkers separate `.text` from `.data` by page, so CoreMark, Dhrystone and glibc don't.
- **TB lists are only cleaned lazily.** `page_tbs` entries for TBs invalidated some other way (`invalidate_pc`) remain until the page is written (they are skipped as invalid).
- **No bound on marked pages.** Marked pages stay marked after a full code-cache flush (the next write just finds no TBs).

## 11. How to reproduce

```
cargo build --release && tools/build-guests.sh
target/release/bridgev run --engine jit --stats guest/build/smc-O2.elf
target/release/bridgev run --engine jit --mem softmmu --stats guest/build/smc-O2.elf
qemu-riscv64 guest/build/smc-O2.elf            # the reference
BRIDGEV_REQUIRE_GUESTS=1 cargo test --release
PROPTEST_CASES=1000000 cargo test --release --test softmmu
```

## 12. Next steps

Phase 9 (boot Linux to a BusyBox shell):
- CLINT/PLIC/UART/syscon devices on the `Mmio` bus from Phase 7
- the built-in SBI (TIME, IPI, RFENCE, HSM, SRST, BASE, DBCN, legacy console)
- an FDT generator
- the boot flow, with timer and UART interrupts delivered by the dispatcher

Guest images: kernel.org and GitHub are blocked by the container's network policy (HTTP 403), but the Ubuntu archive works. The plan is Ubuntu 24.04's riscv64 kernel package (`linux-image-6.8.0-60-generic`, a 38.6 MB EFI-stub `Image` with the needed drivers built in) and `busybox-static` 1.36.1, packed into an initramfs. That deviates from D17 (Linux 6.6 built from source) and will be recorded as a decision.
