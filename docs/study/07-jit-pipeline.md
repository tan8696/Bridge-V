# 07 · The JIT pipeline: from RISC-V bytes to x86 bytes

## What you will learn

- the five stages of translation: **decode → lift → optimize → allocate + lower → place**
- what an **IR** (intermediate representation) is and why Bridge-V's is "SSA"
- the four optimizer passes, each with its algorithm and a worked example
- how IR operations become x86 instructions (instruction selection)
- how instructions the JIT doesn't translate are handled (the interpreter helper)
- a complete worked example: a real three-instruction loop, from bytes to IR to x86

Files: [`src/ir/`](../../src/ir/), [`src/backend/x86/lower_ir.rs`](../../src/backend/x86/lower_ir.rs), and `translate_insns()` / `tb_for_key()` in [`src/jit/dispatch.rs`](../../src/jit/dispatch.rs). Register allocation has its own file (08).

---

## 1. The pipeline at a glance

```
 guest bytes at pc
      │  1. DECODE          build_block_max()                 → Vec<Decoded>   (up to 128 insns)
      ▼
 [c.ld a5,0(a4); c.addi a4,a4,8; c.bnez a5,-4]
      │  2. LIFT            ir::lift::lift_with()              → ir::Block (list of Op)
      ▼
 v0 = x14; v1 = load [v0+0]; x15 = v1; v2 = x14; v3 = add v2, 8; x14 = v3; ...
      │  3. OPTIMIZE        ir::opt::optimize()  (forward, dead_writes, fold, dce)
      ▼
 v0 = x14; v1 = load [v0+0]; x15 = v1; v3 = add v0, 8; x14 = v3; br.Ne v1, 0 ...
      │  4. ALLOCATE + LOWER   backend::x86::lower_ir::translate()
      │                        (asks regalloc::linear_scan::Alloc for registers,
      │                         writes bytes with backend::x86::emit::Asm)
      ▼
 49 83 e9 03  0f 8c ..  48 8b 45 f0  48 8b 0c 03  49 89 cf  48 83 c0 08 ...
      │  5. PLACE           CodeMem::place()  +  TbCache::insert()
      ▼
 a TranslationBlock in executable memory, ready to run and to chain
```

Translation is fast: for all of CoreMark, the total translate time was **9.2 ms, about 0.07% of the run** (Phase 5). That matters, because a JIT pays translation time while the program runs. It is why Bridge-V uses simple, linear-time algorithms and not a heavyweight compiler like LLVM.

---

## 2. Stage 1: decode

Translation reuses the interpreter's `build_block_max(pc, mem, max_block)` (file 06). The JIT's limit is 128 instructions per block (`--max-block`). The result is a `Vec<Decoded>` plus an optional fetch fault.

---

## 3. Stage 2: lift to IR

### 3.1 Why an IR?

You *could* translate each RISC-V instruction straight to x86. Bridge-V's first JIT (`--regalloc=none`, [`lower.rs`](../../src/backend/x86/lower.rs)) did exactly that: load operands from memory into RAX/RCX, compute, store the result back to memory, one instruction at a time. It works, but it wastes effort: a register read by three instructions is loaded three times, a constant built by two instructions (`lui` + `addi`) is computed at run time, and so on.

An **intermediate representation** is a simpler, uniform "middle language" designed to make such waste easy to spot and remove. Optimizations are written once, against the IR, instead of against 100+ RISC-V instruction forms.

### 3.2 Bridge-V's IR ([`src/ir/ops.rs`](../../src/ir/ops.rs))

A block is a list of `Op`s. Every computed value gets a fresh name `V(n)`, printed `v0`, `v1`, …. **Each value is defined exactly once.** This property is called **SSA** (Static Single Assignment). Because a translation block is straight-line code (no loops or joins inside it), SSA is automatic here: just number results in order.

The main operations:

| Op | Meaning | Printed as |
|---|---|---|
| `Insn { pc, idx }` | marker: guest instruction number `idx` starts here | `--- #1 @0x10992` |
| `Const { dst, imm }` | `dst = constant` | `v5 = const 0x0` |
| `ReadReg { dst, g }` | `dst = guest register x[g]` | `v0 = x14` |
| `WriteReg { g, src }` | `x[g] = src` | `x14 = v3` |
| `Bin { op, dst, a, b }` | `dst = a op b` (add, sub, mul, div, slt, the W forms, …) | `v3 = add v1, v2` |
| `BinImm { op, dst, a, imm }` | `dst = a op constant` | `v3 = add v0, 8` |
| `Load { dst, addr, off, size, signed }` | `dst = memory[addr + off]` (may fault) | `v1 = load.u8 [v0+0]` |
| `Store { addr, off, val, size }` | `memory[addr + off] = val` (may fault) | `store.4 [v2+8], v7` |
| `Interp { raw, pc, idx }` | run this one instruction in the interpreter | `interp #3 0x30002573 @0x…` |
| `Branch { cond, a, b, taken, fall }` | terminator: conditional two-way exit | `br.Ne v1, v5 ? 0x10990 : 0x10996` |
| `Jump { pc }` | terminator: direct exit to a known pc | `jump 0x10a00` |
| `JumpInd { target }` | terminator: exit to a pc computed at run time (`jalr`) | `jump [v2]` |
| `Exit { kind, pc }` | terminator: ECALL, FENCE.I, or a fetch fault | `exit Ecall @0x…` |
| `ReadF`, `WriteF`, `FArith`, `FCmp`, `FToI`, `IToF`, `Unbox` | floating point (file 14) | |

Every block ends with exactly one terminator.

### 3.3 The lifter ([`src/ir/lift.rs`](../../src/ir/lift.rs))

`Lifter::insn()` turns one `Inst` into a few `Op`s. Examples:

```
addi a4, a4, 8        →  v2 = x14 ; v3 = add v2, 8 ; x14 = v3
lui  a0, 0x12345      →  v0 = const 0x12345000 ; x10 = v0
auipc a0, 0x2         →  v0 = const (pc + 0x2000) ; x10 = v0      (pc is known now: a constant!)
ld   a5, 0(a4)        →  v0 = x14 ; v1 = load.u8 [v0+0] ; x15 = v1
bne  a5, x0, -4       →  v4 = x15 ; v5 = const 0 ; br.Ne v4, v5 ? pc-4 : pc+2
jalr x0, 0(ra)        →  v0 = x1 ; v1 = add v0, 0 ; v2 = and v1, -2 ; jump [v2]
ecall                 →  exit Ecall @pc
csrr a0, cycle        →  interp #k <raw bits> @pc      (the interpreter will run it)
```

Rules the lifter enforces:
- **Reading x0 becomes `Const 0`; writing x0 is dropped.** (A load into x0 is still emitted, because it can fault.)
- **Anything the back end doesn't translate becomes `Interp`**: CSR instructions, atomics, privileged instructions, illegal instructions, and some FP. If that instruction ends a block (like a CSR access), a `Jump` to the next pc follows.
- **`AUIPC` becomes a constant**, because the lifter knows the pc at translation time.
- Floating point is lifted inline only in the "fast FP variant" of a block (file 14).

---

## 4. Stage 3: optimize ([`src/ir/opt.rs`](../../src/ir/opt.rs))

`optimize()` runs four passes, in this order. Each is a **single linear sweep** over the ops, so optimization time grows linearly with block size.

```rust
pub fn optimize(b: &mut Block) {
    forward(b);       // reuse known register values
    dead_writes(b);   // drop register writes that are overwritten
    fold(b);          // constant folding and simplification
    dce(b);           // dead code elimination
}
```

### 4.1 `forward`: guest-register forwarding and read CSE

**Idea:** if we already know what value guest register `g` holds (because we read it or wrote it earlier in this block), a later `ReadReg g` can reuse that value instead of reading again. ("CSE" = common subexpression elimination.)

**Algorithm:**
```
cur[g] = None for every guest register g        // "the value g is known to hold"
rename[v] = v for every value                   // "v is really this other value"
for op in ops:
    rewrite op's uses through rename
    match op:
        ReadReg { dst, g } if cur[g] = Some(v):  rename[dst] = v; drop this op
        ReadReg { dst, g }:                      cur[g] = dst
        WriteReg { g, src } if cur[g] == src:    drop this op        // g already holds src
        WriteReg { g, src }:                     cur[g] = src
        Interp { .. }:                           cur = all None      // helper may change anything
```

**Example:**
```
before                        after forward
v0 = x14                      v0 = x14
v1 = load.u8 [v0+0]           v1 = load.u8 [v0+0]
x15 = v1                      x15 = v1
v2 = x14        ← known: v0   (removed; v2 renamed to v0)
v3 = add v2, 8                v3 = add v0, 8
x14 = v3                      x14 = v3
v4 = x15        ← known: v1   (removed; v4 renamed to v1)
v5 = const 0                  v5 = const 0
br.Ne v4, v5                  br.Ne v1, v5
```

### 4.2 `dead_writes`: remove overwritten register writes

**Idea:** if a block writes `x5` and later writes `x5` again, the first write is useless… *unless* something in between could observe it. A load or store can fault, and at a fault the guest must see the exact register values at that instruction ("precise exceptions", file 12). A helper call (`Interp`) can read any register.

**Algorithm:**
```
pending[g] = None     // index of the last WriteReg g not yet "observed"
for i, op in ops:
    WriteReg { g }:  if pending[g] = Some(j): delete op j
                     pending[g] = i
    ReadReg { g }:   pending[g] = None             // the earlier write is used
    Load | Store | Interp:  pending = all None     // possible fault site: everything is visible
```

**Example:** `addi a4, a4, 1 ; addi a4, a4, 1` lifts (after forward) to `x14 = v1 ; … ; x14 = v2`. The first `x14 = v1` is deleted. Only the final value is ever written.

Writes that span a fault site can't be removed here, but the register allocator's **lazy write-back** (file 08) handles them: a value is stored only when it must be.

### 4.3 `fold`: constant folding and simplification

**Idea:** compute at translation time anything whose inputs are known constants, and simplify operations with special constants.

**Algorithm:** keep `k[v]` = the constant value of `v`, if known. For each op:
- `Bin` with **both** operands constant → `Const` (computed with `BinOp::eval`, which calls the interpreter's `alu`, so the result is guaranteed identical)
- `Bin` with a constant second operand that fits in 32 bits → `BinImm` (x86 can encode it as an immediate)
- `sub x, c` → `add x, -c`
- a commutative op (add, and, or, xor, mul) with a constant *first* operand → swap it into immediate form
- **identities** that make the op disappear (the result is just renamed to the input): `add x, 0`, `or x, 0`, `xor x, 0`, shifts by 0, `and x, -1`, `mul x, 1`
- `and x, 0` and `mul x, 0` → `Const 0`
- `Branch` on two constants → `Jump` to the side it would take
- `Branch` comparing a value with *itself* → `Jump`
- `JumpInd` to a constant target → `Jump`. This turns some indirect jumps into **direct, chainable** exits (file 10).

**Example: building a 32-bit constant**
```
lui  a0, 0x12345          v0 = const 0x12345000        x10 = v0
addi a0, a0, 0x678        (forward: v1 = x10 → v0)      v2 = add v0, 0x678       x10 = v2
                          fold: v0 is constant → v2 = const 0x12345678
                          dead_writes already removed the first x10 = v0
result:                   v2 = const 0x12345678 ; x10 = v2
```
Two guest instructions become one x86 `mov`.

### 4.4 `dce`: dead code elimination

**Idea:** remove computations whose results nobody uses.

**Algorithm:** walk the ops **backwards**, keeping `used[v]`. An op that is **pure** (no side effects: `Const`, `ReadReg`, `Bin`, `BinImm`, `ReadF`, `Unbox`) and whose result isn't used is deleted. Otherwise, mark everything it uses as used. **Loads are never deleted**, even if their result is unused, because they might fault, and that fault must still happen.

### 4.5 How the passes are proven correct

[`tests/ir_passes.rs`](../../tests/ir_passes.rs) generates random RISC-V blocks and checks, for each one, that:
1. the lifted IR, run by the IR evaluator ([`src/ir/eval.rs`](../../src/ir/eval.rs)), gives exactly the same result as the real interpreter, and
2. the IR still gives that result after each pass.

This isolates optimizer bugs from code-generation bugs.

---

## 5. Stage 4: allocate registers and lower to x86

`lower_ir::translate()` walks the optimized ops once, in order. For each op it asks the register allocator (file 08) "which x86 register holds this value?" / "give me a register for this new value", then emits x86 bytes through `Asm` (file 03).

### 5.1 The block prologue (budget check)

```rust
c.a.alu_ri(Size::B64, Alu::Sub, BUDGET_REG, n);   // sub r9, n      (n = guest instructions)
c.a.jcc(Cond::L, budget_stub);                    // jl  budget_stub
```

Every TB starts by charging for **all** its instructions at once. If the budget goes negative, it leaves immediately, before doing anything. Chained blocks can therefore never loop forever without returning to Rust (file 10). The budget lives in register R9, not memory: a memory-based counter formed a store-to-load dependency chain through every block, and moving it to a register made CoreMark 10.5% and Dhrystone 22.4% faster (decision D46).

### 5.2 Instruction selection: IR op → x86

| IR op | x86 emitted | Why |
|---|---|---|
| `add d, a, b` | `lea d, [a + b]` (or `add d, b` if d == a) | `lea` is a 3-operand add that doesn't touch flags |
| `add d, a, imm` | `lea d, [a + imm]` / `add d, imm` | |
| `sub d, a, b` | `mov d, a ; sub d, b` (or `neg d ; add d, a` if d == b) | x86 `sub` is 2-operand |
| `and/or/xor/mul` | `mov d, a ; and d, b` / `imul d, b` | |
| `slt d, a, b` | `cmp a, b ; setl d8 ; movzx d, d8` | `setcc` writes one byte |
| `sll d, a, b` | `shlx d, a, b` (BMI2) or `mov rcx, b ; shl` | BMI2 avoids needing RCX |
| `addw d, a, b` | `add d32, b32 ; movsxd d, d32` | RISC-V sign-extends W results |
| `mulh`, `div`, `rem` | move operands to R10/R11, free RAX/RDX, `imul`/`mul`/`idiv`/`div` with guards | x86 uses fixed RDX:RAX; RISC-V division never traps |
| `const` | nothing yet (the allocator remembers it; see file 08) | constants are materialized only when needed |
| `load` | `mov d, [rbx + a + off]` (+ `movsx`/`movzx` for small sizes) | RBX = guest memory base |
| `store` | `mov [rbx + a + off], v` (or an immediate) | |

**Division guards** (`div_signed()`), because x86 `idiv` crashes where RISC-V doesn't:
```
    test r11, r11          ; divisor == 0 ?
    je   zero              ;   → result -1 (div) or the dividend (rem)
    cmp  r11, -1           ; divisor == -1 ?
    jne  normal            ;   → -dividend (div; MIN stays MIN) or 0 (rem), no idiv at all
    neg  rax  / mov eax, 0
    jmp  done
normal:
    cqo                    ; sign-extend RAX into RDX
    idiv r11
done:
```

**Branches** become `cmp a, b` (or `test a, a` when comparing with 0) and a conditional exit (file 10). The condition mapping: `beq → je`, `bne → jne`, `blt → jl`, `bge → jge`, `bltu → jb`, `bgeu → jae`.

### 5.3 Loads and stores remember their "fault site"

Just before emitting each memory access, `record_site()` saves:
- its offset in the host code (`rip_off`)
- the guest instruction index and pc
- the register holding the address, the offset, size and direction
- the **state map**: where every not-yet-written-back guest register currently lives

If that exact host instruction later faults, the dispatcher uses this record to rebuild the precise guest state (file 12). The fast path contains **no extra code** for this: all the bookkeeping is on the Rust side, looked up only when a fault really happens (design decision D37).

In system mode (softmmu), each access instead gets an **inline TLB check** and a cold slow path (file 11).

### 5.4 Instructions the JIT doesn't translate: `Interp`

For an `Interp` op, `interp()` emits a call to `helper_interp_one(cpu, raw, pc)`:

```
    (sync_for_call: write back every dirty guest register, store the pinned registers,
     move every still-needed value to a spill slot, because the call destroys the pool)
    add  r9, rest             ; give back the budget for this and later instructions
    mov  [rbp+budget], r9     ; the helper reads the budget to keep icount exact
    lea  rdi, [rbp-128]       ; arg 1: &CpuState
    mov  esi, raw             ; arg 2: the instruction bits
    mov  rdx, pc              ; arg 3: its address
    call [rip + helper_table] ; → Rust: decode it again, run step(), handle the outcome
    mov  r9, [rbp+budget]
    test rax, rax             ; 0 = continue, 1 = leave the block (jump, trap, ecall, ...)
    jne  helper_exit
    sub  r9, rest             ; charge it again
    (reload the pinned registers: the helper may have changed them)
```

This is expensive (a full register sync), so it is used only for rare instructions. Common ones are translated natively.

### 5.5 Terminators, stubs and the tail of the block

The terminator writes back any dirty guest registers, then emits the exits:
- `Branch` → `cmp` + `jcc rel32` (slot 1, taken) + `jmp rel32` (slot 0, fall-through)
- `Jump` → `jmp rel32` (slot 0)
- `JumpInd` → the inline jump-cache lookup (file 10)
- `Exit` → a jump to a special stub (ECALL, FENCE.I, fetch fault)

Every exit initially jumps to a **stub** at the end of the block. `emit_stubs()` writes them after the hot code, so the fast path is one straight line with cold code out of the way:

```
stub: add r9, refund          ; (if some instructions didn't run)
      mov [rbp+pc], target    ; where the guest continues
      mov [rbp+exit_reason], r ; (special exits only)
      mov eax, (tb_id << 2) | slot
      jmp exit_jit
```

`translate()` returns `LoweredIr { code, pcmap, exits, fault_sites, stats }`:
- `pcmap`: host offset → guest instruction (one entry per `Insn` marker)
- `exits`: for each of the two chainable slots, the offset of its rel32 field, its stub, and its target pc

### 5.6 Out of spill slots

`CpuState` has 64 spill slots. If a block needs more, the allocator returns `OutOfSlots`, and `tb_for_key()` retranslates the block with half as many instructions (D38). One instruction always fits.

---

## 6. Stage 5: place

`CodeMem::place(origin, &code)` copies the bytes into the code buffer at the next 16-byte-aligned address (through the writable view; file 09). The code was assembled *for* that exact address (`origin`), so absolute and relative addresses inside it are already right. Then `TbCache::insert()` records the `TranslationBlock`: guest pc, decoded instructions, host address and length, pcmap, exit slots, fault sites, and its key.

If the buffer is full, **everything** is flushed (the bump pointer resets, every TB is forgotten, a generation counter increases) and translation retries. This is rare: CoreMark's whole translation is 231 KiB against a 256 MiB buffer.

---

## 7. The complete worked example

The loop from [`docs/WHITEBOARD.md`](../WHITEBOARD.md) (glibc start-up code, scanning memory for a zero doubleword):

**Guest code:**
```
0x10990: 631c   c.ld   a5, 0(a4)
0x10992: 0721   c.addi a4, a4, 8
0x10994: fff5   c.bnez a5, 0x10990
```

**IR after lifting (left) and after optimizing (right):**
```
--- #0 @0x10990                         --- #0 @0x10990
  v0 = x14                                v0 = x14
  v1 = load.u8 [v0+0]                     v1 = load.u8 [v0+0]
  x15 = v1                                x15 = v1
--- #1 @0x10992                         --- #1 @0x10992
  v2 = x14            ── forward ──►      v3 = add v0, 8
  v3 = add v2, 8                          x14 = v3
  x14 = v3                              --- #2 @0x10994
--- #2 @0x10994                           v5 = const 0x0
  v4 = x15            ── forward ──►      br.Ne v1, v5 ? 0x10990 : 0x10996
  v5 = const 0x0
  br.Ne v4, v5 ? 0x10990 : 0x10996
```
(`load.u8` means an 8-byte unsigned load.)

**x86 generated** (a5 = x15 is pinned in R15; a4 = x14 is not pinned, so its home is `[rbp-0x10]`):
```
sub  r9, 3               ; charge 3 guest instructions
jl   budget_stub
mov  rax, [rbp-0x10]     ; v0 = x14          (fill from CpuState)
mov  rcx, [rbx+rax]      ; v1 = load [v0]    ← fault site recorded here, no check code
mov  r15, rcx            ; x15 = v1          (pinned: just a register move)
add  rax, 8              ; v3 = v0 + 8       (v0 is dead after this, so RAX is reused)
mov  [rbp-0x10], rax     ; write back dirty x14 before leaving
test rcx, rcx            ; bne v1, 0  → compare with zero = test
(2-byte nop)             ; align the next rel32 field to 4 bytes
jne  stub_1              ; slot 1: loop again   (later patched to jump to this TB's own start)
(3-byte nop)
jmp  stub_0              ; slot 0: fall through (later patched to the next TB)
```

**9 hot x86 instructions for 3 guest instructions**, one memory load for the guest load, and nothing at all for the guest register `a5`. After chaining, the loop is `jne` back to its own `sub r9, 3`, running entirely in native code.

---

## 8. See it yourself

```
bridgev run --engine jit --dump-ir /tmp/d --dump-x86 /tmp/d guest/build/fib-O2.elf 20
```
- `/tmp/d/tb_<pc>.ir` — the IR before and after the passes
- `/tmp/d/tb_<pc>.txt` — the guest disassembly and the host code (readable text if built with `cargo build --release --features disasm`; hex otherwise)
- `/tmp/d/links.txt` — every chain patch

The three levels can be compared with `--regalloc none` (no IR at all), `--regalloc pinned` (IR without optimization, every register write stored immediately) and `--regalloc linear` (the default: all passes plus lazy write-back). On CoreMark they gave 7,024 → 10,168 → 13,498 iterations per second (file 18).

---

## Check yourself

1. Name the five stages of translation and the function that does each.
2. What is SSA, and why is it automatic for a single translation block?
3. Run `forward` by hand on: `v0 = x5 ; v1 = add v0, 1 ; x6 = v1 ; v2 = x6 ; v3 = x5 ; x7 = v3`.
4. Why can't `dead_writes` remove a register write that comes before a load?
5. Why does `dce` never remove a load?
6. How does `fold` turn `lui` + `addi` into a single constant?
7. Why does `auipc` always become a constant?
8. What code does the back end emit for a `div`, and why are the guards necessary?
9. What happens to a CSR instruction during translation and at run time?
10. What is a fault site, and why is recording it better than emitting check code?
