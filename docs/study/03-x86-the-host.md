# 03 · x86-64, the host: the machine language Bridge-V writes

## What you will learn

- x86-64's 16 registers and the special jobs Bridge-V gives each one
- the **calling convention**: which registers survive a function call (this drives the whole register design)
- how an x86 instruction is built from bytes: prefixes, **REX**, opcode, **ModRM**, **SIB**, displacement, immediate
- the four encoding "gotchas" that break naive encoders
- how jumps encode their targets (**rel32**), which is what block chaining patches
- where this lives in Bridge-V: [`src/backend/x86/`](../../src/backend/x86/)

---

## 1. Why x86 is harder than RISC-V

RISC-V instructions are all 4 bytes with fields in fixed places. x86 grew over 45 years (from 16-bit 8086 to 32-bit to 64-bit), keeping backward compatibility each time. The result is:
- instructions of **1 to 15 bytes**
- optional **prefix** bytes that change the meaning of what follows
- register numbers split across two different bytes
- special cases for particular registers

Bridge-V writes x86 bytes **by hand**, with no assembler library. That is one of the core skills the project demonstrates. The encoder is [`src/backend/x86/emit.rs`](../../src/backend/x86/emit.rs) (about 1,100 lines), and it is tested against an independent disassembler (`iced-x86`) in [`tests/emitter_golden.rs`](../../tests/emitter_golden.rs).

---

## 2. Registers

x86-64 has 16 general-purpose 64-bit registers. Each has a number from 0 to 15 that appears in the encoding:

| # | Name | # | Name |
|---|---|---|---|
| 0 | RAX | 8 | R8 |
| 1 | RCX | 9 | R9 |
| 2 | RDX | 10 | R10 |
| 3 | RBX | 11 | R11 |
| 4 | RSP (stack pointer) | 12 | R12 |
| 5 | RBP | 13 | R13 |
| 6 | RSI | 14 | R14 |
| 7 | RDI | 15 | R15 |

The same register can be used at smaller sizes: `RAX` (64-bit), `EAX` (low 32 bits), `AX` (16), `AL` (8). **A write to a 32-bit register (`EAX`) zeroes the upper 32 bits of `RAX`.** Remember this: it is why RISC-V `addw` needs a fix-up.

There are also 16 SSE registers `XMM0`–`XMM15` for floating point, and flags (`EFLAGS`: zero ZF, sign SF, carry CF, overflow OF).

The enum is `Reg` in [`src/backend/x86/regs.rs`](../../src/backend/x86/regs.rs); its values are exactly the encoding numbers.

---

## 3. The calling convention (System V AMD64)

When one function calls another on Linux x86-64, both sides follow the **System V ABI**:

- **Arguments** go in `RDI, RSI, RDX, RCX, R8, R9` (in that order). The return value comes back in `RAX`.
- **Callee-saved** registers: `RBX, RBP, R12, R13, R14, R15` (and `RSP`). A called function must restore them before returning. So their values **survive** a call.
- **Caller-saved** registers: `RAX, RCX, RDX, RSI, RDI, R8–R11` and all `XMM`. A called function may destroy them.
- `RSP` must be a multiple of 16 at every `call` instruction.

This single rule decides Bridge-V's register plan. Generated code sometimes calls Rust helper functions. Anything kept in a *callee-saved* register survives those calls for free. So Bridge-V puts its long-lived values there:

| Host register | Role in Bridge-V generated code | Why this one |
|---|---|---|
| **RBP** | pointer to `CpuState` (+128) | callee-saved; always needed |
| **RBX** | host address of guest memory (direct mode) | callee-saved; used by every load/store |
| **R12** | guest `sp` (x2), pinned | callee-saved |
| **R13** | guest `ra` (x1), pinned | callee-saved |
| **R14** | guest `a0` (x10), pinned | callee-saved |
| **R15** | guest `a5` (x15), pinned | callee-saved |
| R9 | the instruction **budget** (time-slice counter) | kept out of the pool |
| R10, R11 | scratch for the encoder (address math, TLB probe) | never hold values across ops |
| RAX, RCX, RDX, RSI, RDI, R8 | the **allocatable pool** for everything else | caller-saved; free between calls |

`POOL`, `CPU`, `MEM_BASE`, `BUDGET_REG` and `PINNED` in [`regs.rs`](../../src/backend/x86/regs.rs) define this table.

**Why RBP = &CpuState + 128?** Guest register `x[i]` is at byte `8*i` in `CpuState`. With RBP pointing 128 bytes in, `x[i]` is at `[rbp + 8*i − 128]`, and every register from x0 (−128) to x31 (+120) fits in a **one-byte signed displacement** (−128…+127). One byte instead of four makes every register load and store 3 bytes shorter.

---

## 4. Anatomy of an x86 instruction

```
[prefixes] [REX] [opcode: 1–3 bytes] [ModRM] [SIB] [displacement: 0/1/4 bytes] [immediate: 0/1/2/4/8 bytes]
```

Not every part is present. Here are the ones that matter.

### 4.1 REX prefix (one byte, `0100WRXB`)

x86 was designed for 8 registers (3-bit numbers). 64-bit mode added R8–R15 and 64-bit operands, stored in an extra prefix byte:

| Bit | Meaning |
|---|---|
| `0100` | fixed high nibble: `0x40`–`0x4F` |
| W | 1 = 64-bit operand size |
| R | the 4th bit of ModRM.reg's register number |
| X | the 4th bit of SIB.index's register number |
| B | the 4th bit of ModRM.rm's / SIB.base's register number |

So a register number 0–15 is split: its **low 3 bits** go in ModRM or SIB, its **4th bit** goes in REX. `Reg::low3()` and `Reg::rex_bit()` in `regs.rs` do exactly this split.

### 4.2 ModRM (one byte: `mod(2) reg(3) rm(3)`)

ModRM says what the operands are:

| `mod` | meaning of `rm` |
|---|---|
| `11` | `rm` is a register |
| `00` | memory at `[rm]` |
| `01` | memory at `[rm + disp8]` (1-byte displacement follows) |
| `10` | memory at `[rm + disp32]` (4-byte displacement follows) |

`reg` is the other register operand, **or** an extra opcode number (written `/digit` in manuals, e.g. `83 /0` = ADD, `83 /5` = SUB).

### 4.3 SIB (one byte: `scale(2) index(3) base(3)`)

For addresses like `[base + index*scale + disp]`. Bridge-V's direct-mode guest memory access is `[rbx + reg + offset]`, which needs a SIB byte.

### 4.4 Worked examples (real bytes from Bridge-V output)

**`mov rax, [rbp-0x10]`** (load guest `a4`, which is x14: 8·14 − 128 = −16) → `48 8B 45 F0`
```
48  REX: 0100 1 0 0 0 → W=1 (64-bit)
8B  opcode: MOV r64, r/m64
45  ModRM: 01 000 101 → mod=01 (disp8), reg=000 (RAX), rm=101 (RBP)
F0  disp8 = -16
```

**`add rax, 8`** → `48 83 C0 08`
```
48  REX.W
83  opcode: ALU r/m64, imm8 (the operation is in ModRM.reg)
C0  ModRM: 11 000 000 → mod=11 (register), reg=000 (/0 = ADD), rm=000 (RAX)
08  imm8 = 8
```

**`mov r15, rcx`** (write guest `a5`, pinned in R15) → `49 89 CF`
```
49  REX: 0100 1 0 0 1 → W=1, B=1 (rm register ≥ 8)
89  opcode: MOV r/m64, r64
CF  ModRM: 11 001 111 → reg=001 (RCX), rm=111 + B → 1111 = R15
```

**`cmp r14, rsi`** (compare guest `a0` with a value in RSI) → `49 39 F6`
```
49  REX.W + REX.B
39  opcode: CMP r/m64, r64  (computes r/m − reg)
F6  ModRM: 11 110 110 → reg=110 (RSI), rm=110 + B = R14
```

Bridge-V's `emit_op()` and `modrm_rm()` in [`emit.rs`](../../src/backend/x86/emit.rs) build exactly these bytes for any register combination.

---

## 5. The four encoding gotchas

These special cases catch every beginner who writes an x86 encoder. `modrm_rm()` handles each one, and `tests/emitter_golden.rs` sweeps all 16 registers in every position to prove it.

1. **RSP and R12 as a base need a SIB byte.** `rm = 100` doesn't mean "RSP"; it means "a SIB byte follows". So `[r12]` must be encoded with a SIB byte whose base is R12.
2. **RBP and R13 as a base need a displacement.** `mod = 00, rm = 101` doesn't mean `[rbp]`; in 64-bit mode it means "RIP-relative" (`[rip + disp32]`). So `[rbp]` must be encoded as `[rbp + 0]` with a one-byte zero displacement. (Bridge-V uses RBP for `CpuState` and pins guest `ra` in R13, so this matters constantly.)
3. **RSP can't be an index.** `index = 100` in SIB means "no index".
4. **Byte registers need care.** Without a REX prefix, byte-register numbers 4–7 mean the old `AH, CH, DH, BH`. To get `SIL`, `DIL`, `SPL`, `BPL` (the low bytes of RSI, RDI, RSP, RBP), a REX prefix must be present, even an "empty" `0x40`.

Two more rules about constants:
5. **32-bit writes zero-extend** (`mov eax, 5` clears the top of RAX).
6. **32-bit immediates in 64-bit instructions are sign-extended.** So a constant like `0x80000000` can't be used directly as a 64-bit immediate. `mov_imm()` picks the shortest correct form: `mov r32, imm32` for 0…2³²−1, `mov r64, simm32` for small negatives, otherwise the 10-byte `movabs r64, imm64`.

---

## 6. Jumps and rel32 (the key to block chaining)

x86 jumps usually store the target **relative to the next instruction**:

```
rel = target − (address of the instruction after the jump)
```

| Instruction | Bytes | Length |
|---|---|---|
| `jmp rel32` | `E9` + 4-byte rel | 5 |
| `jcc rel32` (conditional) | `0F 8x` + 4-byte rel | 6 |
| `jmp rel8` / `jcc rel8` | `EB`/`7x` + 1-byte rel | 2 |

The `x` in `0F 8x` is the **condition code**:

| cc | name | meaning | used for RISC-V |
|---|---|---|---|
| 4 | E | equal | `beq` |
| 5 | NE | not equal | `bne` |
| C | L | less (signed) | `blt` |
| D | GE | greater or equal (signed) | `bge` |
| 2 | B | below (unsigned <) | `bltu` |
| 3 | AE | above or equal (unsigned ≥) | `bgeu` |

**Example:** a `jne` at address `0x…0010` jumping to `0x…0200`: rel = `0x200 − (0x10 + 6)` = `0x1EA`, so the bytes are `0F 85 EA 01 00 00` (little-endian rel32).

**Why this matters:** to point an existing jump at a different target, you only need to rewrite its **4-byte rel32 field**. That is what "hot-patching" means in block chaining (file 10). Bridge-V also makes sure each rel32 field starts at an address divisible by 4, so the rewrite is a single atomic 4-byte store.

A jump can reach ±2 GiB, so everything in the code buffer must be within 2 GiB of everything else. Bridge-V caps the buffer at 1 GiB (default 256 MiB). Rust code, however, can be anywhere in memory, so calls from generated code to Rust helpers go through a table of absolute addresses at the start of the buffer: `call [rip + disp32]` (bytes `FF 15` + disp32).

---

## 7. The instructions Bridge-V emits most

| Instruction | What it does | Typical use |
|---|---|---|
| `mov` | copy | load/store guest registers, guest memory |
| `lea d, [a + b]` / `[a + imm]` | compute an address, no memory access, no flags | RISC-V `add`/`addi` (3-operand add) |
| `add, sub, and, or, xor` | arithmetic | RISC-V ALU ops |
| `imul` | multiply | `mul` |
| `mul` / `imul` (one operand), `div` / `idiv` | 128-bit multiply/divide in RDX:RAX | `mulh*`, `div`, `rem` |
| `shl/shr/sar`, or BMI2 `shlx/shrx/sarx` | shifts | `sll/srl/sra` |
| `movsxd`, `movsx`, `movzx` | sign/zero-extend | W ops, byte/half loads |
| `cmp` + `setcc` | compare, write 0/1 | `slt`, `sltu` |
| `cmp` / `test` + `jcc` | compare and branch | RISC-V branches |
| `jmp [mem]` | indirect jump | the jump cache for `jalr` |
| `lock xadd`, `lock cmpxchg`, `xchg` | atomic operations | in the encoder and tested, but not used yet: RISC-V AMOs currently run through the interpreter helper (file 07), because guest threads never run in parallel (file 15) |
| `addsd, mulsd, sqrtsd, vfmadd231sd, …` | SSE / FMA floating point | inline FP (file 14) |

**BMI2** (`shlx`) is used only if the CPU supports it. Bridge-V checks with the `cpuid` instruction at start-up ([`features.rs`](../../src/backend/x86/features.rs)); `--no-host-features` turns this off.

---

## 8. How the emitter API looks

Generated code is never built from raw bytes outside `emit.rs`. Other code calls typed methods on an `Asm` object:

```rust
let mut a = Asm::new(origin);                   // origin = where these bytes will live
a.load(Size::B64, Reg::Rax, Mem::base(Reg::Rbp, -16));   // mov rax, [rbp-16]
a.alu_ri(Size::B64, Alu::Add, Reg::Rax, 8);               // add rax, 8
let l = a.new_label();
a.jcc(Cond::Ne, l);                              // jne l   (target filled in later)
// ...
a.bind(l);                                       // l is here
let bytes = a.finish();                          // resolve label jumps, return Vec<u8>
```

**Labels** let the code jump forward to places that don't exist yet. `jcc` writes a placeholder and records a **fixup**; `finish()` fills in all the rel values once every label's position is known.

---

## Check yourself

1. Which x86 registers survive a function call? Why does Bridge-V pin guest registers in R12–R15 and not in RAX–RDX?
2. What are the four fields of a REX prefix, and why does x86-64 need them?
3. Encode `mov rax, [rbp-0x10]` by hand and explain every byte.
4. Why must `[rbp]` be encoded with a zero displacement?
5. What does `mov eax, 5` do to the top 32 bits of RAX?
6. How is a `jmp rel32` target computed? What single thing do you change to redirect the jump?
7. Why does Bridge-V keep `CpuState` pointer in RBP with a +128 bias?
8. Why can't generated code just `call` a Rust function with a rel32?
