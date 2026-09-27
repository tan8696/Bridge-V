# 20 · Interview and internship prep

## What you will learn

- how to explain Bridge-V in **30 seconds**, **2 minutes** and **10 minutes**
- resume bullets you can defend, number by number
- the whiteboard drills interviewers like (and the exact answers)
- about 25 likely questions with short, correct answers
- three true "hard bug" stories from this project
- how to answer "what's unique about it?" honestly
- the mistakes that make a strong project sound weak

This file assumes you have read files 01–19 and 23. Every number here comes from `docs/BENCHMARKS.md` or a phase report, with its source in brackets. **Never quote a number you can't point to.**

---

## 1. The 30-second version

> "Bridge-V is a dynamic binary translator I built in Rust. It runs RISC-V Linux programs, and even boots a whole RISC-V Linux system, on an ordinary x86 PC. It works like QEMU or Apple's Rosetta 2: it translates blocks of RISC-V machine code into x86 machine code while the program runs. I wrote the x86 encoder by hand, chained the translated blocks together, pinned hot registers, and emulated the RISC-V virtual memory with a software TLB. It runs CoreMark at 4.8 billion guest instructions a second, 1.5× faster than QEMU. It boots Linux to a shell faster than QEMU too. I checked it against a reference interpreter on every block of a full Linux boot."

Practise this until it takes 30 seconds without notes. It answers *what*, *how*, *how well* and *how do you know*.

---

## 2. The 2-minute version: follow one block

Draw this while you talk (file 01, file 05):

```
 RISC-V program ──► decode ──► IR ──► optimize ──► allocate registers ──► x86 bytes ──► code cache
                                                                                           │
            dispatcher ◄── exit (unlinked, ECALL, fault, budget) ◄── run natively ◄────────┘
                 │                       ▲
                 └── chaining: patch the exit to jump straight to the next block
```

1. **The dispatcher** looks up the guest pc. Brand-new code runs in the interpreter first (file 23). Once a block has run 32 times, it is translated.
2. **Translation:** decode → IR → four optimizer passes → register allocation (4 guest registers pinned in R12–R15, linear scan for the rest) → hand-written x86 encoder → bytes in a code buffer that is mapped twice, writable in one view and executable in the other (W^X).
3. **Run:** a trampoline enters the code. Blocks jump straight into each other through patched jumps (chaining) and an inline jump cache for returns, so control rarely comes back: about 10 times per million guest instructions.
4. **Memory:** user programs use host memory directly. In system mode every load and store checks a software TLB in 9 inline x86 instructions, and a miss walks the RISC-V page table in Rust.
5. **Correctness:** writes to code pages invalidate the translations from them (self-modifying code). Faults inside generated code are made exact with per-access "state maps". And lockstep mode compares the JIT with the interpreter after every block.

---

## 3. The 10-minute version: pick a deep dive

Interviewers usually let you choose. Pick one of these, and know it to the byte:

| Deep dive | What to draw | Study file |
|---|---|---|
| **Block chaining** | the exit stub, the rel32 patch, the incoming list for unlinking; why the rel32 field is 4-byte aligned | 10 |
| **Register allocation** | pinned R12–R15, lazy write-back, spill slots, a fault-site state map | 08, 12 |
| **Software MMU** | the Sv39 walk, the TLB entry, the 9-instruction hit path | 11 |
| **Tiered translation** | the counter, the break-even formula, the U-shaped boot-time curve | 23 |

Structure for any deep dive: **problem → naive solution → why it's slow or wrong → what I built → how I measured it → what I'd do next.**

---

## 4. Resume bullets (all measured)

The canonical list is in CLAUDE.md §28.1. Short versions:

- *Built a RISC-V (RV64GC) to x86-64 dynamic binary translator in Rust. It runs unmodified Linux binaries at 4.8 billion guest instructions/s on CoreMark, 1.49× `qemu-riscv64` and 52.5% of native, and boots Linux 6.8 to a shell.* [Phase 11, `567255a`]
- *Cut dispatcher round trips from 180,762 to 10 per million instructions by hot-patching aligned rel32 jumps between translated blocks: CoreMark 8.9× faster than unchained.* [Phase 5]
- *Hand-wrote the x86-64 encoder and a linear-scan register allocator with 4 pinned registers and precise-fault state maps: 1.9× on CoreMark over the chained baseline.* [Phase 5]
- *Implemented Sv39/Sv48 virtual memory with an inline software TLB: 0.34 ns per hit (throughput) against 28 ns per page walk.* [Phase 7]
- *Added an interpreter tier: measured that 31% of the blocks in a Linux boot run once, then translated only blocks that run 32+ times (the ski-rental break-even). Boot 18% faster (1.33× `qemu-system-riscv64`) with 72% less generated code.* [Phase 12, GitHub runner]
- *Verified with all 244 riscv-tests, lockstep differential execution over a full Linux boot (84 M blocks, no divergence) and 10⁶-case fuzzers.* [Phases 4, 6, 9]

Choose three or four. Put the strongest number first.

---

## 5. Whiteboard drills (practise until automatic)

**5.1 Encode `jne` to a target** (CLAUDE.md §28.5, checked by `tests/emitter_golden.rs::interview_examples_28_5`)
- `jne rel32` is `0F 85` followed by a 4-byte little-endian offset. The offset is counted from the *end* of the instruction.
- At 0x…0010, jumping to 0x…0200: rel = 0x200 − (0x010 + 6) = 0x1EA, so the bytes are **`0F 85 EA 01 00 00`**.

**5.2 Encode `cmp r14, rsi`** (guest `bne a0, a1`, with a0 pinned in R14 and a1 allocated to RSI)
- REX prefix `0x49` (W = 64-bit, B = extends the r/m register to R14).
- Opcode `39` (`CMP r/m64, r64`).
- ModRM `11 110 110` = `0xF6`.
- Result: **`49 39 F6`**.

**5.3 A chain patch.** `jmp rel32` at 0x…1040 to a block at 0x…2000. The rel32 field would start at 0x1041, which is not 4-byte aligned, so the emitter first adds a 3-byte NOP (`0F 1F 00`): **`0F 1F 00 E9 B8 0F 00 00`**. Why aligned? An aligned 4-byte store is atomic on x86, so the patch can never be seen half-written.

**5.4 The translation cache layout** (CLAUDE.md §28.3). Draw:
- the code buffer: trampolines at the start, then the blocks, each with its prologue (budget check), its body, the exit jumps and its cold stubs;
- the Rust side: `tb_map`, `page_tbs` and the jump cache.

**5.5 Self-modifying code in four sentences** (file 13)
1. Translating from a page marks it as a code page.
2. A write to that page is caught: by a TLB flag in system mode, or by host write protection and a SIGSEGV in user mode.
3. The handler invalidates the page's blocks and un-patches every jump into them.
4. Execution resumes right after the store, and the new code is translated fresh.

---

## 6. Likely questions and short answers

**About the design**
- *Why RISC-V → x86 and not the other way?* RISC-V has no condition flags, so there's no flag emulation. x86's memory model is stronger than RISC-V's, so most fences are free. The hard part is x86's smaller register file, hence pinning plus linear scan.
- *Why pin guest registers in callee-saved R12–R15?* They survive calls into Rust helpers and persist across chained blocks with no loads or stores. The set (sp, ra, a0, a5) was kept after alternatives chosen from register-use counts were measured and not clearly better (D42).
- *Why does chaining help so much?* Returning to the dispatcher costs an indirect jump whose target changes on every block, which the branch predictor misses often. A patched direct `jmp` is perfectly predictable.
- *How do you handle indirect jumps (returns)?* An inline jump cache: hash the guest pc into a 4,096-entry table and `jmp [entry]`. A miss exits to the dispatcher.
- *How are exceptions precise when registers are written back lazily?* Each load or store records a "fault site" at translation time: where every dirty guest register lives at that point. The SIGSEGV handler saves the host registers, and the dispatcher uses the site's map to rebuild the exact guest state. The hot path pays nothing.
- *Why a direct-mapped TLB?* One compare, about 5 instructions, no search. Misses are cheap because the walk is in Rust.
- *Why can't a block span two pages?* Page mappings change without the code changing (context switches), so each page must be validated through the TLB separately.
- *Why do you interpret new code before translating it?* Translating a block costs about 8 µs and interpreting a run about 0.1 µs, and a third of boot blocks run only once. Interpreting until the cost of interpreting equals the cost of translating (32 runs) is the ski-rental rule. The boot got 18% faster. (file 23)
- *How do floating-point results stay exact?* SoftFloat, the same library Spike uses, is the reference. Inline SSE is used only where it gives identical bits, with fix-ups for NaNs and saturation. A 10⁶-case fuzzer checks every bit and every flag. (file 14)

**About correctness and testing**
- *How do you know it's correct?* An independent oracle for every layer:
  - LLVM for the decoder;
  - iced-x86 for the encoder;
  - an IR evaluator for the optimizer;
  - the interpreter for the JIT (lockstep, fuzzers);
  - SoftFloat for floating point;
  - QEMU for whole programs.
- *What is lockstep?* After every block, compare the JIT's full architectural state with the interpreter's from the same starting state. It ran over a complete Linux boot without divergence.
- *What's your worst bug?* See §7.

**About performance**
- *Is it faster than QEMU?* On CoreMark, Dhrystone and the FP benchmark in user mode: 1.49×, 4.36× and 5.83×. Booting Linux: slightly faster before Phase 12, 1.33× after it on a GitHub runner. QEMU runs multithreaded programs in parallel and Bridge-V doesn't, so on those QEMU wins.
- *Where does the rest of the time go (52.5% of native)?* Nobody has measured the split exactly, so say "the likely causes are":
  - fewer registers than native code gets;
  - a budget check per block;
  - write-backs of dirty registers at block exits;
  - returns through the jump cache instead of the CPU's return predictor.
- *What didn't work?* Loop-resident registers doubled a toy loop but didn't move the real benchmarks, so they were reverted (D45). A different pinned-register set wasn't a clear win (D42). A return-address stack and superblocks were analysed and not built (Phase 10). Keeping only measured wins kept the code small.

**About you**
- *What did you learn?* Measure before and after every change, and build an oracle before building the thing it checks.
- *What would you do next?*
  - Run guest threads in parallel: needs a shared translation cache and atomic AMOs.
  - Make the interpreter tier cheaper per run.
  - Keep translations across runs (a persistent code cache).

---

## 7. Three true "hard bug" stories

Use the STAR shape: **S**ituation, **T**ask, **A**ction, **R**esult.

**1. The device that was written twice** (D52, Phase 9)
- **Situation:** lockstep ran the interpreter and the JIT on every block of a Linux boot.
- **Task:** find why it reported a divergence after 77.5 million identical blocks.
- **Action:** a device register (the interrupt controller's enable word) was being read-modified-written *twice*, once by each engine. The fix: the reference run records its device accesses, and the JIT run replays the reads and compares the writes.
- **Result:** the whole boot and shell session runs clean under lockstep (84 M blocks).

**2. The slowdown nobody noticed** (D61, after Phase 11)
- **Situation:** the interpreter measured about 10% slower than in Phase 7.
- **Task:** find out whether it was a code regression or just a different cloud machine.
- **Action:** same-batch A/B runs of old and new builds, taking turns, isolated two Phase 8 self-modifying-code checks: one after *every* instruction, and extra page lookups on every store. The fix: only stores check, and the rare code-page case moved to a cold function (`store_slow`).
- **Result:** back to Phase 7 speed. Also a lesson: numbers from different days on a shared VM are not comparable to better than 10–15%.

**3. The tests that would have lied** (Phase 12)
- **Situation:** tiered translation became the default.
- **Task:** make sure the test suite still tested the JIT.
- **Action:** several tests ran tiny programs whose blocks run once. They would have passed while testing only the interpreter, including the test that checks every ALU instruction's translation. Those tests now ask for `--tier 0` explicitly.
- **Result:** the tests kept their meaning, and one boot test keeps full translation covered.
- **Lesson:** when you change a default, ask which tests *change meaning*, not just which ones fail.

---

## 8. "What makes your project different from QEMU?" (answer honestly)

Good, defensible differences:
- **A hand-written x86 encoder.** No JIT library; golden-tested against an independent decoder.
- **An interpreter tier** that QEMU's TCG doesn't have: QEMU translates every block on first use. *But* the idea is old (Java HotSpot, JavaScript V8, HP Dynamo), so say "I applied and measured it", not "I invented it".
- **Lockstep over a full Linux boot**, with device accesses recorded and replayed. It is an unusually strong correctness check for a hobby-scale project.
- **Every design decision is logged with a measured reason** (63 entries), including ideas that were rejected because the numbers said no.

Avoid:
- "It's faster than QEMU" with no workload named. Give numbers per workload, and mention parallel threads, where QEMU wins.
- "No one has ever done X." Interviewers often know the counterexample.
- Numbers from two different machines in one comparison.

---

## 9. A one-week preparation plan

| Day | Do |
|---|---|
| 1 | Say the 30-second and 2-minute versions out loud 5 times each. Record yourself. |
| 2 | Redo the four whiteboard drills of §5 on paper without looking. |
| 3 | Pick your deep dive (§3). Explain it to a friend, or a rubber duck, in 10 minutes. |
| 4 | Answer every question in §6 out loud. Mark the ones you stumble on and reread their study files. |
| 5 | Tell the three bug stories of §7 in STAR form, 90 seconds each. |
| 6 | Run the experiments in file 22 so you have *seen* the numbers. |
| 7 | Mock interview: have someone ask random questions from this file and files 17, 18, 23. |

---

## Check yourself

1. Say the 30-second version without notes.
2. Encode `jne` from 0x…0010 to 0x…0200. Show the arithmetic.
3. Why is the chain-patch rel32 field aligned to 4 bytes?
4. Give the break-even formula behind the default `--tier 32`, with numbers.
5. Name three oracles used to test Bridge-V and what each checks.
6. Tell the lockstep device bug as a STAR story.
7. How do you honestly answer "Is it faster than QEMU?"
8. Which of your resume bullets would you lead with for a performance-engineering role, and which for a compilers role?
