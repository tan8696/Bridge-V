# Bridge-V explained: what it is, what it does, and what it is used for

This document explains Bridge-V from the ground up, for readers ranging from "never heard of binary translation" to "interviewer who has written a JIT".
- The exact engineering specification is [`CLAUDE.md`](../CLAUDE.md).
- The build plan is [`ROADMAP.md`](ROADMAP.md).

> **Status (2026-09-26):** Phase 1 complete. The **reference interpreter** runs real RISC-V Linux programs: all 110 official riscv-tests of the RV64GC user suites pass, 30 test programs match QEMU byte for byte, and CoreMark validates, at 127–176 MIPS interpreted. The JIT (the translator this document mostly describes) starts in Phase 2. Numbers under "Performance" are still targets until the JIT phases measure them (see `docs/phase-reports/`).

---

## 1. The one-paragraph answer

**Bridge-V is a program that lets software compiled for a RISC-V processor run on an ordinary Intel/AMD (x86-64) computer, fast.** It reads the RISC-V machine code, a few instructions at a time, and translates each chunk into equivalent x86-64 machine code *while the program is running*. It stores the translation, and then lets the real x86 CPU execute it directly. Translated chunks are linked together so that, once warmed up, the program runs almost entirely as native x86 code. Only occasionally does it return to Bridge-V for help, for example to translate new code or perform a system call. This technique is called **dynamic binary translation (DBT)**. It is the same core idea behind Apple's Rosetta 2, the QEMU emulator, and many video-game console emulators.

---

## 2. The problem: CPUs speak different languages

Every processor family understands its own **instruction set architecture (ISA)**: the binary "language" of machine instructions.
- An ARM chip in a phone speaks ARM64.
- A typical laptop or cloud server speaks **x86-64**.
- A growing family of open-standard chips speaks **RISC-V**.

When you compile a C program, the compiler turns it into machine code for one specific ISA. A RISC-V executable is just bytes like `93 05 15 00`, and to an x86 CPU those bytes are gibberish. It cannot run them.

**Analogy.** A compiled program is like a book written in Japanese, and your CPU only reads English. You have three options:
1. **An interpreter.** A human reads one Japanese sentence, says it in English, then moves to the next sentence, every time you read the book. It is correct but slow, and it repeats work every time a passage is re-read (every loop iteration).
2. **A static (ahead-of-time) translator.** Translate the whole book before reading. That sounds ideal, but for machine code it is extremely hard to do fully:
   - You can't always tell which bytes are code and which are data.
   - Jumps can go to computed addresses.
   - Programs can generate new code while running.
3. **A dynamic translator (Bridge-V's approach).** Translate each passage *the first time you reach it*, and keep the translation. The next time that passage comes up (the next loop iteration, the next call to that function), read the English version straight away. Hot code gets translated once and then runs at near-native speed. Code that's never reached never costs anything.

---

## 3. The two architectures involved

| | **RISC-V (guest)** | **x86-64 (host)** |
|---|---|---|
| What it is | An open, royalty-free ISA from UC Berkeley (2010). Used in microcontrollers, SSDs, AI accelerators, and increasingly in Linux-capable boards and servers. | The ISA of Intel and AMD PCs and most cloud servers. |
| Design style | RISC: fixed 32-bit instructions (plus 16-bit "compressed" ones), simple and regular | CISC: variable length, 1 to 15 bytes, many historical encodings |
| General-purpose registers | 32 (`x0`–`x31`; `x0` is always zero) | 16 (`RAX`…`R15`), some with special duties |
| Condition flags | None: compare-and-branch in one instruction | EFLAGS register set by most arithmetic |
| Memory ordering | RVWMO (weak) | TSO (strong) |
| In Bridge-V | **RV64GC**: 64-bit, with integer, multiply/divide, atomics, single- and double-precision floating point, CSRs, fence.i, and compressed instructions | 64-bit x86 on Linux, System V calling convention |

"**Guest**" is the architecture being emulated (RISC-V), and "**host**" is the machine actually running (x86-64).

---

## 4. What Bridge-V does

Bridge-V is a command-line program, `bridgev`, with two modes.

### 4.1 User mode: run a RISC-V Linux program
```
bridgev run ./hello-riscv64          # a RISC-V ELF executable
Hello, world!
```
- It loads a RISC-V Linux executable (an ELF file) into memory and sets up its stack, arguments and environment the way Linux would.
- It translates and runs the code.
- When the program asks the operating system for something (a **system call**: write to the screen, open a file, allocate memory), Bridge-V catches the request, converts it to the equivalent x86-64 Linux system call, and hands back the result.

The RISC-V program never knows it's not on real RISC-V hardware. This is the same job `qemu-riscv64` does.

### 4.2 System mode: emulate an entire RISC-V computer (stretch goal)
```
bridgev boot --kernel Image          # a RISC-V Linux kernel
[    0.000000] Linux version 6.6.x ... riscv64 ...
...
/ #                                  # an interactive BusyBox shell
```
Here Bridge-V emulates a whole machine:
- **The CPU's privilege levels**, so the kernel and user programs are isolated.
- **Virtual memory** (SV39 page tables).
- **A timer, an interrupt controller and a serial port** (the console you type into).
- **The firmware interface (SBI)** that the kernel talks to.

You boot a real, unmodified Linux kernel and get a shell prompt. This is what `qemu-system-riscv64` does.

---

## 5. What dynamic binary translators are used for in the real world

| Use | Real examples | What the DBT does there |
|---|---|---|
| **Moving a platform to a new CPU** | Apple **Rosetta** (PowerPC → x86, 2006) and **Rosetta 2** (x86-64 → Apple Silicon ARM64, 2020); Microsoft **Prism** (x64 apps on Windows on ARM) | Runs existing apps on new hardware so users don't lose their software during the transition. Rosetta 2 translates mostly ahead of time and falls back to a JIT for code generated at runtime. |
| **Running software for another architecture** | **QEMU** (TCG), **box64**, **FEX-Emu** | Run ARM/RISC-V/x86 Linux programs or whole operating systems on a different host. Used for development, testing and compatibility. |
| **Developing for chips that don't exist yet, or are scarce** | QEMU for RISC-V, used by the Linux kernel, Debian/Fedora and toolchain projects | Build and test RISC-V software on fast x86 servers and CI machines before affordable hardware exists. |
| **Game-console emulation** | **Dolphin** (GameCube/Wii PowerPC), plus many others | JIT-compile a console's CPU code to run games on PCs at full speed. |
| **Program analysis and instrumentation** | **Valgrind**, **DynamoRIO**, **Intel Pin** | Translate a program while inserting extra code: memory-error detection, profiling, taint tracking. |
| **Security research and fuzzing** | **AFL++ QEMU mode**, Unicorn engine | Run binary-only programs with coverage tracking to find bugs, and analyse malware for other architectures. |
| **Hardware design itself** | Transmeta Crusoe "Code Morphing" | Translated x86 into the native instructions of a VLIW processor, in firmware. |

**What Bridge-V specifically is for:**
1. **A deep systems-engineering portfolio project.** It demonstrates, end to end, the techniques used in the products above. It is scoped so that each hard subsystem is implemented from scratch and measured (see §9).
2. **A practical RISC-V execution tool.** It runs RISC-V Linux binaries and benchmarks on an x86 machine.
3. **A learning vehicle.** CPU architecture, instruction encoding, compilers (IR, register allocation), operating-system memory management, and Linux internals, all in one codebase.

---

## 6. How Bridge-V works, step by step

### 6.1 The big picture

```
 RISC-V program (ELF)
        │  load into memory
        ▼
 ┌─────────────┐   cache miss   ┌──────────┐   ┌─────┐   ┌──────────────┐   ┌──────────────┐
 │  Dispatcher │ ─────────────► │ Decoder  │─► │ IR  │─► │ Register     │─► │ x86 Emitter  │
 │  (Rust)     │                │          │   │opt. │   │ allocator    │   │ (raw bytes)  │
 └─────────────┘                └──────────┘   └─────┘   └──────────────┘   └──────┬───────┘
        ▲  exit code                                                               │ store
        │                                                                          ▼
        │                 ┌──────────────────────────────────────────────────────────────┐
        └──────────────── │ Translation cache: native x86 blocks, linked to each other   │
          (only when      │ by patched jumps ("block chaining"), executed by the CPU     │
           needed)        └──────────────────────────────────────────────────────────────┘
```

### 6.2 The unit of translation: the basic block
A **basic block** is a straight run of instructions with no jumps into the middle and only one way out at the end (a branch, jump or call). Bridge-V translates one basic block at a time into a **translation block (TB)** of x86 code.

### 6.3 A worked example
Take this C function:
```c
long sum(long n) { long s = 0; for (long i = 0; i < n; i++) s += i; return s; }
```
A RISC-V compiler produces something like this (illustrative):
```
sum:    blez  a0, .Lzero        # if n <= 0, return 0
        li    a5, 0             # s = 0
        li    a4, 0             # i = 0
.Lloop: add   a5, a5, a4        # s += i
        addi  a4, a4, 1         # i++
        bne   a0, a4, .Lloop    # repeat while i != n
        mv    a0, a5            # return s
        ret
.Lzero: li    a0, 0
        ret
```
Here is what happens the first time `.Lloop` runs:

1. **Decode.** Bridge-V reads the bytes at `.Lloop` and decodes three instructions: `add`, `addi`, `bne`. It stops at `bne` because a branch ends the block.
2. **Lift to IR.** The instructions become a simple internal form: "read a5, read a4, add, write a5; …". Bridge-V optimizes this form. For example, it folds constants and removes stores that are immediately overwritten.
3. **Allocate registers.** RISC-V has 32 registers and x86 only 16, so they can't all live in x86 registers.
   - The busiest guest registers are **pinned** permanently. In the default configuration a0 lives in `R14` and a5 in `R15`.
   - Others, like a4, are loaded into a free x86 register for the duration of the block and written back to memory at the end.
4. **Emit x86 machine code.** Bridge-V writes raw bytes, with no assembler involved. They correspond to:
   ```
   sub  qword [rbp+BUDGET], 3   ; charge 3 guest instructions; exit if time slice used up
   jl   budget_exit
   mov  rsi, [rbp+A4]           ; load a4 from the saved-register area
   add  r15, rsi                ; add  a5, a5, a4      (a5 lives in R15)
   lea  rsi, [rsi+1]            ; addi a4, a4, 1
   mov  [rbp+A4], rsi           ; write a4 back before leaving the block
   cmp  r14, rsi                ; bne  a0, a4          (a0 lives in R14)
   jne  <start of this block>   ; loop again: patched to jump straight back here
   jmp  <block after the loop>  ; fall through: patched once that block exists
   ```
5. **Store and run.**
   - The bytes go into a special memory region that can be made executable.
   - Bridge-V casts that address to a function pointer and calls it. The x86 CPU now runs the translated loop *natively*.
6. **Chain.**
   - The first time the `jne` is taken, it goes back to Bridge-V. Bridge-V sees the target block is already translated and **rewrites the jump's 4-byte offset** so it points straight at that block.
   - From then on, the loop runs entirely in native code, about 8 x86 instructions per iteration, never touching Bridge-V.
   - An interpreter would need dozens of host instructions *per guest instruction* to fetch, decode and dispatch.

Notice that `a4` is loaded and stored on every iteration, because it isn't pinned. Measuring which registers are hottest, and pinning those, is exactly the kind of tuning Phase 4 does.

### 6.4 When the translated code needs help
Native code returns to the Rust dispatcher only when:
- it reaches code that hasn't been translated yet
- the program makes a system call
- an exception happens (a bad memory access, an illegal instruction)
- the time slice runs out, so interrupts and timers can be serviced

Everything else stays on the fast path.

---

## 7. The four hard subsystems, explained

### 7.1 Runtime machine-code generation (the JIT pipeline)
- **What:** Bridge-V produces x86 instructions as bytes, because the x86 encoding is intricate:
  - prefixes
  - ModRM/SIB addressing bytes
  - special cases for particular registers
  - sign-extended immediates

  It writes them into memory obtained from the OS with `mmap`, makes that memory executable, and jumps into it.
- **Security (W^X):** memory is never writable and executable at the same time. Bridge-V maps the same physical memory twice: once writable (for emitting code) and once executable (for running it).
- **Why it's hard:** one wrong bit crashes the whole process, with no error message. That is why every encoder function is tested against an independent disassembler.

### 7.2 Direct block chaining
- **What:** each translated block ends with jumps whose target offsets are **hot-patched** once the destination block exists. After that, blocks jump straight to each other.
- **Why it matters:** without chaining, every block returns to a central dispatcher loop. The CPU's branch predictor keeps guessing wrong on that one heavily shared jump, which costs roughly 15–20 cycles each time. With chaining, execution flows block to block like normal compiled code.
- **Subtleties:**
  - Keeping the patch atomic (4-byte-aligned offsets).
  - Undoing links when code is invalidated.
  - Keeping infinite loops interruptible (the "budget" counter at each block's start).
  - Handling indirect jumps such as function returns. These can't be patched, so they use a small in-memory lookup table, the "jump cache".

### 7.3 Register remapping and spill allocation
- **What:** 32 guest registers must be mapped onto roughly 11 usable x86 registers.
  - Four of the hottest guest registers are **pinned** to `R12`–`R15`. These x86 registers survive function calls under the host ABI, so they cost nothing to keep live across blocks.
  - The rest live in a memory structure (`CpuState`, addressed through `RBP`). A **linear-scan register allocator** assigns them to free x86 registers within each block.
- **"Zero-cost spill resolution":** the rare slow paths (a memory-translation miss, a helper call) save and restore registers in out-of-line "cold stubs". The common path contains no save/restore code at all.

### 7.4 Software MMU and two-level TLB emulation
- **What:** in system mode, the guest OS uses **virtual memory**. Every address the program uses must be translated through RISC-V SV39 page tables (a 3-level tree in guest memory) into a physical address.
- **Problem:** walking three levels of page tables on every load and store would be devastatingly slow.
- **Solution:** a **software TLB** (translation lookaside buffer), a small cache of recent translations.
  - The JIT emits the TLB check inline, right in the translated code: about 5 x86 instructions to compute the slot, compare the tag and branch.
  - On a hit, the access proceeds immediately. On a miss, a cold stub calls the Rust page walker, which fills the TLB.
- **Why it's hard:**
  - Permissions (user vs supervisor, read/write/execute) and the accessed/dirty bits must all be exactly right.
  - Exceptions must be *precise*: the guest must see exactly which instruction faulted, with all registers correct.

---

## 8. Other important parts

| Part | What it does |
|---|---|
| **Reference interpreter** | A straightforward (pre-decoded) interpreter. It is the *golden model*: the JIT is checked against it instruction by instruction ("lockstep" mode). It is also the baseline for speedup numbers. |
| **Decoder** | Turns raw bytes into structured instructions, including the 16-bit compressed forms, and rejects illegal encodings. |
| **ELF loader and Linux syscall layer** | Loads executables and emulates about 40 Linux system calls. It translates data structures whose layout differs between RISC-V and x86 (e.g. `struct stat`). |
| **Self-modifying code handling** | Programs such as JIT compilers and OS loaders sometimes write new code into memory. Bridge-V write-protects memory pages it has translated. On a write, it discards the affected translations and unlinks any chained jumps into them, so stale code never runs. |
| **Floating point** | IEEE-754 details differ between RISC-V and x86: NaN encoding, rounding modes, exception flags, conversion saturation. Bridge-V starts with the bit-exact Berkeley SoftFloat library, then adds inline SSE fast paths only where it can prove identical results. |
| **Atomics and memory ordering** | RISC-V atomic instructions map to x86 `lock`-prefixed instructions. Because x86 has *stronger* ordering than RISC-V, most RISC-V fences cost nothing on x86. |
| **Devices (system mode)** | Timer (CLINT), interrupt controller (PLIC), serial port (16550 UART), power-off device, and an SBI firmware interface. Together these form a machine compatible with QEMU's `virt` board, so a stock Linux kernel boots. |
| **Verification tooling** | The official RISC-V `riscv-tests` suite, lockstep differential testing, random instruction fuzzing, and comparison against QEMU's output. |

---

## 9. What makes this project hard (and impressive)

- **Correctness is unforgiving.** One wrong sign-extension or flag breaks programs in ways that surface millions of instructions later. That is why the design leans heavily on differential testing.
- **Two ISAs in depth.** You must know RISC-V semantics exactly (e.g. division by zero returns −1 instead of trapping), and know the x86 encoding at the bit level.
- **Compiler techniques at runtime.** IR design, optimization, liveness and register allocation, all under a tight time budget, because translation time is paid while the program runs.
- **Operating-system internals.** Virtual memory, page-table walks, TLBs, privilege modes, traps and interrupts, system calls, signals, `mmap`/`mprotect`.
- **Performance engineering.** Measuring precisely, knowing where cycles go (branch prediction, memory round-trips), and proving each optimization with numbers.

**Skills demonstrated:** systems programming in Rust (including disciplined `unsafe`), computer architecture, instruction encoding, JIT compilation, register allocation, virtual memory, OS/ABI contracts, testing methodology, and performance analysis.

---

## 10. Performance: what the numbers mean

- **MIPS / "instructions per second"** means how many *guest* RISC-V instructions Bridge-V completes per second of wall-clock time.
- **Speedup** means JIT time compared with the interpreter on the same workload (CoreMark, Dhrystone), same machine, same inputs.
- **Targets, not results** (CLAUDE.md §22):

  | Configuration | Target |
  |---|---|
  | JIT in user mode | ≥ 5–10× faster than the interpreter; on the order of hundreds of MIPS |
  | JIT in system mode (with SoftMMU) | ≥ 100–120 MIPS |
  | TLB | ~4–5 cycles on a hit vs tens of cycles for a page walk |

- Every real number is recorded in `docs/BENCHMARKS.md` and the phase reports, with the exact command and machine used. **No unmeasured number is ever reported as a result.**

---

## 11. What Bridge-V is *not*

- It's not a cycle-accurate simulator. It doesn't model pipeline timing; it only produces correct results fast.
- It's not a hardware design tool like Verilog or a RISC-V CPU core.
- It's not (initially) a general "any ISA" emulator. It does exactly one guest (RV64GC) and one host (x86-64 Linux).
- It's not a replacement for QEMU. It's a focused, from-scratch implementation of the core techniques, built to be understood, measured and explained.

---

## 12. Project status and how it will be used

- **Now:** Phases 0–1 are complete: the toolchain, CI, the decoder and disassembler, and the reference interpreter with Linux user-mode emulation (`bridgev run program.elf` works). The next phase is the naive JIT.
- **Milestone A (required):** CoreMark and Dhrystone run under both the interpreter and the JIT, with a printed speedup table.
- **Milestone B (stretch):** Linux 6.6 boots to a BusyBox shell.
- **Planned usage:**
  ```
  bridgev run   [--engine=interp|jit|lockstep] [--stats] program.elf [args]
  bridgev boot  --kernel Image [--initrd rootfs.cpio] [--ram 512M]
  bridgev disasm program.elf
  bridgev bench coremark
  ```
- **Progress reports:** one detailed report per completed phase in [`docs/phase-reports/`](phase-reports/).

---

## 13. Glossary

| Term | Meaning |
|---|---|
| **ISA** | Instruction Set Architecture: the machine-language contract of a CPU family. |
| **Guest / host** | The emulated architecture (RISC-V) / the real machine (x86-64). |
| **DBT** | Dynamic Binary Translator: translates machine code while the program runs. |
| **JIT** | Just-In-Time compilation: generating machine code at runtime. |
| **Basic block / TB** | A straight-line run of instructions with one exit / its translated x86 version. |
| **Translation cache** | The executable memory region holding all translated blocks. |
| **Block chaining** | Patching a block's exit jump to point directly at the next translated block. |
| **Jump cache** | A small table mapping guest addresses to translated blocks, for indirect jumps. |
| **Dispatcher** | The Rust loop that finds or creates translations and handles exits. |
| **IR** | Intermediate Representation: a simplified internal instruction form used for optimization. |
| **Register allocation** | Deciding which values live in which host registers, and when to spill them to memory. |
| **Spill / fill** | Saving a register's value to memory / loading it back. |
| **Pinned register** | A guest register permanently kept in a specific host register. |
| **MMU / SoftMMU** | Memory Management Unit (translates virtual → physical addresses) / its software emulation. |
| **SV39** | RISC-V's 39-bit virtual-memory scheme with 3-level page tables. |
| **TLB** | Translation Lookaside Buffer: a cache of recent address translations. |
| **W^X** | "Write xor Execute": memory is never writable and executable at once. |
| **SMC** | Self-Modifying Code: a program writing instructions it later executes. |
| **ELF** | The executable file format used by Linux. |
| **Syscall** | A request from a program to the operating system kernel. |
| **SBI** | Supervisor Binary Interface: the firmware API a RISC-V kernel calls. |
| **CLINT / PLIC / UART** | RISC-V timer / interrupt controller / serial port devices. |
| **ABI** | Application Binary Interface: calling conventions and data layouts. |
| **MIPS** | Millions of (guest) Instructions Per Second. |
| **Lockstep testing** | Running the JIT and the interpreter side by side and comparing state after every block. |

---

## 14. FAQ

**Why RISC-V as the guest?** Its regular encoding and lack of condition flags make it the cleanest guest to translate well. It's also increasingly important in industry, which makes RISC-V tooling on x86 genuinely useful.

**Why x86-64 as the host?** It's the most common development and cloud machine, and its complex encoding makes the emitter a real demonstration of low-level skill.

**Why Rust?** Most of a DBT is ordinary logic (decoding, loading files, page walking, syscalls), where Rust's safety prevents whole classes of bugs. The genuinely unsafe parts (executable memory, jumping into generated code, signal handlers) are small and isolated.

**Why not use LLVM or an assembler library to generate code?** Writing the x86 encoder by hand is one of the core skills this project demonstrates. It also keeps translation extremely fast, because LLVM is too slow for per-block JIT use.

**How do you know the translation is correct?** In four ways:
- The official RISC-V test suites.
- A reference interpreter compared against the JIT after every block.
- Millions of randomly generated instruction sequences.
- Comparing program output against QEMU.

**How is it different from QEMU?** QEMU is a huge, general, multi-architecture system. Bridge-V is a focused, single-pair, from-scratch implementation of the same core ideas. It is designed to be small enough to understand fully and explain on a whiteboard, with every subsystem measured.
