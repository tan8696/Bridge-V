# 24 · Your brag doc: what Bridge-V achieved, with proof

## What you will learn

- what a brag doc is, and how to use this one
- how to say honestly who wrote the code (read §0 before anything else)
- the scoreboard: every headline number, the machine it was measured on, and the file that proves it
- everything Bridge-V can do, and how its correctness was proven
- what the repository shows that *you* did
- ready-made lines for a resume, an interview and a LinkedIn post
- the numbers you must never say

A **brag doc** is a list of your wins, each with its proof. You add to it when something happens, because a few months later you won't remember the details. When you need a resume line, an interview story or a cover letter, you copy from it instead of trying to remember.

File 20 teaches you how to *talk* about Bridge-V. This file is the store room that file 20 takes from: everything, each item with a link to its proof. Every number here was checked against its source file on 2026-09-29, at commit `6632aaf`. **If a line has no proof, don't use it.**

---

## 0. Read this first: say how Bridge-V was built

Anyone can open the repository's history on GitHub. It shows:

| Fact (from `git log`) | Value |
|---|---|
| Commits | 67, from 26 Sep 10:34 to 27 Sep 22:51 (UTC): about 36 hours |
| Commits ending with `Co-Authored-By: Claude` | all 67 |
| Commits whose author is "Claude" (cloud sessions) | 59 |
| Commits whose author is you (`tan8696`, from your PC) | 8: the Phase 12 work, the study guide and `AGENTS.md` |

So the code was written by Claude Code, an AI coding agent, working to the plan and rules in `CLAUDE.md`. An interviewer who opens the repository sees this in one click.

That is not a weakness *if you say it first*. Getting an AI agent to build a 20,000-line systems program that is tested, measured and documented is a real skill. It becomes a big weakness if the interviewer finds out after you said "I wrote it".

The rule: use **"I"** only for what you did yourself (§7). For the code, say **"Bridge-V does…"** or **"I built it with Claude Code"**.

| Don't say | Say instead |
|---|---|
| "I wrote the x86 encoder by hand." | "Its x86 encoder is hand-written, with no assembler library. I can show you how it encodes a jump, byte by byte." |
| "I built Bridge-V." (and stop there) | "I built Bridge-V with Claude Code. The agent wrote the code, and every phase had to end with tests, measured numbers and a written report. Then I studied it part by part." |
| "My register allocator…" | "Bridge-V's register allocator…" |

The honest version is also the stronger answer. It moves the interview to the question that matters, *do you understand it?*, and files 00–23 are how you get to "yes". File 20's 30-second version and resume bullets use this wording too.

---

## 1. The one-paragraph version

> Bridge-V is a dynamic binary translator in Rust. It runs programs made for RISC-V processors on an ordinary x86-64 PC by translating their machine code into x86 machine code while they run, the way QEMU and Apple's Rosetta 2 do. It runs Linux programs and boots an unmodified Ubuntu Linux 6.8 kernel to a shell. On CoreMark it executes 4.8 billion RISC-V instructions per second, 1.49× the speed of QEMU. A lockstep check compared it with a reference interpreter after every one of the 84 million blocks of a Linux boot and found no difference. I built it with Claude Code in 13 phases over about a day and a half, and every phase ended with tests, measured numbers and a written report.

---

## 2. The scoreboard

Numbers from different machines **must never be compared with each other**, so there are two tables.

**Table A. Intel Xeon @ 2.10 GHz, a shared cloud machine (Phases 0–11)**

| What | Result | Compared with | Proof |
|---|---|---|---|
| CoreMark (a standard CPU benchmark) | 13,409 iterations/s, which is 4,807 million RISC-V instructions/s | 1.49× `qemu-riscv64` (8,988); 52.5% of the same C code compiled for x86 (25,531) | [BENCHMARKS: final results](../BENCHMARKS.md#final-results-phase-11-2026-09-27-commit-567255a) |
| Dhrystone (an older CPU benchmark) | 20,728,869 Dhrystones/s | 4.36× QEMU (4,758,387) | same |
| fpbench (a floating-point benchmark) | 3,711 units/s | 5.83× QEMU (636) | same |
| Linux boot to a shell | 1.48 s | QEMU 1.54 s; Bridge-V's own interpreter 10.49 s | same |
| JIT against Bridge-V's interpreter | 26.5× (CoreMark), 39.6× (Dhrystone) | | [BENCHMARKS: Milestone A](../BENCHMARKS.md#milestone-a-coremark-and-dhrystone-user-mode-2026-09-26-commit-668723f) |

**Table B. GitHub Actions runner, AMD EPYC 7763 (Phase 12)**

| What | Result | Compared with | Proof |
|---|---|---|---|
| Linux boot to a shell | 1.06 s, down from 1.30 s (−18%) | QEMU 1.41 s, so 1.33× faster | [BENCHMARKS: tiered translation](../BENCHMARKS.md#tiered-translation-phase-12-d63-2026-09-27-commits-53f9030-and-2e498ee) |
| x86 code generated during the boot | 12,631 KiB, down from 44,501 KiB (−72%) | | same |
| Time spent translating during the boot | 154.2 ms, down from 560.7 ms | | same |

The raw data behind every number (each run, the machine, the commit) is in [`docs/bench/`](../bench/).

---

## 3. What each technique bought

Each row compares two measurements from the same kind of machine: rows 1–5 the Xeon, row 6 the GitHub runner.

| Technique | In plain words | Before → after | Proof |
|---|---|---|---|
| Block chaining | patch the jump at the end of each translated block so it goes straight to the next block, instead of back to the dispatcher | CoreMark 791 → 7,024 iterations/s (8.9×); trips back to the dispatcher 180,762 → 10 per million instructions | [Phase 5](../phase-reports/phase-05-milestone-a.md), [BENCHMARKS: Milestone A](../BENCHMARKS.md#milestone-a-coremark-and-dhrystone-user-mode-2026-09-26-commit-668723f) |
| Register allocation | keep RISC-V registers in real x86 registers instead of memory | CoreMark 7,024 → 13,498 iterations/s (1.9×) | same |
| Inline floating point | do FP math with x86's own SSE instructions instead of calling a helper function | fpbench 58 → 3,508 units/s (60×), with bit-exact results | [Phase 6](../phase-reports/phase-06-fp-jit.md), [BENCHMARKS](../BENCHMARKS.md) (FP section) |
| Software TLB | a small cache of recent address translations, checked by 9 inline x86 instructions | a hit costs 0.34 ns per access; a page-table walk costs 28.1 ns | [Phase 7](../phase-reports/phase-07-privileged-softmmu.md), [tlb.md](../bench/2026-09-27-2da741e-softmmu/tlb.md) |
| System-mode chaining (D51) | chaining and the jump cache inside a full Linux boot too | trips to the dispatcher 6.5 M → 0.7 M per boot; boot 1.80 → 1.30 s | [Phase 9 §1](../phase-reports/phase-09-milestone-b.md), [BENCHMARKS: Linux boot](../BENCHMARKS.md#linux-boot-to-a-busybox-shell-phase-9-2026-09-27-commit-c24bc9c) |
| Tiered translation (D63) | interpret new code; translate a block only after it has run 32 times | boot 1.30 → 1.06 s | [Phase 12](../phase-reports/phase-12-tiered-translation.md) |

---

## 4. Everything it can do

**Runs Linux programs** (user mode, like `qemu-riscv64`)
- Static programs, and dynamic ones (including PIE) with the standard `ld.so` and C library of the RISC-V cross toolchain: [Phase 10, dynamic ELF](../phase-reports/phase-10-dynamic-elf.md)
- Threads: pthread mutexes, condition variables, thread-local storage, joins: [Phase 10, threads](../phase-reports/phase-10-mt-user.md)
- Signals: handlers, `raise`, `abort`, fault handlers and alternate signal stacks, with output byte-identical to QEMU on the test program: [Phase 10, signals](../phase-reports/phase-10-signals.md)
- Programs that rewrite their own code (self-modifying code): [Phase 8](../phase-reports/phase-08-smc.md)
- A GDB server, so `gdb-multiarch` can set breakpoints in a guest program and single-step it: [Phase 10, GDB stub](../phase-reports/phase-10-gdb-stub.md)

**Emulates a whole computer** (system mode, like `qemu-system-riscv64`)
- Boots an unmodified Ubuntu Linux 6.8 kernel to a BusyBox shell, with a timer, an interrupt controller, a serial console and a power-off device: [Phase 9](../phase-reports/phase-09-milestone-b.md)
- Firmware: its own, written in Rust, or the real OpenSBI firmware: [Phase 10, OpenSBI](../phase-reports/phase-10-opensbi.md)
- Virtual memory with 3-level (Sv39) and 4-level (Sv48) page tables: [Phase 7](../phase-reports/phase-07-privileged-softmmu.md), [Phase 10, Sv48](../phase-reports/phase-10-sv48.md)
- Up to 8 CPUs; the tests boot Linux with 4: [Phase 10, SMP](../phase-reports/phase-10-smp.md)
- A virtual disk (virtio-blk): [Phase 10, virtio-blk](../phase-reports/phase-10-virtio-blk.md)

**Inside the translator** (the four hard parts named in `CLAUDE.md` §1, then the extras)
- Its own x86-64 encoder. It writes machine code byte by byte with no assembler library, and every encoding is checked against the independent iced-x86 decoder: [Phase 2](../phase-reports/phase-02-naive-jit.md)
- Block chaining, with jumps patched in place, plus an inline jump cache for returns: [Phase 3](../phase-reports/phase-03-chaining.md)
- Register allocation: 4 guest registers pinned in R12–R15, a linear-scan allocator for the rest: [Phase 4](../phase-reports/phase-04-ir-regalloc.md)
- A software MMU with an inline TLB: [Phase 7](../phase-reports/phase-07-privileged-softmmu.md)
- Exact (precise) faults with no cost on the fast path, using per-access "state maps": [Phase 4](../phase-reports/phase-04-ir-regalloc.md)
- Safe code memory: the code buffer is mapped twice, writable in one view and executable in the other, never both at once (W^X): [Phase 2](../phase-reports/phase-02-naive-jit.md)
- Tiered translation: [Phase 12](../phase-reports/phase-12-tiered-translation.md)
- Real translated blocks, shown byte by byte: [WHITEBOARD.md](../WHITEBOARD.md)

---

## 5. How correctness was proven

Each check compares Bridge-V with something independent that is known to be right (an "oracle").

| Check | Result | Proof |
|---|---|---|
| The official RISC-V test suite (riscv-tests) | all 244 tests pass, under the interpreter, the JIT at every setting, and lockstep | [Phase 7 §6](../phase-reports/phase-07-privileged-softmmu.md) |
| Lockstep over a whole Linux boot and shell session | 84,207,381 blocks (908 million instructions) compared, no difference | [Phase 9 §6](../phase-reports/phase-09-milestone-b.md) |
| Random-block fuzzer | 1,000,000 random blocks, each under 5 JIT configurations: clean. On the way it found one real register-allocator bug, which is now a regression test | [Phase 4](../phase-reports/phase-04-ir-regalloc.md) |
| Floating-point fuzzer | 1,000,000 cases bit-exact against Berkeley SoftFloat, exception flags included | [Phase 6](../phase-reports/phase-06-fp-jit.md) |
| Whole programs | 14 test programs match `qemu-riscv64` byte for byte (output and exit code) | [`tests/data/expected/`](../../tests/data/expected/) |
| Decoder and encoder | the decoder is checked against LLVM's; the encoder against the iced-x86 decoder | `CLAUDE.md` D22 and D3 |
| Continuous integration (CI) | every push runs the format and lint checks, the tests, riscv-tests and a Linux boot (JIT, interpreter and lockstep); the latest run passed | [`ci.yml`](../../.github/workflows/ci.yml), [run 36356720328](https://github.com/tan8696/Bridge-V/actions/runs/36356720328) |

---

## 6. Engineering habits you can point to

- **A decision log of 63 entries** (D1–D63), each with its reason, often a measurement: [`CLAUDE.md` §3](../../CLAUDE.md)
- **A written report for every phase:** 21 reports, 3,661 lines: [`docs/phase-reports/`](../phase-reports/)
- **No number without proof:** only numbers produced by the benchmark harness go into [`BENCHMARKS.md`](../BENCHMARKS.md#rules-claudemd-22), each with its commit, machine and date.
- **Ideas that lost were removed.** Loop-resident registers doubled the speed of a toy loop but didn't move CoreMark or Dhrystone, so they were reverted (D45). A return-address stack and superblocks were analysed and not built: [Phase 10, not pursued](../phase-reports/phase-10-not-pursued.md)
- **A hidden 10% slowdown was caught and fixed** with interleaved A/B runs (D61): [BENCHMARKS: interpreter fix](../BENCHMARKS.md#interpreter-regression-fix-2026-09-27)
- **Size** (counted with `wc -l` at `6632aaf`): 20,195 lines of Rust in 62 source files, plus 4,320 lines of tests in 15 files with 148 test functions. Some functions run hundreds of cases: the 8 in `tests/riscv_tests.rs` each run all 244 riscv-tests under a different engine or setting. The documentation is 12,780 lines in 82 files.

---

## 7. What the repository shows that you did

List only what you can prove. The repository proves these:

- **You started and own the project.** It began from your brief: `CLAUDE.md` marks core designs as "Taken from the brief", among them the pinned registers (D5), block chaining (D7) and the self-modifying-code handling (D11).
- **You set the rules and made the owner's decisions.** A written report at the end of every phase is an "Owner requirement" (D19). The license, MIT OR Apache-2.0, is "the owner's choice" (D62, [ROADMAP §18](../ROADMAP.md#18-open-questions-for-the-project-owner)).
- **You ran Phase 12 from a Windows PC with no Linux machine.** Every build, test and benchmark ran on GitHub Actions, through a benchmark workflow added in that phase. Result: the Linux boot got 18% faster. Proof: your commits `75c9053` to `3e565ad`, the [Phase 12 report](../phase-reports/phase-12-tiered-translation.md) and [`bench.yml`](../../.github/workflows/bench.yml).
- **You are learning it properly:** this study guide, 25 files and about 21 hours of reading.
- **You finished it:** from the first commit to Phase 12 in about a day and a half, with CI passing.

Add to this list as things happen, one line each: the date, what you did, the proof. For example: an idea you suggested, a bug you noticed, a part you explained to someone without notes. Only you know these, and they are exactly what interviewers ask about.

---

## 8. Copy-ready lines

**Resume** (pick 2 or 3; every number is in §2–§6):
- Built Bridge-V, a RISC-V to x86-64 dynamic binary translator in Rust, with Claude Code as the coding agent: 20k lines in 13 phases. It runs CoreMark 1.49× faster than `qemu-riscv64` and boots Linux 6.8 to a shell.
- Required tests, measured numbers and a written report at the end of every phase (21 reports, a 63-entry decision log). Correctness was verified by lockstep comparison over a full Linux boot (84 M blocks, no divergence), all 244 riscv-tests and 10⁶-case fuzzers.
- Ran the tiered-translation phase from a Windows PC with no Linux machine by moving every build, test and benchmark to GitHub Actions: Linux boot 18% faster (1.33× `qemu-system-riscv64`) with 72% less generated code.

**Interview, 30 seconds:**
> "Bridge-V is a dynamic binary translator: it runs RISC-V programs on an x86 PC by translating their machine code while they run, like QEMU or Rosetta 2. I built it with Claude Code, an AI coding agent. The agent wrote the code, and my rule was that every phase ends with tests, measured numbers and a written report. It runs CoreMark 1.49× faster than QEMU, boots Linux to a shell, and was checked against a reference interpreter on every one of the 84 million blocks of a Linux boot. I've been studying it part by part, so ask me about block chaining or the software TLB."

In the last sentence, name only parts you can explain on a whiteboard (file 20 §3).

**LinkedIn or a short post:**
> I built Bridge-V with Claude Code: a RISC-V to x86-64 dynamic binary translator in Rust. It translates RISC-V machine code into x86 machine code while the program runs, the way QEMU and Rosetta 2 do.
> - CoreMark 1.49× faster than QEMU (4.8 billion RISC-V instructions per second)
> - boots an unmodified Ubuntu Linux 6.8 kernel to a shell
> - checked against a reference interpreter on all 84 million blocks of a Linux boot, with no difference
>
> My part: the brief, the rules (tests, measured numbers and a written report for every phase), and now learning how every part works. Code, reports and benchmarks: https://github.com/tan8696/Bridge-V

---

## 9. Numbers you must never say

- **Numbers from two machines in one sentence.** "The boot went from 1.48 s to 1.06 s" is wrong: 1.48 s is the Xeon and 1.06 s is the GitHub runner. Say "18% faster on the same machine".
- **"4.8 billion instructions per second" without the workload.** That is CoreMark. Dhrystone runs at 6,955 million per second, fpbench at 2,600 million, and a Linux boot averages 592 million ([BENCHMARKS](../BENCHMARKS.md#final-results-phase-11-2026-09-27-commit-567255a)).
- **"Faster than QEMU" without the workload.** Programs with several threads run slower than on QEMU, because QEMU runs the threads in parallel and Bridge-V runs one at a time: multithreaded CoreMark 12,189 against QEMU's 38,314 iterations/s (D55, [data](../bench/2026-09-27-mt-coremark/)).
- **The 29.5× and 43.9× "JIT vs interpreter" figures** in the final results. They were measured while the interpreter had a slowdown (fixed later), so they are up to 10% too high. Use Phase 5's 26.5× and 39.6×.
- **"120 million instructions per second" or "45 → 4 cycles".** These were targets in the brief, not results ([`CLAUDE.md` §28.1](../../CLAUDE.md)).

---

## Check yourself

1. Who wrote Bridge-V's code? Say it in one sentence, the way you would in an interview.
2. What proves "1.49× QEMU"? Name the machine and the commit.
3. Why is "the boot went from 1.48 s to 1.06 s" wrong?
4. Name three things the repository proves you did yourself.
5. Pick your three resume lines. For each number in them, point to its proof.
