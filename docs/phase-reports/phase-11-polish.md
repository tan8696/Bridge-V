# Phase 11 report: Polish, presentation and resume

| Field | Value |
|---|---|
| Phase | 11: Polish, presentation and resume (see `docs/ROADMAP.md`) |
| Status | Complete |
| Dates | 2026-09-27 → 2026-09-27 |
| Branch / final commit | `claude/compassionate-babbage-ul3prl` @ the commit adding this report |
| Sessions used | 1 |

## 1. Summary
Phase 11 turned the finished translator into something that can be shown and defended with evidence:
- the full benchmark matrix re-measured at the final code commit (`567255a`);
- a README built around real run transcripts;
- resume bullets that contain only measured numbers;
- a whiteboard walkthrough made from blocks the JIT actually emitted;
- the §28.5 encoding examples checked by a test against the real emitter.

**Headline:**
- CoreMark runs at 13,409 iterations/s (4.8 billion guest instructions/s), **1.49× `qemu-riscv64`** and 52.5% of native.
- Dhrystone runs at **4.36× QEMU**, and the FP benchmark at **5.83× QEMU**.
- Linux 6.8 reaches a shell in 1.48 s, against QEMU's 1.54 s.

**Two measurement surprises**, both investigated with same-batch A/B runs (§7):
- **The reference interpreter is about 10% slower than at Phase 7**, lost gradually over Phases 8–10. This is real, and a follow-up task was queued.
- **The Linux boot measured slower than in Phase 9** (1.48 s against 1.25 s). This turned out to be the host, not the code.

## 2. Planned vs delivered
| Task | Status | Notes |
|---|---|---|
| README demo + quick start | done | Text transcripts of real runs (CoreMark JIT with `--stats`, `tools/demo-milestone-a.sh`, a Linux boot with commands typed at the prompt) instead of asciinema/screenshots: no terminal recorder or display in the container, and text is greppable and diffable. |
| PROJECT_EXPLAINED with measured results | done | Status line, §4.2, §10 (full final table), §12. |
| Resume bullets with measured numbers only | done | CLAUDE.md §28.1 rewritten: five bullets, each number with its commit. The "120M instructions/s" and "45 → 4 cycles" targets are explicitly retired. |
| Re-derive §28.5 against the real emitter | done | `tests/emitter_golden.rs::interview_examples_28_5`. One correction: the aligned chain exit is padded with **one** 3-byte NOP (`0F 1F 00`), not "3 NOPs". |
| Whiteboard walkthrough from real TB dumps | done | `docs/WHITEBOARD.md`. |
| Optional write-up with lessons learned | done (short) | §13 of this report; `WHITEBOARD.md` is the design write-up. No separate blog post. |

## 3. What was built
- **`tests/emitter_golden.rs::interview_examples_28_5`** runs each CLAUDE.md §28.5 example through the real `Asm` API at the addresses the text uses, and asserts the bytes:
  - `jne` at B+0x10 → B+0x200 = `0F 85 EA 01 00 00`;
  - `cmp r14, rsi` = `49 39 F6`;
  - `jmp` at B+0x1040 → B+0x2000 = `E9 BB 0F 00 00`;
  - the same jump emitted as an aligned chainable exit = `0F 1F 00 E9 B8 0F 00 00`, with the rel32 field at a 4-byte boundary;
  - a `write_rel32` re-patch.

  The interview numbers can no longer drift from the code.
- **`docs/WHITEBOARD.md`** covers three TBs from `fib-O2.elf 20` (`--dump-x86`/`--dump-ir` with `--features disasm`):
  1. a 3-instruction self-loop in glibc start-up, followed from lifted IR through optimized IR to the 125 bytes of x86. The walkthrough explains every offset (`[rbp-0x10]` = x14, pc at +0x80, exit_reason at +0x90) and both chain patches from `links.txt` (the loop's `jne` patched to its own prologue: `d8 ff ff ff`);
  2. `ret` through the inline jump cache;
  3. the same loop under `--mem=softmmu`, showing the 9-instruction TLB hit path and the cold slow path (register save, budget refund, `call [helper table]`, fault exit).

  It ends with the code-buffer layout drawn from those real addresses and the Rust-side metadata for the same TB.
- **README.md** was rewritten:
  - a results table (final run);
  - quick start;
  - three real transcripts;
  - a feature summary;
  - the documents table.
- **BENCHMARKS.md** gains a "Final results" section: all workloads × all configurations at `567255a`, the boot benchmark, and both A/B investigations with raw files.
- **CLAUDE.md:**
  - §0 status (Phase 11 complete);
  - §22 (the TLB microbenchmark note now points to the measurements);
  - §28.1 (resume bullets);
  - §28.5 (padding text, test reference).
- **ROADMAP.md:** Phase 11 ticked.

## 4. Design decisions made
No new architecture decisions (no D-number).

**Presentation choices:**
- **Text transcripts over recordings,** for the reasons given in §2.
- **Conservative ratios in the resume.** The resume bullets cite ratios against QEMU and native, which are independent of our own interpreter's speed. Where they cite our own configurations, they use the Phase 5 per-level figures, because the interpreter got slower later (§7.2). Quoting "29.5× the interpreter" without that caveat would flatter the JIT.

## 5. How it works: worked example
See `docs/WHITEBOARD.md` §1. The short version, for the self-loop at 0x10990 (`c.ld a5,0(a4); c.addi a4,a4,8; c.bnez a5,-4`):
- **Forwarding** removes both register re-reads, leaving 6 IR values.
- **The allocator** puts `a4` in RAX (filled once from `[rbp-0x10]`, written back once at the end) and `a5` in pinned R15.
- **The load** is one `mov rcx,[rbx+rax]`: the fault site, with no check code.
- **The branch** becomes `test rcx,rcx; jne rel32`, with the rel32 at an aligned address.
- **Chaining:** the first taken exit leaves through `stub_1` (exit code 0x11 = TB 4, slot 1). The dispatcher rewrites the rel32 to `0x3b0 − 0x3d8 = −0x28`. From then on the loop is 9 host instructions per iteration, the budget check included.

## 6. Tests and verification
- **New test:** `interview_examples_28_5` passes.
- **`cargo test`** (full suite, at the Phase 11 commits): `134 passed; 0 failed; 9 ignored` over 16 test binaries. The 9 ignored tests are the Linux boot tests (`tests/linux_boot.rs`), which the CI `linux-boot` job runs with `--ignored`.
- **`cargo fmt --check` and `cargo clippy --all-targets -- -D warnings`:** clean, with and without `--features disasm`.
- **Acceptance criteria (ROADMAP Phase 11):**
  - ✅ README demo and quick-start commands (transcripts are real output; commands in §11).
  - ✅ PROJECT_EXPLAINED updated with measured results.
  - ✅ §28.1 contains measured numbers only. Each was checked against `docs/BENCHMARKS.md` or a phase report:
    - 180,762 → 10 dispatcher entries per M instructions, and 791 → 7,024 → 13,498 it/s (Phase 5 table);
    - 0.34 ns / 28.1 ns (`bench/2026-09-27-2da741e-softmmu/tlb.md`);
    - 84,207,381 lockstep TBs (Phase 9 report §6);
    - 244 riscv-tests (Phase 7 report §6);
    - 10⁶ fuzz blocks (Phase 4 report §6) and 10⁶ FP cases (Phase 6 report §6).
  - ✅ §28.5 re-derived by a test; whiteboard walkthrough from real dumps.

## 7. Performance
All raw data is in `docs/bench/2026-09-27-567255a-final/` and `docs/bench/2026-09-27-567255a-boot/`. Host: Intel Xeon @ 2.10 GHz (4 vCPU), `taskset -c 2`, 1 warm-up run + 5 measured runs, median. This is a shared cloud VM with noise of several percent.

### 7.1 Final matrix (`tools/bench.py --suite coremark,dhrystone,fpbench --configs interp,jit+linear,softmmu,qemu,native`)
| workload | interp | jit+linear | softmmu | qemu | native |
|---|---:|---:|---:|---:|---:|
| CoreMark it/s | 454 | 13,409 | 7,526 | 8,988 | 25,531 |
| Dhrystone /s | 472,238 | 20,728,869 | 10,115,192 | 4,758,387 | 45,562,668 |
| fpbench units/s | 118 | 3,711 | 2,654 | 636 | 16,589 |

- **JIT vs QEMU:** 1.49× on CoreMark, 4.36× on Dhrystone and 5.83× on fpbench.
- **softmmu vs direct:** 0.56, 0.49 and 0.72.
- **Translate time:** 0.07–0.12% of each run.
- **Dispatcher entries:** 10 per million guest instructions.

### 7.2 The interpreter slowdown (real)
- **Final matrix:** interp CoreMark 454 it/s, against 520 in the Phase 7 session (`2da741e`). Two builds of mine were running during the first measurement, so the interp cells were re-run on an idle machine: 471 it/s and 465,530 Dhrystones/s, still about 10% down.
- **Same-batch A/B** (`dhrystone-rv64.elf 1500000`, interleaved runs): Phase 7 took 2.84–2.99 s, HEAD 3.13–3.48 s.
- **Five builds interleaved:** f488d24 2.92–3.13, 41fbfee 3.14–3.21, 6b5a7c2 3.10–3.40, 25c9ffd 3.25–3.38, HEAD 3.00–3.29.
- **Conclusion:** a gradual loss of a few percent per phase, likely from the SMC, WFI/interrupt and thread/signal checks added to the interpreter's store and block paths. There is no single culprit commit.
- **Impact:** the JIT is unaffected (13,409 vs 13,498 at Phase 5). Only "vs interp" ratios change, by about 10%.
- **Follow-up:** queued as a separate task ("Recover the interpreter slowdown from Phases 8–10"). Evidence: `bench/2026-09-27-567255a-final/interp-ab.md`, `interp-rerun.md`. **Resolved** in `f7f41aa` (the store path's SMC bookkeeping; see `docs/BENCHMARKS.md`, "Interpreter regression fix").

### 7.3 The boot "slowdown" (host, not code)
**`tools/boot-bench.py --configs jit,interp,qemu --runs 5`:**

| config | time to shell | MIPS |
|---|---:|---:|
| jit | 1.48 s (1.34–1.50) | 592 |
| interp | 10.49 s | 85 |
| qemu | 1.54 s (1.40–1.93) | — |

- **Earlier measurements:** Phase 9 (`c24bc9c`) measured 1.25 s, and the Phase 10 OpenSBI session (`57af1e0`) 1.37 s.
- **`--stats`:** the work is the same, within 1–4%: 926 M vs 929 M instructions, 66.0 k vs 66.7 k TBs, 698 k vs 703 k dispatcher entries, 642 k vs 674 k TLB fills.
- **Same-batch A/B** of the `c24bc9c` build against HEAD, 5 interleaved boots each: medians 1.57 s vs 1.54 s.
- **Conclusion:** the Phase 10 code is as fast as the Phase 9 code, and the host is slower today. Evidence: `bench/2026-09-27-567255a-boot/ab.md`.

## 8. Bugs found and fixed
| Symptom | Root cause | Fix | Regression test |
|---|---|---|---|
| CLAUDE.md §28.5 said the chain exit is padded with "3 NOPs" | The text predates the emitter, which pads with one multi-byte NOP | Text corrected | `interview_examples_28_5` |
| PROJECT_EXPLAINED listed a `bridgev bench coremark` command | The harness is `tools/bench.py` (D43); the subcommand never existed | Usage block corrected | n/a (docs) |

No code bugs were found in this phase.

## 9. Deviations from the plan / spec
- **The README demo is text transcripts,** not an asciinema recording or screenshots (§2).
- **The optional write-up is this report's §13 plus `WHITEBOARD.md`,** not a separate blog post.

## 10. Known limitations and technical debt
- **The interpreter's 10% slowdown since Phase 7** (§7.2) is not fixed in this phase. The follow-up task is queued.
- **The standing limitations from Phase 10** are unchanged:
  - guest threads and SMP harts run one at a time;
  - no return-address stack or superblocks;
  - asynchronous host signals are not forwarded to the guest.
- **Transcripts show one run.** Their timings (e.g. 13,057 it/s, 1.46 s to the prompt) differ from the harness medians, which are the numbers of record.

## 11. How to reproduce
```sh
tools/setup.sh && cargo build --release && tools/build-guests.sh && tools/build-bench.sh
python3 tools/bench.py --suite coremark,dhrystone,fpbench --configs interp,jit+linear,softmmu,qemu,native --out /tmp/final
tools/fetch-guest-images.sh && python3 tools/boot-bench.py --configs jit,interp,qemu --runs 5
tools/demo-milestone-a.sh
cargo test --test emitter_golden interview_examples_28_5
cargo build --release --features disasm
target/release/bridgev run --engine jit --dump-x86 /tmp/d --dump-ir /tmp/d guest/build/fib-O2.elf 20   # whiteboard TBs
```

## 12. Next steps
The roadmap is complete. Candidates, in order of value:
1. Recover the interpreter slowdown (the queued task).
2. Parallel guest execution: threads and harts. This needs a shared, synchronized translation cache and `DirectMem`, plus atomic AMOs (D55's list).
3. Settle the open owner questions in `docs/ROADMAP.md` §18 (license first).

## 13. Lessons learned
- **An oracle for every layer paid for itself.** Every layer had an independent reference:
  - LLVM for the decoder;
  - iced-x86 for the emitter;
  - the IR evaluator for the passes;
  - the interpreter for the JIT (lockstep, fuzzers);
  - SoftFloat for FP;
  - QEMU for guest programs.

  Most bugs were found by a mismatch within minutes of being introduced, not by debugging a crash.
- **Chaining was by far the biggest single win** (8.9×). Pinning and the allocator together added 1.9×. The optimizations that "should" help but didn't pay on the benchmarks (loop-resident registers, D45; use-count register pinning, D42; a return-address stack and superblocks) were measured or analysed and left out. Keeping only what wins on the benchmarks kept the code small.
- **Lockstep needed more than register comparison.** Device reads-modify-writes ran twice until MMIO was recorded and replayed (D52), and time had to be made deterministic (D29). Without both, a whole-system lockstep boot would have been impossible.
- **Precise faults do not need hot-path code.** Recording per-site state maps at translation time and resolving them in the dispatcher (D37) kept direct-mode loads and stores at one instruction each, and the same maps later made softmmu page faults precise.
- **Measure, then explain.** Two surprising numbers in this phase had opposite explanations: a real regression in one case, host drift in the other. Only same-batch A/B runs told them apart. Numbers from different sessions on a shared VM are not comparable to better than about 10–15%.
