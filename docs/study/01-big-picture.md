# 01 · The big picture: what Bridge-V does

## What you will learn

- what Bridge-V is, in one sentence and in one page
- the two **modes** (user mode, system mode) and the three **engines** (interpreter, JIT, lockstep)
- the main parts of the program and how data flows between them
- a story of what happens, from the moment you type a command to the moment the program ends

---

## 1. One sentence

**Bridge-V lets programs compiled for a RISC-V processor run on an ordinary x86-64 (Intel/AMD) computer, fast, by translating their machine code into x86 machine code while they run.**

It is written in Rust from scratch. It uses no code-generation library: it writes x86 bytes by hand.

Real-world products that use the same idea:
- **Apple Rosetta 2** runs Intel Mac apps on Apple Silicon (ARM) Macs.
- **QEMU** runs programs and whole operating systems for many CPU types. It is the standard tool for RISC-V on x86.
- **Microsoft Prism** runs x86 apps on ARM Windows laptops.
- Game-console emulators such as **Dolphin** (GameCube/Wii) run console games on PCs.

---

## 2. The problem and the solution in a picture

```
   RISC-V program (bytes that only a RISC-V CPU understands)
                 │
                 ▼
   ┌───────────────────────────────────────────────────────────┐
   │                        BRIDGE-V                            │
   │                                                            │
   │  read the next block of RISC-V instructions               │
   │         → translate it to x86 instructions                │
   │         → store the x86 bytes in a "translation cache"    │
   │         → jump into them: the real CPU runs them           │
   │         → link blocks together so they jump to each other │
   │                                                            │
   │  when the program asks the OS for something (syscall),    │
   │  Bridge-V forwards the request to real Linux              │
   └───────────────────────────────────────────────────────────┘
                 │
                 ▼
   Your x86-64 CPU runs the translated code at close to native speed
```

**How fast?** On the CoreMark benchmark, Bridge-V runs at **4.8 billion guest instructions per second**: 1.49× faster than QEMU and 52.5% of the speed of the same C code compiled directly for x86 (file 18).

---

## 3. Two modes

Bridge-V can imitate two different things. You choose with the command.

### 3.1 User mode: run one Linux program (`bridgev run`)

```
bridgev run --engine jit guest/build/hello-O2.elf
Hello, world!
```

Bridge-V pretends to be **a RISC-V CPU plus the Linux kernel**, for one program:
- it loads the program's ELF file into memory
- it builds the start-up stack (arguments, environment variables) the way Linux does
- it runs the program's code
- when the program makes a **system call** (print, open a file, get memory), Bridge-V performs it on the real x86 Linux kernel and hands back the result

The program never knows it isn't on real RISC-V hardware. `qemu-riscv64` does the same job. See file 15.

### 3.2 System mode: emulate a whole computer (`bridgev boot`)

```
bridgev boot --kernel Image --initrd rootfs.cpio
[    0.000000] Linux version 6.8.0-60-generic ... riscv64 ...
...
/ # uname -a
Linux (none) 6.8.0-60-generic ... riscv64 GNU/Linux
```

Here Bridge-V imitates **an entire RISC-V computer**: the CPU with all its privilege levels, virtual memory hardware, a timer, an interrupt controller, a serial port (the console you type into), and firmware. A real, unmodified Linux kernel boots on it to a shell prompt in about 1.5 seconds. `qemu-system-riscv64` does the same job. See file 16.

| | User mode | System mode |
|---|---|---|
| Command | `bridgev run prog.elf` | `bridgev boot --kernel Image` |
| What runs | one Linux program | a whole OS (Linux kernel + programs) |
| Who handles syscalls | Bridge-V (forwards them to host Linux) | the guest Linux kernel itself |
| Memory | "direct": guest address = host base + address (fast) | "softmmu": every address goes through emulated page tables |
| Privilege levels | only U (user) | M, S and U |
| Like | `qemu-riscv64` | `qemu-system-riscv64` |

There is also a small third mode, `bridgev run --mode bare`, used only to run the official RISC-V test suite (file 17).

---

## 4. Three engines

An **engine** is the part that actually executes guest instructions. You choose with `--engine`.

| Engine | How it works | Speed | Why it exists |
|---|---|---|---|
| `interp` (interpreter) | decodes blocks once, then runs each instruction with a Rust `match` | slow (~450 CoreMark it/s) | simple, obviously correct: the **golden model** everything else is checked against |
| `jit` | translates blocks into x86 machine code and runs them natively | fast (~13,400 CoreMark it/s) | the point of the project |
| `lockstep` | runs every block **twice**, once with the interpreter and once with the JIT, and compares the results | slow | finds JIT bugs: the first difference is printed with the guest and host code |

All three implement the same Rust trait, `Engine`, so the rest of the program doesn't care which one it is using. `make_engine()` in [`src/jit/mod.rs`](../../src/jit/mod.rs) creates the one you chose.

---

## 5. The main parts

```
                 ┌──────────────────────────────┐
  command line → │ main.rs (CLI, parses options) │
                 └──────────────┬───────────────┘
                                │
          ┌─────────────────────┴──────────────────────┐
          ▼                                            ▼
 ┌──────────────────────┐                   ┌─────────────────────────┐
 │ user/  (user mode)   │                   │ system/ (system mode)   │
 │ ELF loader, stack,   │                   │ machine, devices, SBI,  │
 │ syscalls, threads,   │                   │ devicetree, boot flow   │
 │ signals              │                   └───────────┬─────────────┘
 └──────────┬───────────┘                               │
            └──────────────┬────────────────────────────┘
                           ▼   engine.run(cpu, mem, ...)
        ┌────────────────────────────────────────────────────┐
        │ ENGINE: interp/ (interpreter) or jit/ (translator)  │
        └─────┬───────────────────────────────┬──────────────┘
              ▼                               ▼
   ┌─────────────────────┐      ┌───────────────────────────────────────────┐
   │ isa/  decoder        │      │ JIT pipeline                              │
   │ bytes → Inst          │      │ ir/lift → ir/opt → regalloc → backend/x86 │
   └─────────────────────┘      │ → jit/code_mem (executable memory)         │
                                 │ → jit/chain (linking), jit/dispatch (loop) │
                                 └───────────────────────────────────────────┘
        ┌────────────────────────────────────────────────────┐
        │ cpu/  CpuState (registers), CSRs, traps, FP         │
        │ mem/  guest memory, MMU (page walk), TLB, SMC       │
        └────────────────────────────────────────────────────┘
```

- **`CpuState`** ([`src/cpu/state.rs`](../../src/cpu/state.rs)) is the most important data structure. It holds the guest's registers `x[0..32]`, the program counter `pc`, the FP registers, the jump cache, the software TLB and more. Both the Rust code and the generated x86 code read and write it.
- **`DirectMem`** ([`src/mem/direct.rs`](../../src/mem/direct.rs)) holds the guest's memory.
- An **engine** takes a `CpuState` and a `DirectMem` and runs guest instructions until something needs attention (a syscall, an exception, a time limit).

File 04 lists every file in detail.

---

## 6. A story: the life of a program under Bridge-V

You type:

```
bridgev run --engine jit guest/build/fib-O2.elf 20
```

Here is what happens, in plain words. (File 05 follows the same story through the real function names.)

**1. Start-up.** `main()` parses the command line. It sees `run`, user mode, JIT engine, the file name and the argument `20`.

**2. Loading.** The loader reads the ELF file. It reserves a 256 GiB region of *virtual* address space for the guest (no real memory is used yet). It copies the program's code and data into that region at the addresses the ELF file asks for, with the right permissions. It builds the initial stack: `argc = 2`, `argv = ["fib-O2.elf", "20"]`, the environment variables and some extra information Linux normally provides. It creates a `CpuState` with `pc` = the entry point and `sp` = the top of the new stack.

**3. The JIT starts.** It allocates a code buffer (256 MiB) that will hold generated x86 code. It writes three small pieces of x86 code at the start of the buffer: `enter_jit` (to jump from Rust into generated code), `exit_jit` (to jump back) and `fault_exit` (for crashes). It installs a SIGSEGV handler.

**4. The dispatcher loop begins.** The dispatcher is a Rust loop. It asks: "Is there already a translation for the block at `pc`?" At first there isn't.

**5. Translation.** The translator:
- **decodes** the RISC-V bytes at `pc`, one instruction at a time, until it reaches a jump or branch (the end of the basic block)
- **lifts** the instructions to a simple **intermediate representation (IR)**
- **optimizes** the IR: folds constants, skips repeated register reads, removes register writes that are immediately overwritten
- **allocates** x86 registers to the values
- **emits** x86 machine-code bytes
- **places** the bytes in the code buffer and records the new **translation block (TB)** in a hash map

**6. Running.** The dispatcher calls `enter_jit` with the TB's address. Now the real CPU runs the translated code *natively*. There is no Rust code in the loop at all.

**7. Leaving.** At the end of the block the code needs to go to the next block. The first time, the next block isn't linked yet, so the code jumps to a small "exit stub", which stores the next `pc` and returns to the dispatcher.

**8. Chaining.** The dispatcher translates the next block (if needed) and **patches the exit jump** of the previous block so that it now points straight at the new block. Next time, the CPU goes from block to block without returning to Rust. After a short warm-up, almost all of the program runs this way: about **10 dispatcher returns per million guest instructions**.

**9. A system call.** `fib` prints its result with `printf`, which eventually executes `ecall` with `a7 = 64` (`write`). The translated code exits with reason "ECALL". The user-mode layer reads the registers, checks that the buffer is valid guest memory, calls the real Linux `write` on the host, puts the result in `a0`, moves `pc` past the `ecall`, and the dispatcher continues.

**10. The end.** The program calls `exit_group(0)`. Bridge-V stops the engine, prints statistics if you asked for `--stats`, and exits with the program's exit code.

Things that can happen along the way, and the file that explains each one:
- a bad memory access → the host sends SIGSEGV → Bridge-V turns it into an exact guest fault (file 12)
- the program writes over its own code → the old translations are thrown away (file 13)
- floating-point math → inline SSE instructions plus careful fix-ups (file 14)
- the time slice runs out → the block's "budget" check sends control back to the dispatcher, so long loops can't run forever unchecked (file 10)

---

## 7. The four hard subsystems (the heart of the project)

The project's design document names four "hard subsystems". An interviewer will most likely ask about these:

1. **Runtime machine-code generation.** A hand-written x86-64 encoder writes bytes into memory that is mapped twice, once writable and once executable (W^X). The buffer's address is then cast to a function pointer and called. → files 03, 07, 09
2. **Direct block chaining.** A block's exit jumps are rewritten ("hot-patched") to point directly at the next block, so execution rarely returns to the dispatcher. → file 10
3. **Register remapping and a spill allocator.** Four hot guest registers live permanently in x86 registers R12–R15. The rest are assigned per block by a linear-scan register allocator that keeps values in registers and saves them to memory only when needed. → file 08
4. **A software MMU.** For system mode, RISC-V virtual memory (Sv39/Sv48 page tables) is emulated, with a software TLB that the generated code checks inline in about 9 instructions. → file 11

---

## 8. How we know it works

Bridge-V never trusts itself. Correctness is proven in several independent ways (file 17):
- all **244 official RISC-V tests** pass under every engine
- test programs produce **byte-identical output** to QEMU
- **lockstep mode** compares the JIT with the interpreter after every block, including a whole Linux boot (84 million blocks, no difference)
- **random fuzzers** generate a million random instruction blocks and check the JIT against the interpreter, and a million floating-point cases against the reference FP library

---

## Check yourself

1. Explain what Bridge-V does in one sentence, without using the word "emulator".
2. What is the difference between user mode and system mode? Give an example command for each.
3. Why does Bridge-V keep an interpreter if the JIT is 30× faster?
4. In the story above, what is the dispatcher, and when does control return to it?
5. What does "chaining" change, and why does it matter?
6. Name the four hard subsystems.
