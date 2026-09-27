# 02 · RISC-V, the guest: the machine language Bridge-V reads

## What you will learn

- RISC-V's 32 registers and their nicknames
- what "RV64GC" means
- how a 32-bit RISC-V instruction is split into fields, with a full byte-by-byte example
- why immediates look "scrambled", and how compressed (16-bit) instructions work
- the instruction groups, and the tricky rules the translator must get exactly right
- privilege levels and CSRs (needed for system mode)
- where all of this lives in Bridge-V's code: [`src/isa/`](../../src/isa/)

---

## 1. RISC-V in one paragraph

RISC-V ("risk five") is an **open** instruction set: anyone can build a RISC-V chip without paying license fees. It was designed at UC Berkeley in 2010 to be simple and regular. Today it is used in microcontrollers, SSD controllers, AI accelerators, and increasingly in Linux boards and servers. Because real RISC-V hardware is still rare and slow, developers often run RISC-V software on x86 machines through translators like QEMU or Bridge-V.

## 2. What "RV64GC" means

The name lists the parts of the ISA that Bridge-V supports:

| Letter | Meaning |
|---|---|
| **RV64** | 64-bit registers and addresses |
| **I** | the base **I**nteger instructions (add, load, branch, …) |
| **M** | **M**ultiply and divide |
| **A** | **A**tomic memory operations (for multi-threaded code) |
| **F** | single-precision **F**loating point (32-bit `float`) |
| **D** | **D**ouble-precision floating point (64-bit `double`) |
| **G** | shorthand for "IMAFD + Zicsr + Zifencei" (the **G**eneral-purpose set) |
| **C** | **C**ompressed 16-bit versions of common instructions (smaller code) |
| Zicsr | instructions to read/write **C**ontrol and **S**tatus **R**egisters |
| Zifencei | the `fence.i` instruction (used when code is modified) |

This is exactly what a normal Linux distribution for RISC-V expects.

---

## 3. Registers

RISC-V has 32 integer registers, `x0`–`x31`, each 64 bits wide, plus the program counter `pc`. Programmers usually use their **ABI names**, which describe how the calling convention uses them:

| Register | ABI name | Used for |
|---|---|---|
| x0 | `zero` | **always 0.** Writes to it are thrown away. |
| x1 | `ra` | **r**eturn **a**ddress (where a function returns to) |
| x2 | `sp` | **s**tack **p**ointer |
| x3 | `gp` | global pointer |
| x4 | `tp` | thread pointer (thread-local storage) |
| x5–x7 | `t0`–`t2` | temporaries (a called function may overwrite them) |
| x8 | `s0` / `fp` | saved register / frame pointer |
| x9 | `s1` | saved register |
| x10–x11 | `a0`–`a1` | function **a**rguments **and return values** |
| x12–x17 | `a2`–`a7` | function arguments (`a7` also holds the syscall number) |
| x18–x27 | `s2`–`s11` | saved registers (a called function must preserve them) |
| x28–x31 | `t3`–`t6` | temporaries |

There are also 32 floating-point registers `f0`–`f31` (64 bits each).

**Remember for later:** Bridge-V permanently keeps four busy guest registers in x86 registers: `sp` (x2) in R12, `ra` (x1) in R13, `a0` (x10) in R14 and `a5` (x15) in R15. GCC uses `a5` a lot as a scratch register. File 08 explains why these four.

---

## 4. Instruction length

RISC-V instructions are either 32 bits (4 bytes) or 16 bits (2 bytes, "compressed"). The **lowest two bits** of the first 16-bit piece tell you which:

```
low 2 bits != 11                    → 16-bit compressed instruction
low 2 bits == 11 and bits 4:2 != 111 → 32-bit instruction
otherwise                            → 48-bit or longer (not supported → illegal)
```

This is `insn_len()` in [`src/isa/decode.rs`](../../src/isa/decode.rs). Because instructions can be 2 bytes, Bridge-V fetches code 16 bits at a time (`fetch16`).

---

## 5. The 32-bit instruction formats

Every 32-bit instruction has a 7-bit **opcode** in bits 6:0. The other fields depend on the **format**. There are six formats:

```
bit:  31        25 24    20 19    15 14  12 11         7 6       0
R   : [  funct7   ][  rs2  ][  rs1  ][funct3][    rd     ][ opcode ]   reg-reg: add, sub, mul...
I   : [      imm[11:0]     ][  rs1  ][funct3][    rd     ][ opcode ]   addi, loads, jalr
S   : [ imm[11:5] ][  rs2  ][  rs1  ][funct3][ imm[4:0]  ][ opcode ]   stores
B   : [imm[12|10:5]][  rs2 ][  rs1  ][funct3][imm[4:1|11]][ opcode ]   branches
U   : [           imm[31:12]                ][    rd     ][ opcode ]   lui, auipc
J   : [      imm[20|10:1|11|19:12]           ][    rd     ][ opcode ]   jal
```

- `rd` = **d**estination register (where the result goes)
- `rs1`, `rs2` = **s**ource registers
- `funct3`, `funct7` = extra bits choosing the exact operation
- `imm` = an **immediate**: a constant number stored inside the instruction

Notice that `rd`, `rs1` and `rs2` are **always in the same bit positions** in every format that has them. That makes decoding fast in hardware, and simple in Bridge-V.

### 5.1 Worked example: decoding `0x00150513`

Suppose memory contains the bytes `13 05 15 00`. Little-endian, that is the 32-bit number `0x00150513`.

**Step 1: length.** Low two bits of `0x0513` are `11`, and bits 4:2 are `100` (not `111`), so it is a 32-bit instruction.

**Step 2: write it in binary and cut it into fields.**
```
0x00150513 = 0000 0000 0001 0101 0000 0101 0001 0011

 imm[11:0]      rs1    funct3  rd     opcode
 000000000001   01010  000     01010  0010011
     = 1        = 10   = 0     = 10   = 0x13
```

**Step 3: look up the opcode.** `0x13` is **OP-IMM** (arithmetic with an immediate, I-format). `funct3 = 0` means **ADDI**.

**Step 4: result.** `addi x10, x10, 1`, which is `addi a0, a0, 1`: "a0 = a0 + 1".

In Bridge-V this is `decode(0x00150513)`, which returns
`Inst::OpImm { op: AluOp::Add, rd: 10, rs1: 10, imm: 1 }`.

### 5.2 Why immediates are sign-extended

All immediates are **sign-extended** to 64 bits (section 1.5 of file 00). `addi a0, a0, -1` stores `imm[11:0] = 0xFFF`, and the CPU treats it as −1. In Bridge-V's decoder, `imm_i(x)` computes this with one trick: `((x as i32) >> 20) as i64`. Shifting a *signed* number right copies the sign bit into the new top bits.

### 5.3 Why branch immediates look scrambled

Look at the B format: the immediate bits are stored in a strange order (`imm[12|10:5]` then `imm[4:1|11]`). This isn't random. The designers wanted:
1. the **sign bit** (the highest immediate bit) always at instruction bit 31, so sign extension is the same wire for every format, and
2. the **register fields** in the same place in every format.

The leftover immediate bits were packed into whatever positions remained. Branch targets are always even (instructions are at least 2 bytes), so bit 0 isn't stored at all. Bridge-V puts the pieces back together in `imm_b()`:

```rust
let v = (bits(x, 31, 31) << 12)   // imm[12]
      | (bits(x, 7, 7)   << 11)   // imm[11]
      | (bits(x, 30, 25) << 5)    // imm[10:5]
      | (bits(x, 11, 8)  << 1);   // imm[4:1]
sext(v, 13)                        // sign-extend the 13-bit value
```

### 5.4 The main opcodes

| Opcode | Name | Instructions |
|---|---|---|
| `0x37` | LUI | `lui rd, imm` → rd = imm << 12 |
| `0x17` | AUIPC | `auipc rd, imm` → rd = pc + (imm << 12) |
| `0x6F` | JAL | jump and link (function call) |
| `0x67` | JALR | jump to register + offset (returns, indirect calls) |
| `0x63` | BRANCH | beq, bne, blt, bge, bltu, bgeu |
| `0x03` | LOAD | lb, lh, lw, ld, lbu, lhu, lwu |
| `0x23` | STORE | sb, sh, sw, sd |
| `0x13` | OP-IMM | addi, slti, sltiu, xori, ori, andi, slli, srli, srai |
| `0x1B` | OP-IMM-32 | addiw, slliw, srliw, sraiw |
| `0x33` | OP | add, sub, sll, slt, sltu, xor, srl, sra, or, and, mul, mulh, div, rem… |
| `0x3B` | OP-32 | addw, subw, sllw, srlw, sraw, mulw, divw, remw… |
| `0x0F` | MISC-MEM | fence, fence.i |
| `0x73` | SYSTEM | ecall, ebreak, mret, sret, wfi, sfence.vma, csrrw/csrrs/csrrc… |
| `0x2F` | AMO | lr, sc, amoswap, amoadd, amoand, amoor, amoxor, amomin, amomax |
| `0x07`, `0x27` | LOAD-FP, STORE-FP | flw, fld, fsw, fsd |
| `0x43`–`0x4F` | MADD etc. | fused multiply-add |
| `0x53` | OP-FP | fadd, fsub, fmul, fdiv, fsqrt, fcvt, fmv, feq, flt… |

`decode()` in [`src/isa/decode.rs`](../../src/isa/decode.rs) is one big `match` on these opcodes. Anything reserved or unsupported becomes `Inst::Illegal(raw)`. The decoder **never panics**, even on garbage.

---

## 6. Compressed instructions (the "C" extension)

About half the instructions in typical code are compressed to 16 bits. Each compressed instruction is exactly equivalent to one 32-bit instruction, just with smaller fields. For example:
- only registers x8–x15 can appear in some 3-bit register fields (`rd'`, `rs1'`)
- immediates are smaller

Bridge-V's approach is simple: **expand each compressed instruction into its 32-bit equivalent** (`rvc::expand()` in [`src/isa/rvc.rs`](../../src/isa/rvc.rs)). Everything after that point (interpreter, IR, JIT) only sees normal `Inst` values. The only difference kept is `Decoded::len` (2 instead of 4), because the next `pc` and link addresses depend on it.

### Worked example: a real three-instruction loop

This loop is from glibc's start-up code (it appears in [`docs/WHITEBOARD.md`](../WHITEBOARD.md)):

```
address   bytes   meaning
0x10990:  631c    c.ld   a5, 0(a4)     → ld   x15, 0(x14)      load 8 bytes at a4 into a5
0x10992:  0721    c.addi a4, a4, 8     → addi x14, x14, 8      a4 += 8
0x10994:  fff5    c.bnez a5, 0x10990   → bne  x15, x0, -4      loop while a5 != 0
```

Decoding `0x0721` by hand:
```
0x0721 = 000 0 01110 01000 01
         │   │ │     │     └─ bits 1:0  = 01  → quadrant 1
         │   │ │     └─ bits 6:2  = 01000 = 8 → imm[4:0]
         │   │ └─ bits 11:7 = 01110 = 14 → rd = x14 (a4)
         │   └─ bit 12 = 0 → imm[5]
         └─ bits 15:13 = 000 → C.ADDI
→ addi x14, x14, 8
```

---

## 7. The instructions, by group

**Integer arithmetic.** `add`, `sub`, `and`, `or`, `xor`, shifts (`sll` left, `srl` right logical, `sra` right arithmetic), and set-less-than (`slt`, `sltu`: rd = 1 if rs1 < rs2 else 0). Most have an immediate form (`addi`, `andi`, `slli`, …).

**W ("word") forms.** `addw`, `subw`, `sllw`, … compute on the **low 32 bits** and **sign-extend** the 32-bit result to 64 bits. This is how C's `int` (32-bit) math is done on a 64-bit machine.

**Constants.** `lui rd, imm` puts `imm << 12` in rd. Together with `addi` it builds any 32-bit constant: `lui a0, 0x12345` + `addi a0, a0, 0x678` → `a0 = 0x12345678`. `auipc rd, imm` gives `pc + (imm << 12)`, used for position-independent addresses. Bridge-V's optimizer turns both of these into plain constants (file 07).

**Loads and stores.** `lb/lh/lw/ld` load 1/2/4/8 bytes and sign-extend; `lbu/lhu/lwu` zero-extend. `sb/sh/sw/sd` store. The address is always `register + 12-bit immediate`.

**Branches.** `beq, bne, blt, bge, bltu, bgeu`: compare two registers and jump to `pc + offset` if the condition holds. RISC-V has no flags register.

**Jumps.**
- `jal rd, offset`: `rd = pc + length`, then jump to `pc + offset`. A function call is `jal ra, func`.
- `jalr rd, offset(rs1)`: jump to `(rs1 + offset) & ~1`, `rd = pc + length`. A return is `jalr x0, 0(ra)`, written `ret`.

**Multiply/divide (M).** `mul`, `mulh` (high 64 bits of the 128-bit product, signed), `mulhu` (unsigned), `mulhsu` (signed × unsigned), `div`, `divu`, `rem`, `remu`, and W forms.

**Atomics (A).** `lr` (load-reserved) and `sc` (store-conditional) implement lock-free updates: `sc` only succeeds if nobody wrote the address since the matching `lr`. `amoadd`, `amoswap`, … do a read-modify-write in one step.

**Floating point (F, D).** Arithmetic, fused multiply-add, conversions, comparisons, moves (file 14).

**System.** `ecall` (system call), `ebreak` (breakpoint), `fence` (memory ordering), `fence.i` (code was modified), `mret`/`sret` (return from a trap), `wfi` (wait for interrupt), `sfence.vma` (flush the TLB), and `csrrw/csrrs/csrrc` (access control registers).

**Pseudo-instructions** are nicknames the assembler understands: `li a0, 5` (load immediate), `mv a0, a1` (= `addi a0, a1, 0`), `j label` (= `jal x0, label`), `ret` (= `jalr x0, 0(ra)`), `nop` (= `addi x0, x0, 0`), `beqz`/`bnez`.

---

## 8. The tricky rules (each one is a place where a translator can go wrong)

These are all tested in Bridge-V. An interviewer might ask about any of them.

1. **x0 is always zero.** Writes are discarded. But a *load* into x0 still reads memory, because it can fault or touch a device.
2. **W instructions sign-extend.** x86's 32-bit instructions zero-extend, so each translated W op needs an extra `movsxd`.
3. **Shift amounts are masked.** 64-bit shifts use only the low 6 bits of rs2; W shifts use 5. x86 happens to do the same.
4. **Division never traps.** Division by zero gives −1 (all ones), remainder gives the dividend. `MIN / −1` gives `MIN` (overflow), remainder 0. x86's `idiv` **crashes** (#DE exception) in both cases, so Bridge-V emits checks before every division. See `div_signed()` in [`src/backend/x86/lower_ir.rs`](../../src/backend/x86/lower_ir.rs) and the Rust reference in `alu()` in [`src/interp/mod.rs`](../../src/interp/mod.rs):
   ```rust
   AluOp::Div if b == 0 => u64::MAX,                         // x/0 = -1
   AluOp::Div => (a as i64).wrapping_div(b as i64) as u64,   // MIN/-1 = MIN
   AluOp::Rem if b == 0 => a,                                 // x%0 = x
   ```
5. **`mulhsu` has no x86 equivalent.** It is computed as `mulhu(a, b) − (a < 0 ? b : 0)` or through 128-bit math.
6. **JALR reads rs1 before writing rd.** `jalr ra, 0(ra)` is legal and common. The target is also ANDed with `~1`.
7. **`sltiu` compares with the sign-extended immediate treated as unsigned.** `sltiu rd, rs, -1` is "rs < 0xFFFF…FFFF".
8. **CSR instructions sometimes don't read or write.** `csrrs`/`csrrc` with rs1 = x0 must not write the CSR; `csrrw` with rd = x0 must not read it (reads can have side effects).
9. **Link addresses depend on instruction length.** `c.jalr` sets `rd = pc + 2`, `jalr` sets `rd = pc + 4`.

---

## 9. Privilege levels and CSRs (for system mode)

RISC-V has three privilege levels:

| Level | Name | Who runs there |
|---|---|---|
| M (3) | Machine | firmware (e.g. OpenSBI); full control |
| S (1) | Supervisor | the operating-system kernel (Linux) |
| U (0) | User | normal programs |

**CSRs** (Control and Status Registers) are special registers for OS work. The important ones:

| CSR | Purpose |
|---|---|
| `mstatus` / `sstatus` | global state: interrupt enable bits, previous privilege, FP state (FS), MMU tweaks (SUM, MXR, MPRV) |
| `mtvec` / `stvec` | the address to jump to when a trap happens (the trap **vector**) |
| `mepc` / `sepc` | the `pc` where the trap happened (**e**xception **pc**) |
| `mcause` / `scause` | why the trap happened (e.g. 13 = load page fault) |
| `mtval` / `stval` | extra information (e.g. the bad address) |
| `mie` / `mip` | which interrupts are enabled / pending |
| `medeleg` / `mideleg` | which traps M-mode **delegates** (hands down) to S-mode |
| `satp` | the MMU mode (Bare, Sv39, Sv48) and the root page-table address |
| `cycle`, `time`, `instret` | counters |

When something goes wrong (or an interrupt arrives), the CPU takes a **trap**: it saves `pc` in `xepc`, the reason in `xcause`, switches to a higher privilege level and jumps to `xtvec`. `mret`/`sret` return. File 12 explains this in detail. The code is in [`src/cpu/trap.rs`](../../src/cpu/trap.rs) and [`src/cpu/csr.rs`](../../src/cpu/csr.rs).

---

## 10. The calling convention (how functions talk to each other)

Compilers follow an agreed convention (the **ABI**, Application Binary Interface):
- arguments go in `a0`–`a7`; the return value comes back in `a0` (and `a1`)
- `jal ra, func` saves the return address in `ra`; the function returns with `ret`
- `s0`–`s11` and `sp` must be the same after the call as before (**callee-saved**)
- `t0`–`t6` and `a0`–`a7` may be overwritten (**caller-saved**)

Bridge-V's user mode also uses this for system calls: the number goes in `a7`, arguments in `a0`–`a5`, the result comes back in `a0`.

---

## 11. Where it lives in Bridge-V

| File | What it does |
|---|---|
| [`src/isa/inst.rs`](../../src/isa/inst.rs) | `enum Inst`: the decoded form of every instruction. `Decoded { inst, len, raw }` adds the length (2 or 4) and the raw bits. `ends_block()` says which instructions end a basic block. |
| [`src/isa/decode.rs`](../../src/isa/decode.rs) | `decode(u32) -> Inst` for 32-bit instructions, the immediate helpers (`imm_i`, `imm_s`, `imm_b`, `imm_u`, `imm_j`), `insn_len`, and `decode_parts`, which fetches the second halfword only if needed. |
| [`src/isa/rvc.rs`](../../src/isa/rvc.rs) | `expand(u16) -> Inst` for compressed instructions. |
| [`src/isa/disasm.rs`](../../src/isa/disasm.rs) | turns an `Inst` back into assembly text (for `--trace`, `bridgev disasm` and error messages). |
| [`tests/decoder_vectors.rs`](../../tests/decoder_vectors.rs) | checks the decoder against ~1,500 golden encodings produced by LLVM's assembler (`llvm-mc`). |

One design choice worth remembering: `Inst` is exactly **16 bytes** (checked at compile time). Small decoded instructions make the interpreter's cache-friendly arrays fast.

---

## Check yourself

1. How does Bridge-V know whether an instruction is 2 or 4 bytes long?
2. Decode `0x00150593` by hand. (Hint: only `rd` differs from the example.)
3. Why are branch immediates stored in a scrambled bit order?
4. What does `addw` do that `add` doesn't? Why does that need an extra x86 instruction?
5. What does RISC-V return for `7 / 0`? What would x86's `idiv` do?
6. Why does Bridge-V expand compressed instructions to their 32-bit form instead of handling them separately?
7. Which register holds the syscall number? Which holds the return value?
8. What are `mepc`, `mcause` and `mtvec` used for?

<details>
<summary>Answer to 2</summary>

`0x00150593`: opcode `0x13` (OP-IMM), funct3 0 (ADDI), rs1 = 10, imm = 1, rd = bits 11:7 of `0x593` = `01011` = 11. So `addi a1, a0, 1` ("a1 = a0 + 1"). Its bytes in memory are `93 05 15 00`.
</details>
