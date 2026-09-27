# 00 · Beginner foundations: everything you need before Bridge-V

## What you will learn

This file assumes you know nothing about how computers work at the low level. By the end you will understand:

- how numbers are stored (bits, bytes, binary, hexadecimal, negative numbers)
- what a CPU does, and what registers, memory and instructions are
- what "machine code" and "assembly" mean
- how a C program becomes something a CPU can run
- what an operating system does (processes, system calls, virtual memory, permissions)
- what emulators, interpreters and JIT compilers are
- the small amount of Rust you need to read Bridge-V's source code

Take your time with this file. Everything else is built on it.

---

## 1. Bits, bytes and numbers

### 1.1 Bits and bytes

A computer stores everything as **bits**. A bit is a single 0 or 1 (think of a light switch: off or on).

- 8 bits make a **byte**. One byte can hold 2⁸ = 256 different values (0 to 255).
- Bigger groups have names that depend on context. In this project:
  - 16 bits = 2 bytes = a **halfword**
  - 32 bits = 4 bytes = a **word**
  - 64 bits = 8 bytes = a **doubleword**

The CPUs in this project are **64-bit**: their normal numbers are 64 bits wide (8 bytes), which can hold values up to about 18 billion billion.

### 1.2 Binary

We normally count in base 10 (decimal). Computers count in base 2 (**binary**): each position is worth twice the one to its right.

```
binary   1  0  1  1
worth    8  4  2  1      →  8 + 0 + 2 + 1 = 11 (decimal)
```

### 1.3 Hexadecimal ("hex")

Binary is long and hard to read, so programmers use **hexadecimal** (base 16). Its digits are `0–9` then `A–F` (A = 10, B = 11, …, F = 15). Hex numbers are written with a `0x` prefix.

The trick that makes hex useful: **one hex digit is exactly four bits.**

```
hex digit:  0    1    2    3    4    5    6    7    8    9    A    B    C    D    E    F
binary:   0000 0001 0010 0011 0100 0101 0110 0111 1000 1001 1010 1011 1100 1101 1110 1111
```

So `0x2F` = `0010 1111` in binary = 47 in decimal. A byte is always exactly two hex digits (`0x00` to `0xFF`). You will see hex everywhere in this project: addresses like `0x10990`, instruction bytes like `48 8b 45 f0`.

**Practice:** `0x13` = `0001 0011` = 16 + 3 = 19. `0xFF` = 255. `0x100` = 256.

### 1.4 Negative numbers: two's complement

How do you store −1 in bits? Computers use **two's complement**. For an 8-bit number:

- 0 to 127 are stored normally (`0x00` to `0x7F`).
- −1 is stored as `0xFF` (all ones), −2 as `0xFE`, and so on down to −128 as `0x80`.

The rule: to negate a number, flip every bit and add 1. The top bit (the **sign bit**) tells you if the number is negative.

The nice thing about two's complement is that addition works the same for signed and unsigned numbers. The CPU doesn't need to know which one you mean. But some operations *do* care, like comparison ("is −1 less than 5?") and division. That is why RISC-V has both `blt` (branch if less than, **signed**) and `bltu` (branch if less than, **unsigned**). As unsigned numbers, `0xFF…FF` is the biggest possible value. As signed numbers, it is −1.

### 1.5 Sign extension and zero extension

Often you need to widen a number, for example turn a 32-bit value into a 64-bit value. There are two ways:

- **Zero extension:** fill the new top bits with 0. `0xFFFFFFFF` (32-bit) → `0x00000000FFFFFFFF` (that's 4,294,967,295).
- **Sign extension:** fill the new top bits with copies of the sign bit. `0xFFFFFFFF` (32-bit, meaning −1) → `0xFFFFFFFFFFFFFFFF` (still −1).

This matters a lot in Bridge-V. RISC-V's 32-bit "W" instructions (like `addw`) **sign-extend** their results to 64 bits. x86's 32-bit instructions **zero-extend** them. So when Bridge-V translates `addw`, it must add an extra x86 instruction (`movsxd`) to fix the top bits. A tiny detail like this, got wrong, breaks programs.

### 1.6 Overflow and wrapping

A 64-bit register can't hold a number bigger than 2⁶⁴ − 1. If you add past that, the result **wraps around** (like a car's odometer going from 999999 to 000000). In Rust you will see `wrapping_add` and `wrapping_sub`: they mean "add, and wrap around on overflow instead of crashing".

### 1.7 Little-endian

A 4-byte number like `0x00150513` must be stored as 4 separate bytes in memory. **Little-endian** machines (both RISC-V and x86) store the *lowest* byte first:

```
number:    0x00150513
in memory: 13 05 15 00      (address +0 holds 0x13, +1 holds 0x05, …)
```

So when you look at raw memory, multi-byte numbers appear "backwards". In Rust, `u32::from_le_bytes` and `to_le_bytes` convert between the two views (`le` = little-endian).

### 1.8 Bit tricks you will see everywhere

| Operation | Symbol | Example | Use |
|---|---|---|---|
| AND | `&` | `x & 0xFF` keeps the low 8 bits | extract bits ("masking") |
| OR | `\|` | `x \| 0x1` sets bit 0 | set bits |
| XOR | `^` | `x ^ y` | flip bits, compare |
| NOT | `!` (Rust) | `!0xFFF` | all bits flipped |
| shift left | `<<` | `x << 12` = x × 4096 | move bits up, multiply by powers of 2 |
| shift right | `>>` | `x >> 12` = x ÷ 4096 | move bits down, extract high parts |

Two patterns to memorize:
- **Extract bits hi..lo** of `x`: `(x >> lo) & ((1 << (hi - lo + 1)) - 1)`. Bridge-V's decoder does exactly this in its `bits()` helper.
- **Round down to a 4 KiB page**: `x & !0xFFF` (clear the low 12 bits). The page offset is `x & 0xFFF`.

---

## 2. The CPU

### 2.1 What a CPU does

The **CPU** (Central Processing Unit, the processor) is the chip that runs programs. It does one thing, forever, billions of times per second:

```
loop {
    1. FETCH:   read the next instruction from memory (at the address in the program counter)
    2. DECODE:  work out what the instruction means ("add these two registers")
    3. EXECUTE: do it
    4. move the program counter to the next instruction (or somewhere else, for a jump)
}
```

This is called the **fetch-decode-execute cycle**. Remember it: Bridge-V's interpreter is a software copy of this loop.

### 2.2 Registers

**Registers** are tiny, extremely fast storage slots *inside* the CPU. Each one holds a single number (64 bits here). Almost all computation happens in registers: "add register 5 and register 6, put the result in register 7".

- RISC-V has **32** general-purpose registers, named `x0` to `x31`. (`x0` is special: it always reads as zero.)
- x86-64 has **16**, named `RAX`, `RBX`, `RCX`, `RDX`, `RSI`, `RDI`, `RBP`, `RSP`, `R8` … `R15`.

Notice the problem already: RISC-V programs expect 32 registers, but the x86 CPU running them only has 16. A big part of Bridge-V (file 08) is solving exactly this.

### 2.3 The program counter

The **program counter (PC)** is a special register that holds the address of the instruction being executed. Normally it just moves to the next instruction. **Jumps** and **branches** change it to somewhere else. That is how loops, `if` statements and function calls work.

- A **branch** is a conditional jump: "if a0 ≠ a4, jump to 0x10990".
- A **jump** is unconditional: "jump to 0x10a00".
- An **indirect jump** jumps to an address stored in a register, so the target is only known at run time. Function returns are indirect jumps (`ret` jumps to the address saved in the return-address register).

### 2.4 Memory

**Memory** (RAM) is a huge array of bytes. Each byte has a number called its **address**. If memory is an array called `mem`, then address `0x1000` is just `mem[0x1000]`.

- A **load** instruction copies bytes from memory into a register: `ld a5, 0(a4)` means "read 8 bytes at the address in a4, put them in a5".
- A **store** copies a register into memory: `sd a5, 8(sp)`.

Memory is much slower than registers, which is why the CPU (and Bridge-V) tries to keep values in registers.

### 2.5 The stack

The **stack** is a region of memory that programs use for temporary data: local variables, saved registers, return addresses. It grows downward. A register called the **stack pointer** (`sp` on RISC-V, `RSP` on x86) points to its top. When a function is called, it "pushes" things onto the stack. When it returns, it "pops" them off.

### 2.6 Instructions and machine code

An **instruction** is one basic command for the CPU: add, load, store, compare, jump. Here is the key idea: **an instruction is just a number.** The CPU reads that number from memory and decodes its bits.

Example: the RISC-V instruction "add 1 to register a0" is the 32-bit number `0x00150513`. Its bits are split into fields:

```
0x00150513 = 000000000001 01010 000 01010 0010011
             └── imm=1 ─┘ └rs1┘ └f3┘ └rd─┘ └opcode┘
                          =a0         =a0   =0x13 (OP-IMM: "arithmetic with an immediate")
```

The CPU looks at the `opcode` field to learn "this is an add-immediate", then reads the other fields. File 02 explains this fully.

- **Machine code** = the raw instruction numbers (bytes) that a CPU executes.
- **Assembly language** = the same instructions written as text for humans: `addi a0, a0, 1`.
- An **assembler** turns assembly text into machine code. A **disassembler** does the reverse. Bridge-V contains both a RISC-V disassembler (for debugging) and an x86 **machine-code emitter**, a small assembler that writes x86 bytes directly.

### 2.7 ISA: the instruction set architecture

An **ISA** (Instruction Set Architecture) is the complete "language contract" of a CPU family: which instructions exist, how they are encoded as bits, how many registers there are, and exactly what each instruction does.

- **RISC-V** is an ISA (open and free, from UC Berkeley, 2010).
- **x86-64** is an ISA (Intel and AMD PCs and servers).
- **ARM64** is an ISA (phones, Apple M-series Macs).

Different ISAs cannot run each other's machine code. RISC-V bytes are meaningless to an x86 CPU. That is the problem Bridge-V solves.

Two design styles:
- **RISC** (Reduced Instruction Set Computer): few, simple, fixed-size instructions. RISC-V instructions are all 4 bytes (or 2 bytes for "compressed" ones). Easy to decode.
- **CISC** (Complex Instruction Set Computer): many instructions, variable size. x86 instructions are 1 to 15 bytes long, with many special cases. Hard to encode by hand.

### 2.8 Flags

Many x86 instructions set **flags**: single bits in a special register (EFLAGS) that record facts about the last result, such as "was it zero?" (ZF) or "was there a carry?" (CF). A conditional jump then checks the flags: `cmp rax, rbx` followed by `jne target` means "compare, then jump if they were not equal".

RISC-V has **no flags**. Its branch instructions compare two registers directly (`bne a0, a4, target`). This makes RISC-V easier to translate: Bridge-V just emits `cmp` + `jne`.

### 2.9 Branch prediction (a speed detail you will need later)

Modern CPUs are **pipelined**: they start working on the next instructions before the current one finishes. At a branch, the CPU doesn't know yet which way it will go, so it **predicts** and runs ahead. If the guess was wrong, it throws the work away (about 15–20 cycles lost).

Predictors are good at branches that behave consistently. They are bad at one indirect jump that goes to a different place every time. This fact explains why **block chaining** (file 10) makes Bridge-V so much faster.

---

## 3. From C code to a running program

### 3.1 Compiling

You write C (or Rust, or Go…). A **compiler** such as `gcc` translates it into machine code for one specific ISA. The command decides which ISA:

```
gcc hello.c -o hello                          → x86-64 machine code (runs on your PC)
riscv64-linux-gnu-gcc hello.c -o hello-rv     → RISC-V machine code (does NOT run on your PC)
```

The second one is a **cross-compiler**: it runs on x86 but produces RISC-V code. Bridge-V's test programs are built this way.

Along the way:
- `-O0` means "no optimization" (simple, slow code), `-O2` means "optimize" (fast code). Bridge-V tests both.
- A **linker** combines your code with library code (like `printf` from the C library, **libc**) into one executable file.
- **Static** linking copies the library code into the file. **Dynamic** linking leaves it in separate `.so` files that a helper program (`ld.so`, the **dynamic loader**) loads at start-up.

### 3.2 The executable file: ELF

On Linux, executables use the **ELF** format (Executable and Linkable Format). An ELF file contains:
- a **header**: "this is a 64-bit RISC-V program, start executing at address 0x10400" (the **entry point**)
- **segments**: chunks of bytes to load into memory at given addresses, with permissions (code: read + execute; data: read + write)
- optional **symbols**: names for addresses (like `main`)

Bridge-V's first job is to read an ELF file and put its segments into memory: [`src/elf.rs`](../../src/elf.rs) and [`src/user/loader.rs`](../../src/user/loader.rs).

---

## 4. The operating system

### 4.1 Kernel and processes

The **operating system (OS)**, such as Linux or Windows, manages the computer. Its core is the **kernel**, which has full control of the hardware.

A running program is a **process**. Each process thinks it has the machine to itself. The kernel shares the CPU between processes (switching between them many times per second) and keeps their memory separate.

### 4.2 Privilege levels

The CPU enforces the separation with **privilege levels**. Normal programs run in **user mode**: they cannot touch hardware directly or access other processes' memory. The kernel runs in a **privileged mode** and can do anything.

- RISC-V has three: **U** (user), **S** (supervisor: the kernel), **M** (machine: firmware, the most powerful).
- Bridge-V's "system mode" emulates all three, so it can run a real Linux kernel.

### 4.3 System calls

A user program can't print to the screen or open a file by itself. It asks the kernel with a **system call** ("syscall"). On RISC-V, the program puts a syscall number in register `a7` (for example 64 = `write`), arguments in `a0`–`a5`, and executes the `ecall` instruction. The kernel does the work and puts the result in `a0`.

Bridge-V's "user mode" pretends to be the Linux kernel. When a RISC-V program does `ecall`, Bridge-V catches it and performs the equivalent syscall on the real (x86) Linux kernel. See file 15.

### 4.4 Virtual memory and pages

Each process sees its own private address space. Address `0x10000` in process A and address `0x10000` in process B are different physical memory. This is **virtual memory**:

- Memory is divided into **pages**, usually **4 KiB** (4096 bytes = `0x1000`).
- The kernel keeps a **page table** for each process: a map from virtual page → physical page, plus **permissions** for each page: **R**ead, **W**rite, e**X**ecute.
- The CPU hardware that uses the page table on every access is the **MMU** (Memory Management Unit).
- Walking the page table on every access would be slow, so the CPU caches recent translations in a **TLB** (Translation Lookaside Buffer).

If a program touches a page it has no permission for (or one that isn't mapped), the MMU raises a **page fault**. The kernel usually kills the program with a **segmentation fault** (signal **SIGSEGV**).

Bridge-V uses this in two ways:
1. In system mode it *emulates* a RISC-V MMU and TLB in software (a "softmmu", file 11).
2. It deliberately *uses* the host's SIGSEGV as a cheap way to detect bad guest memory accesses (files 11 and 12).

### 4.5 Signals

A **signal** is the kernel interrupting a process to tell it something: SIGSEGV (bad memory access), SIGINT (Ctrl+C), SIGPROF (a profiling timer). A program can install a **signal handler**: a function the kernel calls when the signal arrives. Bridge-V installs a SIGSEGV handler ([`src/user/signal.rs`](../../src/user/signal.rs)) to catch faults inside generated code.

### 4.6 mmap and mprotect

Programs ask the kernel for memory with the **`mmap`** syscall ("map N bytes, readable and writable"), and change permissions with **`mprotect`**. Bridge-V uses these constantly: to reserve the guest's address space, to create the memory that holds generated x86 code, and to write-protect pages.

---

## 5. Running code for a different CPU

### 5.1 Guest and host

- The **guest** is the machine being imitated: RISC-V.
- The **host** is the real machine doing the work: your x86-64 PC.

An **emulator** makes the host behave like the guest. There are three ways to do it. Imagine a book in Japanese and a reader who only knows English:

1. **Interpreter.** A translator reads one Japanese sentence, says it in English, then moves to the next. It is simple and always correct, but slow, and a sentence read ten times is translated ten times. For a CPU: a loop that fetches one guest instruction, decodes it, and runs a `match` on its type, every time.
2. **Static (ahead-of-time) translation.** Translate the whole book before reading. For machine code this is very hard: you can't always tell code from data, jump targets can be computed at run time, and programs can create new code while running.
3. **Dynamic binary translation (DBT).** Translate each passage the first time you reach it, and keep the translation. Hot passages (loops) are translated once and then read at full speed. Code that is never run never costs anything.

**Bridge-V is a dynamic binary translator.** It also contains an interpreter, used as the reference ("golden model") for testing.

### 5.2 JIT compilation

**JIT** means **Just-In-Time** compilation: generating machine code while the program is running, then jumping into it. JavaScript engines (V8), the Java VM and .NET all use JITs. Bridge-V is a JIT whose input is RISC-V machine code instead of JavaScript or Java bytecode.

The trick at the heart of every JIT:
1. Ask the OS for some memory.
2. Write machine-code bytes into it.
3. Make that memory executable.
4. Treat its address as a function pointer and call it.

The CPU then runs your freshly written bytes as if they were a normal compiled function. File 09 shows exactly how Bridge-V does this safely.

### 5.3 Basic blocks

A **basic block** is a straight run of instructions with one entry (the first instruction) and one exit (a branch or jump at the end). No jumps land in the middle of it. Bridge-V translates one basic block at a time. A translated block is called a **TB** (Translation Block).

---

## 6. The Rust you need to read this code

Bridge-V is written in **Rust**. You don't need to be a Rust expert to follow it, but these pieces appear everywhere.

### 6.1 Basic types

| Type | Meaning |
|---|---|
| `u8`, `u16`, `u32`, `u64` | unsigned integers of 8/16/32/64 bits |
| `i8`, `i32`, `i64` | signed (two's complement) integers |
| `usize` | unsigned integer the size of a pointer (64 bits here), used for indexes |
| `bool` | `true` / `false` |
| `[u64; 32]` | a fixed array of 32 `u64`s (the guest registers are exactly this) |
| `Vec<T>` | a growable array |
| `&T`, `&mut T` | a reference (borrowed pointer) to a `T`, read-only or writable |
| `Box<T>` | a `T` stored on the heap |

**Casts** use `as`: `x as u32` keeps the low 32 bits, `x as i32 as i64` sign-extends a 32-bit value, `x as u32 as u64` zero-extends. You will see chains like `v as u32 as i32 as u64` in the interpreter. Read them left to right: each `as` is one conversion step.

### 6.2 Structs, enums and `match`

A **struct** groups fields:
```rust
pub struct CpuState {
    pub x: [u64; 32],   // the 32 guest registers
    pub pc: u64,        // the guest program counter
    // ...
}
```

An **enum** is a value that is one of several variants, each with its own data. The decoded RISC-V instruction is an enum:
```rust
pub enum Inst {
    Lui { rd: Reg, imm: i64 },
    Jal { rd: Reg, imm: i64 },
    Load { op: LoadOp, rd: Reg, rs1: Reg, imm: i64 },
    // ... about 25 variants
}
```

**`match`** picks the code for each variant. It is like a very powerful `switch`:
```rust
match d.inst {
    Inst::Lui { rd, imm } => cpu.set_x(rd, imm as u64),
    Inst::Jal { rd, imm } => { /* ... */ }
    _ => { /* anything else */ }
}
```

### 6.3 Methods, `impl` and traits

`impl CpuState { fn set_x(&mut self, ...) }` adds methods to a struct. `self` is the object itself (`this` in other languages).

A **trait** is an interface: a set of methods that several types implement. Bridge-V's most important trait is `Engine` ([`src/interp/mod.rs`](../../src/interp/mod.rs)):
```rust
pub trait Engine {
    fn run(&mut self, cpu: &mut CpuState, mem: &mut DirectMem, env: &Env, max_insns: u64) -> Stop;
    fn flush(&mut self);
}
```
The interpreter, the JIT and the lockstep checker all implement `Engine`, so the rest of the program can use any of them through a `Box<dyn Engine>` ("a boxed value of some type that implements Engine").

### 6.4 `Option` and `Result`

Rust has no `null` and no exceptions. Instead:
- `Option<T>` is either `Some(value)` or `None`.
- `Result<T, E>` is either `Ok(value)` or `Err(error)`.
- The `?` operator means "if this is an error, return it from the current function right now".
- `if let Some(x) = ... { }` runs the block only when there is a value.

### 6.5 `unsafe`

Rust checks memory safety at compile time. A few things can't be checked, like calling into freshly generated machine code or using raw pointers from `mmap`. Those must be inside `unsafe { }` blocks. Bridge-V keeps `unsafe` in a small number of files, and each block has a `// SAFETY:` comment explaining why it is correct.

### 6.6 Attributes you will see

| Attribute | Meaning |
|---|---|
| `#[repr(C)]` | lay out this struct's fields in memory in declaration order, like C. Needed because generated x86 code reads `CpuState` fields at fixed byte offsets. |
| `extern "sysv64" fn` | use the System V calling convention, the one Linux x86-64 C code uses, so generated code can call this Rust function. |
| `#[inline(always)]` | always paste this function's body into its callers (speed). |
| `#[test]` | this function is a test, run by `cargo test`. |
| `const _: () = assert!(...)` | a check at compile time: the build fails if it is false. Used to pin `CpuState` field offsets. |

### 6.7 Cargo

**Cargo** is Rust's build tool:
- `cargo build --release` compiles an optimized binary to `target/release/bridgev`.
- `cargo test` runs all tests.
- `cargo fmt` formats the code; `cargo clippy` checks for common mistakes.

---

## 7. Putting it together: the one-paragraph mental model

A RISC-V program is a file of RISC-V machine code. Your x86 CPU can't run it. Bridge-V loads the file into memory, then repeatedly: takes the next basic block of RISC-V instructions, **decodes** them, **translates** them into equivalent x86 machine code, stores that code in executable memory, and **jumps into it** so your real CPU runs it at full speed. Translated blocks are **linked** so they jump directly to each other without coming back to Bridge-V. When the program needs the operating system (a system call), Bridge-V catches the request and forwards it to real Linux. When something goes wrong (a bad memory access), Bridge-V turns it into the exact error a real RISC-V machine would report.

Everything else in this study guide explains one part of that paragraph in detail.

---

## Check yourself

1. What is `0x2A` in decimal? In binary?
2. What is the difference between sign extension and zero extension? Extend the 8-bit value `0x80` both ways to 16 bits.
3. How are the bytes of the 32-bit number `0x12345678` laid out in memory on a little-endian machine?
4. How do you get the page number and the page offset of address `0x10994` (4 KiB pages)?
5. Describe the fetch-decode-execute cycle in your own words.
6. Why can't an x86 CPU run a RISC-V program directly?
7. What is the difference between an interpreter and a dynamic binary translator? Why is the second faster for loops?
8. What happens, step by step, when a RISC-V program calls `write` to print text?
9. What is a page fault, and what usually happens to the program?
10. What does `#[repr(C)]` do, and why would a JIT need it?

<details>
<summary>Answers to 1–4</summary>

1. `0x2A` = 2×16 + 10 = 42 = `0010 1010`.
2. Zero extension fills new high bits with 0; sign extension copies the sign bit. `0x80` → zero-extended `0x0080` (128), sign-extended `0xFF80` (−128).
3. `78 56 34 12` (lowest byte first).
4. Page number = `0x10994 >> 12` = `0x10`. Page start = `0x10994 & !0xFFF` = `0x10000`. Offset = `0x10994 & 0xFFF` = `0x994`.
</details>
