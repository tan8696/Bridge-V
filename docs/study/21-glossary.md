# 21 · Glossary

Every term used in the study files, A to Z, in one or two plain sentences. The number in brackets is the study file that explains it best.

---

## A

- **A/D bits** — "Accessed" and "Dirty" flags in a page-table entry. Hardware (or Bridge-V's page walker) sets A when a page is used and D when it is written. [11]
- **ABI (application binary interface)** — the rules machine code follows so separately compiled pieces can work together: which registers hold arguments, which ones a function must preserve, how the stack is laid out. [03, 09]
- **Addend** — in a software-TLB entry, the number to add to a guest virtual address to get the host address of the same byte. [11]
- **AMO (atomic memory operation)** — a RISC-V instruction that reads, modifies and writes memory as one indivisible step, e.g. `amoadd`. [02]
- **Assembly language** — the human-readable text form of machine code, e.g. `add a0, a1, a2`. [00]
- **AUIPC** — "add upper immediate to pc": a RISC-V instruction that computes an address relative to the current instruction. Bridge-V turns it into a constant, because the pc is known at translation time. [07]
- **Auxiliary vector (auxv)** — a list of key–value pairs Linux puts on a new program's stack (page size, entry point, random bytes, …). Bridge-V builds it for guest programs. [15]

## B

- **Bare mode** — running a program with no operating system at all, as the riscv-tests do. Bridge-V's `--mode bare`. [17]
- **Basic block** — a run of instructions with one entry and no jumps in the middle; it ends at the first branch or jump. The unit Bridge-V translates. [01, 07]
- **Binary translation** — converting machine code for one CPU into machine code for another. *Static* translation does it before running; *dynamic* translation (Bridge-V) does it while the program runs. [01]
- **Bit / byte** — a bit is a 0 or 1; a byte is 8 bits. [00]
- **Block chaining** — patching the jump at the end of a translated block so it goes straight to the next translated block, without returning to the dispatcher. Bridge-V's biggest single speed-up (8.9× on CoreMark). [10]
- **Block-boundary ABI** — Bridge-V's rules for what must be true when one translated block hands over to another: where CpuState is, which guest registers are in R12–R15, and that nothing else is left in registers. [09]
- **Branch predictor** — the part of a CPU that guesses where a jump will go, so it can keep working before it knows. Wrong guesses cost 15–20 cycles. [10]
- **Break-even point** — the number of runs after which translating a block becomes cheaper than interpreting it: translation cost ÷ (interpretation cost − JIT cost per run), about 50–90 runs in Bridge-V. [23]
- **Budget** — the number of guest instructions translated code may run before it must return to the dispatcher (normally 100,000). It stops infinite loops from freezing the emulator. [05, 10]
- **BusyBox** — a single small program that provides many Unix commands (`sh`, `ls`, `cat`, …). The shell you reach when Bridge-V boots Linux. [16]

## C

- **Cache** — a small, fast store of recently used things. In Bridge-V it can mean the CPU's caches, the **translation cache** (translated blocks) or the **software TLB**. [01]
- **Callee-saved / caller-saved registers** — callee-saved registers (RBX, RBP, R12–R15 on x86-64 Linux) must be preserved by a called function; caller-saved ones may be overwritten by it. [03, 08]
- **Canonical NaN** — the one standard "not a number" bit pattern RISC-V always produces. x86 produces different NaNs, so Bridge-V corrects them. [14]
- **CLINT** — "core-local interruptor": the device that provides the timer and software interrupts in a RISC-V machine. [16]
- **Code buffer / code cache** — the memory where translated x86 code is placed. [09]
- **Code page** — a guest memory page that holds code Bridge-V has decoded or translated; writes to it are watched (self-modifying code). [13]
- **Cold block / hot block** — a cold block has not run often enough to be translated (it runs in the interpreter); a hot block has, and runs as x86 code. [23]
- **Cold stub / cold path** — rarely used code placed at the end of a translated block (TLB misses, exits), so the common path stays short. [07, 11]
- **Competitive ratio** — how much worse an algorithm that can't see the future is than the best choice in hindsight. The ski-rental rule is 2-competitive. [23]
- **Constant folding** — computing results at translation time when the inputs are known constants, e.g. `lui` + `addi` becomes one constant. [07]
- **Context switch** — the operating system switching the CPU from one process to another. [16]
- **CoreMark** — the industry-standard CPU benchmark (lists, matrices, state machine, CRC). [18]
- **CpuState** — the Rust struct holding all guest registers, pc, CSRs, the jump cache, spill slots and the TLB. Translated code finds it through RBP. [08, 09]
- **Cross-compiler** — a compiler that runs on one machine (x86) but produces programs for another (RISC-V), e.g. `riscv64-linux-gnu-gcc`. [00]
- **CSR (control and status register)** — special RISC-V registers for privileged state (e.g. `mstatus`, `satp`) and counters. [02]

## D

- **Dead code elimination (DCE)** — removing computations whose results are never used. [07]
- **Decode** — turning raw instruction bytes into a structured form (which instruction, which registers, which constant). [02, 06]
- **Devicetree (DTB)** — a data structure describing the machine's hardware (memory, devices, CPUs) that Linux reads at boot. Bridge-V generates one. [16]
- **Dhrystone / DMIPS** — a classic small integer benchmark; DMIPS = Dhrystones per second ÷ 1,757. [18]
- **Direct mode (memory)** — user-mode guest memory placed at a fixed offset in host memory, so a guest load is one x86 instruction. [11]
- **Dirty (register)** — a guest register whose newest value is only in a host register and has not been written back to CpuState yet. [08]
- **Dispatcher** — the Rust loop that picks the next block to run, translates it (or, while cold, interprets it), and handles exits. `Jit::run` and `select()`. [05]
- **DMA (direct memory access)** — a device writing guest memory by itself, e.g. the virtio disk. [16]
- **Doubleword / word / halfword** — in RISC-V, 8 / 4 / 2 bytes. [02]
- **Dual mapping** — mapping the same memory twice: once writable (to emit code) and once executable (to run it), so no page is ever both at once. [09]
- **Dynamic loader** — the program (`ld.so`) that loads a program's shared libraries when it starts. [15]

## E

- **ECALL** — the RISC-V instruction for a system call (or an SBI call from the kernel). [15, 16]
- **ELF** — the executable file format of Linux: headers, segments, entry point, symbols. [15]
- **Emitter** — Bridge-V's hand-written x86 encoder that writes machine-code bytes. [03, 07]
- **Emulator** — a program that imitates a different machine. [00]
- **Engine** — a way of running guest code: `interp`, `jit` or `lockstep`. [05]
- **Entry point** — the address where a program starts running. [15]
- **Exception / trap / interrupt** — an exception is caused by an instruction (bad memory access, illegal instruction); an interrupt comes from outside (timer, device); a trap is the jump into the handler for either. [12]
- **Exit slot / exit stub** — a patchable jump at the end of a translated block (slot), and the small code it points to until it is chained (stub), which returns to the dispatcher. [10]

## F

- **Fault site / state map** — a record, made at translation time for every load and store, of where each dirty guest register lives at that point. It lets a crash inside translated code be turned into an exact guest exception. [08, 12]
- **FENCE.I** — the RISC-V instruction that says "I changed some code; make it visible". [13]
- **Fetch–decode–execute cycle** — what every CPU and interpreter does: read an instruction, work out what it is, do it, repeat. [06]
- **fflags / frm** — the RISC-V floating-point exception flags and rounding mode. [14]
- **Flush** — throw away cached things: translations (code-cache flush) or TLB entries (TLB flush). [11, 13]
- **FMA (fused multiply-add)** — `a × b + c` computed with a single rounding. [14]
- **Fuzzer** — a test that generates random inputs (here random instruction blocks or FP values) and compares two implementations. [17]

## G

- **GDB stub** — a small server that lets the `gdb` debugger control a guest program. [15]
- **GIL (global interpreter lock)** — one lock that lets only one guest thread run at a time in Bridge-V's user mode. [15]
- **GitHub Actions / workflow / runner** — GitHub's service that runs commands on its machines (runners) whenever you push; a workflow is the list of commands. Phase 12 was built and measured this way. [22]
- **Golden model / golden test / oracle** — a trusted reference to compare against: the interpreter for the JIT, LLVM for the decoder, iced-x86 for the emitter. [17]
- **Guest / host** — the guest is the emulated machine and its code (RISC-V); the host is the real machine running Bridge-V (x86-64). [01]

## H

- **Hart** — RISC-V's word for a hardware thread (a CPU core). [16]
- **Helper (helper call)** — a Rust function that translated code calls for rare or complex instructions (`helper_interp_one`) or TLB misses (`helper_mmu_access`). [07]
- **Hexadecimal (hex)** — base 16, written with 0–9 and a–f; one hex digit is exactly 4 bits. [00]
- **HTIF / tohost** — a simple way for bare-metal test programs to report results: they write to a memory word called `tohost`. [17]

## I

- **icount** — the number of guest instructions retired so far. [05]
- **IEEE 754** — the standard for floating-point numbers. [14]
- **Immediate** — a constant stored inside an instruction, e.g. the 5 in `addi a0, a0, 5`. [02]
- **Indirect jump** — a jump whose target is in a register (RISC-V `jalr`), so it can't be known at translation time. [10]
- **initramfs / initrd** — a small file system loaded into memory at boot, holding BusyBox and `/init`. [16]
- **Instruction set architecture (ISA)** — the "language" a CPU understands: its instructions, registers and rules. RISC-V and x86-64 are ISAs. [00]
- **Interpreter** — runs a program by reading and carrying out one instruction at a time in software. Bridge-V's is the golden model. [06]
- **Interpreter tier** — Bridge-V's use of the interpreter for blocks that have run fewer than `--tier` times (default 32), so code that barely runs is never translated. [23]
- **IR (intermediate representation)** — a simple in-between form of code that is easy to optimize, used between decoding and x86 emission. Bridge-V's IR is SSA. [07]

## J

- **JAL / JALR** — RISC-V jump-and-link instructions: JAL to a fixed offset, JALR to an address in a register (used for returns). [02]
- **JIT (just-in-time compiler)** — a compiler that translates code while the program runs, right before it is needed. [01]
- **Jump cache** — a 4,096-entry table in CpuState from guest pc to host code address, looked up inline by translated code for indirect jumps. [10]

## K

- **Kernel** — the core of an operating system (here Linux), running in the privileged mode. [16]

## L

- **Lazy write-back** — keeping a changed guest register in a host register and writing it to CpuState only when needed (at an exit), not after every change. [08]
- **Level-triggered interrupt** — an interrupt that stays active as long as the device keeps its line raised. [16]
- **Lift** — translate decoded instructions into IR. [07]
- **Linear scan** — a fast register-allocation algorithm that walks the code once and gives registers to values in order. [08]
- **Link / unlink** — chaining an exit to a block (link), or undoing it when the block is invalidated (unlink). [10]
- **Linker** — the tool that combines compiled pieces into one executable. [00]
- **Live interval** — the stretch of code between a value's creation and its last use. [08]
- **Lockstep** — running the JIT and the interpreter on the same block from the same state and comparing the full results, block by block. [17]
- **Long tail (power law)** — a distribution where a few items are used enormously and most items barely at all, like the run counts of boot code. [23]

## M

- **Machine code** — the bytes a CPU actually executes. [00]
- **Machine loop** — the system-mode outer loop that runs harts slice by slice and updates devices and timers in between. [16]
- **memfd_create** — a Linux call that creates an anonymous in-memory file, used for dual mapping. [09]
- **MIPS (metric)** — millions of guest instructions completed per second. [18]
- **MMIO (memory-mapped I/O)** — talking to a device by reading and writing special addresses. [16]
- **MMU (memory management unit)** — the hardware that translates virtual addresses to physical ones and checks permissions; Bridge-V emulates RISC-V's in software (softmmu). [11]
- **mmap / mprotect** — Linux calls to map memory and to change its permissions. [09, 15]
- **M/S/U modes** — RISC-V privilege levels: Machine (firmware), Supervisor (kernel), User (programs). [02, 16]
- **MXCSR** — the x86 register holding SSE rounding mode and floating-point flags. [14]

## N

- **NaN / NaN-boxing** — NaN is "not a number". NaN-boxing is how RISC-V stores a 32-bit float in a 64-bit register: the upper half is all ones. [14]
- **Native** — code compiled directly for the host CPU; the "100%" speed reference. [18]

## O

- **Opcode** — the part of an instruction that says which operation it is. [02, 03]
- **OpenSBI** — the standard RISC-V firmware that provides SBI services in M-mode. [16]
- **Operating system (OS)** — the software that manages the machine and runs programs (here Linux). [00]

## P

- **Page / page table / page fault** — memory is divided into 4 KiB pages; the page table maps virtual pages to physical ones; a page fault is the exception when a mapping or permission is missing. [11]
- **pc (program counter)** — the register holding the address of the current instruction. [00]
- **pcmap** — per translated block, a map from host code offsets back to guest instructions (for faults and profiling). [12]
- **Physical / virtual address** — a physical address names real memory; a virtual address is what a program uses, translated by the MMU. [11]
- **Pinned register** — a guest register that always lives in the same host register (x2, x1, x10, x15 in R12–R15). [08]
- **PLIC** — "platform-level interrupt controller": routes device interrupts to harts. [16]
- **Precise exception** — the guest sees the exact state from just before the faulting instruction, as real hardware would give. [12]
- **Privilege level** — how much a piece of code is allowed to do (M/S/U). [02]
- **Promotion / tier-up** — the moment a cold block has run `--tier` times and gets translated. [23]

## Q

- **QEMU / TCG / TCI** — QEMU is the best-known open-source emulator; TCG is its translator, which translates every block on first use; TCI is an optional interpreter build of TCG, not a tier. [01, 23]

## R

- **R12–R15, RBP, RBX, RSP** — x86-64 registers. In Bridge-V, R12–R15 hold pinned guest registers, RBP points into CpuState, RBX holds the guest memory base in direct mode, and RSP is the host stack. [03, 08]
- **rel32 / rel8** — a jump's 4-byte or 1-byte signed distance, counted from the end of the jump instruction. [03, 10]
- **Reservation (LR/SC)** — the memory address remembered by `lr` (load-reserved) that a later `sc` (store-conditional) checks. [02]
- **REX / ModRM / SIB** — the prefix and bytes of an x86 instruction that select 64-bit size, registers and memory addressing. [03]
- **RISC-V / RV64GC / RVC** — an open ISA; RV64GC is its 64-bit general-purpose profile; RVC is its 16-bit compressed instruction set. [02]
- **riscv-tests** — the official RISC-V instruction test suite (244 programs in Bridge-V's setup). [17]
- **Rosetta 2** — Apple's x86-to-ARM translator; it translates ahead of time where it can. [01]
- **Rounding mode** — how a result that doesn't fit exactly is rounded (to nearest-even, toward zero, …). [14]
- **rustfmt / clippy** — Rust's official code formatter and linter; CI refuses code they complain about. [22]

## S

- **satp** — the RISC-V CSR that selects the page-table mode (Bare/Sv39/Sv48) and the root page table. [11]
- **Saturating conversion** — converting an out-of-range value to the nearest representable one instead of wrapping around. [14]
- **SBI (supervisor binary interface)** — the calls a RISC-V kernel makes to its firmware (timer, console, IPIs). Bridge-V has a built-in one. [16]
- **SFENCE.VMA** — the RISC-V instruction that flushes TLB entries after page tables change. [11]
- **Sign-extend / zero-extend** — widening a number by copying its sign bit (sign-extend) or adding zeros (zero-extend). [00, 02]
- **SIGSEGV / signal** — a signal is an asynchronous notification to a process; SIGSEGV means "bad memory access". Bridge-V catches it to handle guest faults. [12]
- **Ski-rental problem** — deciding between renting (small repeated cost) and buying (one big cost) without knowing the future; "rent until you've paid the price of buying, then buy" is never worse than 2× the best. Bridge-V's `--tier` is this rule. [23]
- **Slice** — a fixed number of instructions (100,000 by default) after which the engine returns so devices and timers can be updated. [05, 16]
- **SMC (self-modifying code)** — a program writing new code into memory that may already have been translated. [13]
- **SMP** — several harts (CPUs) in one machine. [16]
- **softmmu** — Bridge-V's software MMU: every guest memory access goes through a software TLB. [11]
- **SoftFloat** — a C library that computes floating point exactly in software; Bridge-V's FP reference. [14]
- **Spill / spill slot** — moving a value out of a host register into memory when registers run out; the memory place is a spill slot. [08]
- **SSA (static single assignment)** — an IR form where every value is assigned exactly once, which makes optimization simpler. [07]
- **SSE** — the x86 instructions and XMM registers used for floating point. [14]
- **Superpage** — a large page (2 MiB or 1 GiB) mapped by a single page-table entry. [11]
- **Sv39 / Sv48** — RISC-V virtual-memory modes with 39- and 48-bit virtual addresses (three and four page-table levels). [11]
- **Syscall (system call)** — a program's request to the operating system (read a file, write to the screen, …). [15]
- **sysroot** — the folder holding a target system's libraries and headers, used to find `ld.so` and libc for dynamic programs. [15]
- **System mode / user mode** — emulating a whole computer (boots a kernel) versus running a single Linux program. [01]

## T

- **TB (translation block)** — one translated guest basic block: its x86 code plus metadata (exits, pcmap, fault sites). [07]
- **TbKey** — what a TB is looked up by: guest pc, FP variant and, in system mode, MMU flags and physical page. Cold blocks use the same key. [07, 23]
- **Tier (`--tier N`)** — how many times a block runs in the interpreter before it is translated; 0 translates on first run, the default is 32. [23]
- **TLB (translation lookaside buffer)** — a cache of virtual-to-physical translations. Bridge-V's is a 256-entry software table per MMU index (U, S, S+SUM, M), checked inline by translated code. [11]
- **Trampoline** — a tiny piece of generated code that enters (`enter_jit`) or leaves (`exit_jit`) translated code, saving and restoring registers. [09]
- **Two's complement** — the standard way to store negative integers: flip all bits and add one. [00]

## U

- **UART (16550)** — the serial-port device the guest's console talks to. [16]
- **`unsafe` (Rust)** — a Rust block where the compiler can't check memory safety, needed for executable memory and signal handlers; Bridge-V limits it to a few files. [00, 09]

## V

- **virtio-blk** — a standard virtual disk device; Bridge-V's `--disk`. [16]

## W

- **W^X ("write xor execute")** — the rule that no memory is writable and executable at the same time. [09]
- **WFI (wait for interrupt)** — a RISC-V instruction that idles the hart until an interrupt arrives; Bridge-V then sleeps the host. [16]
- **WSL2** — "Windows Subsystem for Linux 2": a real Linux kernel inside Windows, where Bridge-V can be built and run. [22]

## X

- **x0** — RISC-V register 0, which always reads as zero. [02]
- **x86-64** — the 64-bit instruction set of Intel and AMD PCs; Bridge-V's host. [03]
