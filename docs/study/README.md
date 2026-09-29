# Bridge-V study guide: start here

This folder explains **everything about Bridge-V** in plain English, from zero.
You don't need to know anything about CPUs, assembly, compilers or operating systems before you start.
Every new word is explained the first time it appears, and [21-glossary.md](21-glossary.md) collects all of them.

Read the files in order. Each one builds on the ones before it.
Each file starts with **"What you will learn"** and ends with **"Check yourself"** questions. If you can answer those questions out loud, you have understood the file.

---

## Reading order

### Part A: the background (read these first)

| # | File | What it covers | Time |
|---|---|---|---|
| 00 | [Beginner foundations](00-beginner-foundations.md) | Binary and hex, bits and bytes, what a CPU, register, memory, instruction, compiler, operating system and emulator are. The small amount of Rust you need to read the code. | 2–3 h |
| 01 | [The big picture](01-big-picture.md) | What Bridge-V does, told as a story. The main parts and how they fit. | 30 min |
| 02 | [RISC-V, the guest](02-riscv-the-guest.md) | The machine language Bridge-V *reads*: registers, instruction formats, a byte-by-byte decoding example, compressed instructions, privilege levels. | 1 h |
| 03 | [x86-64, the host](03-x86-the-host.md) | The machine language Bridge-V *writes*: registers, calling convention, how an x86 instruction is built from bytes (REX, ModRM, SIB), jumps. | 1 h |

### Part B: the code and how it flows

| # | File | What it covers | Time |
|---|---|---|---|
| 04 | [Code map](04-code-map.md) | Every folder and source file: what it does and what order to read it in. | 30 min |
| 05 | [Code flow: one program, start to finish](05-code-flow.md) | A trace of `bridgev run --engine jit hello.elf` through the real functions, from `main()` to the exit code. | 1 h |
| 06 | [The interpreter](06-interpreter.md) | The simple, slow, always-correct engine: decoding, blocks, `step()`. | 45 min |
| 07 | [The JIT pipeline](07-jit-pipeline.md) | How a block of RISC-V becomes x86 bytes: lift → IR → optimizer passes → register allocation → emitter. | 1.5 h |
| 08 | [Register allocation](08-register-allocation.md) | Fitting 32 guest registers into 10 host registers: pinning, linear scan, lazy write-back, spills, state maps. | 1 h |
| 09 | [Code memory and trampolines](09-code-memory-and-trampolines.md) | Making memory executable safely (W^X), entering and leaving generated code, the block-boundary rules. | 45 min |
| 10 | [Block chaining and the jump cache](10-chaining-and-jump-cache.md) | The biggest speed-up: patching jumps so blocks run straight into each other. | 1 h |

### Part C: the hard subsystems

| # | File | What it covers | Time |
|---|---|---|---|
| 11 | [Memory, MMU and TLB](11-memory-mmu-tlb.md) | Direct memory, virtual memory, the Sv39 page-table walk, the software TLB and its 9-instruction fast path. | 1.5 h |
| 12 | [Exceptions, interrupts and precise faults](12-exceptions-and-faults.md) | Traps, the SIGSEGV trick, how a crash inside generated code becomes an exact guest exception. | 1 h |
| 13 | [Self-modifying code](13-self-modifying-code.md) | What happens when a program overwrites code that was already translated. | 45 min |
| 14 | [Floating point](14-floating-point.md) | Why FP is tricky (NaNs, rounding, flags), SoftFloat, and the fast SSE path. | 45 min |

### Part D: running real software

| # | File | What it covers | Time |
|---|---|---|---|
| 15 | [User mode: running Linux programs](15-user-mode-linux.md) | ELF loading, the initial stack, system calls, threads, signals, dynamic linking, the GDB stub. | 1 h |
| 16 | [System mode: booting Linux](16-system-mode-linux-boot.md) | Emulating a whole computer: devices, SBI firmware, the devicetree, the machine loop, SMP. | 1 h |

### Part E: proof and numbers

| # | File | What it covers | Time |
|---|---|---|---|
| 17 | [Testing and verification](17-testing-and-verification.md) | How we *know* it is correct: golden tests, riscv-tests, lockstep, fuzzers, QEMU comparison. | 45 min |
| 18 | [Performance and benchmarks](18-performance-and-benchmarks.md) | What was measured, how, the results, and why each technique made things faster. | 45 min |

### Part F: review and preparation

| # | File | What it covers | Time |
|---|---|---|---|
| 19 | [Algorithms cheat sheet](19-algorithms-cheat-sheet.md) | Every algorithm in the project, in short pseudocode, on one page. Good for revision. | 1 h |
| 20 | [Interview and internship prep](20-interview-and-internship-prep.md) | How to explain the project in 30 seconds, 2 minutes and 10 minutes, likely questions with answers, resume bullets. | 1 h |
| 21 | [Glossary](21-glossary.md) | Every term, A to Z, in one or two plain sentences. | reference |
| 22 | [Run it yourself](22-run-it-yourself.md) | How to build and run Bridge-V on your Windows PC (through WSL2, or with no Linux at all through GitHub Actions), and small experiments to try. | 1 h |

### Part G: beyond the roadmap (what makes Bridge-V different)

| # | File | What it covers | Time |
|---|---|---|---|
| 23 | [Tiered translation](23-tiered-translation.md) | Phase 12: new code runs in the interpreter first and is translated only after 32 runs, because most boot code runs only a few times. The break-even math (the ski-rental problem), the code, the tests and the measured result: Linux boots 18% faster, 1.33× QEMU. | 1 h |

### Part H: your brag doc

| # | File | What it covers | Time |
|---|---|---|---|
| 24 | [Brag doc](24-brag-doc.md) | Everything Bridge-V achieved, each item with its proof: the scoreboard, what each technique bought, how correctness was proven, what the repository shows you did, ready-made resume and interview lines, and how to say honestly who wrote the code. | 30 min |

Total: about 21 hours of reading. Take it slowly; one or two files a day works well.

---

## How to study with these files

1. **Read a file once, quickly.** Don't stop at every hard word. Get the shape of the idea.
2. **Read it again, slowly.** Open the source files it links to and find the functions it names.
3. **Answer the "Check yourself" questions out loud**, as if an interviewer asked you. If you get stuck, reread that section.
4. **Draw the diagrams yourself on paper.** Interviewers love whiteboard explanations, and drawing is the best test of understanding.
5. **Run things** (file 22). Seeing real output makes the ideas stick.

## Other documents in the repository

These existed before this study guide. They are more technical, so read them after the matching study file.

| Document | What it is |
|---|---|
| [`CLAUDE.md`](../../CLAUDE.md) | The full engineering specification and the decision log (D1–D63). Very dense. |
| [`docs/PROJECT_EXPLAINED.md`](../PROJECT_EXPLAINED.md) | A good general explanation of the project. |
| [`docs/WHITEBOARD.md`](../WHITEBOARD.md) | Real translated blocks, byte by byte. Read after files 07–11. |
| [`docs/BENCHMARKS.md`](../BENCHMARKS.md) | Every measured number, with its method. |
| [`docs/ROADMAP.md`](../ROADMAP.md) | The phase-by-phase build plan. |
| [`docs/phase-reports/`](../phase-reports/) | A detailed report for each of the 13 phases (0–12): what was built, how it was tested, what went wrong. |

A note on numbers: every performance number in these files comes from `docs/BENCHMARKS.md` or a phase report. Phases 0–11 were measured on a shared cloud machine (Intel Xeon @ 2.10 GHz). Phase 12 was measured on a GitHub Actions runner (AMD EPYC 7763), so compare its numbers only with each other. Your own machine will give different numbers again.
