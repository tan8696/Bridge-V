# 08 · Register allocation: fitting 32 registers into 10

## What you will learn

- why register allocation is needed, and what "spill" and "fill" mean
- Bridge-V's three-part plan: **pinned** registers, a **pool**, and **memory homes**
- the **linear scan** algorithm, and why it becomes a single walk for a basic block
- **lazy write-back**: storing a guest register at most once per block
- the **eviction rule**: throw out the value used furthest in the future
- how the allocator makes crashes precise without slowing the fast path (**state maps**)
- what happens around helper calls

File: [`src/regalloc/linear_scan.rs`](../../src/regalloc/linear_scan.rs) (about 600 lines), used by [`src/backend/x86/lower_ir.rs`](../../src/backend/x86/lower_ir.rs).

---

## 1. The problem

RISC-V programs use 32 registers. x86-64 has 16, and Bridge-V needs several of those for itself (RBP = `CpuState`, RBX = guest memory base, R9 = budget, R10/R11 = scratch, RSP = stack). Only 10 are left for guest values: 4 pinned registers plus a pool of 6.

The **register allocator** decides, at every point in the generated code, which value lives in which x86 register. When there are more live values than registers, some must go to memory:
- **spill** = save a register's value to memory to free the register
- **fill** = load a value from memory into a register

Every spill and fill is a memory access that native code wouldn't need, so a good allocator does as few as possible.

---

## 2. Bridge-V's three-part plan

| Where a guest value can live | Which registers | For how long |
|---|---|---|
| **Pinned** host register | R12 = `sp` (x2), R13 = `ra` (x1), R14 = `a0` (x10), R15 = `a5` (x15) | **forever**: across every block and every chained jump |
| **Pool** register | RAX, RCX, RDX, RSI, RDI, R8 (6 registers) | within one block only |
| **Memory home** | `CpuState.x[g]`, at `[rbp + 8*g − 128]` | the "official" copy for every non-pinned register |

Plus temporary values that aren't guest registers (like an address `a4 + 8`) can go to **spill slots**: `CpuState.spill[0..64]`.

### 2.1 Pinned registers

The **block-boundary rule** (CLAUDE.md §8.3): at the start and end of every block, the pinned guest registers live **only** in R12–R15, and all other guest registers are up to date in `CpuState`. Pool registers hold nothing across blocks.

Why pin at all? A value that crosses from one block to the next (a loop counter, the stack pointer, the return address) would otherwise be stored at the end of every block and loaded at the start of the next. Pinned registers skip both. Why R12–R15? They are **callee-saved** in the System V ABI (file 03), so they also survive calls into Rust helpers without any save/restore code.

Why these four guest registers? `sp` and `ra` are used by almost every function call and return; `a0` carries arguments and return values; GCC uses `a5` as its favourite temporary. The project measured alternatives (decision D42): pinning the four most-*used* registers on CoreMark (x15, x14, x13, x10) was only +2.6%, within noise, and hurt other programs. What matters is values that live *across* blocks, not raw use counts. `--pin` lets you choose others.

Pinning alone (`--regalloc=pinned`) took CoreMark from 7,024 to 10,168 iterations/s (+45%).

### 2.2 The pool

Inside a block, any guest register or temporary can be held in one of the 6 pool registers. These are **caller-saved**, so they are lost at a helper call, which is fine because nothing lives in them across blocks anyway.

R10 and R11 are never allocated: the code generator uses them as scratch (address calculations, the TLB probe, copies).

---

## 3. Linear scan, simplified for a basic block

**Linear scan** (Poletto & Sarkar, 1999) is a fast register-allocation algorithm used by JITs (Java HotSpot's client compiler, V8, and others). The classic version:
1. compute each value's **live interval**: from where it is defined to its last use
2. walk the intervals in order of start position
3. give each new interval a free register; if none is free, **spill** the interval that ends furthest away

For a translation block, which is straight-line code, this collapses into **one forward walk over the ops, done at the same time as emitting code**. Bridge-V's version (design decision D36):

```
for each op, in order:                             (pos = op index)
    for each operand v:  reg = get(v)              // already in a register? use it
                                                   // else take a free pool register and fill v
    result: d = def(dst)                           // pick a register (prefer reusing an operand's)
    emit the x86 instruction(s)
    release values whose last use was this op      // their registers become free
```

**The information it needs** comes from `Liveness` ([`src/ir/liveness.rs`](../../src/ir/liveness.rs)): for every value, the sorted list of positions where it is used. From that the allocator can ask "is `v` used again after this op?" (`live_after`) and "when is `v` used next?" (`use_from`).

### 3.1 Choosing a victim: furthest next use

When `alloc()` needs a register and the pool is full, it evicts the value whose **next use is furthest away**:

```rust
let victim = pool.iter()
    .filter(|r| !avoid.contains(r))
    .max_by_key(|r| use_from(owner[r], pos).unwrap_or(u32::MAX))   // never used again = best victim
```

This is the same idea as **Belady's algorithm** for caches ("evict what you'll need latest"). The current op's own operands have a next use equal to the current position, so they are never chosen.

### 3.2 What eviction costs depends on the value

Each value remembers a **backing** (`enum Back`): where it can be recovered without a register.

| Backing | Meaning | Cost to evict |
|---|---|---|
| `Home(g)` | the value is also in guest register `g`'s memory home | **free**: just forget the register, re-fill from home later |
| `Const(c)` | the value is a known constant | **free**: rebuild it with `mov r, c` later |
| `Slot(k)` | already saved in spill slot `k` | **free** |
| `None` | only in this register | must be stored: to its home if it is a dirty guest register, else to a new spill slot |

So a value loaded from `CpuState` and never modified ("clean") can be dropped at no cost. This is why the allocator doesn't mind evicting guest register values.

**Constants are lazy.** `Op::Const` emits nothing. The allocator records `Back::Const(c)`. Only if an instruction actually needs the constant in a register is it materialized (`mov r, c`). Stores of small constants use an immediate operand instead, and branches against 0 use `test`.

---

## 4. Lazy write-back

This is the allocator's most important optimization (the `lazy` flag, on at `--regalloc=linear`).

When the IR says `WriteReg { g, src }` for a non-pinned register:
- **eager** (`--regalloc=pinned`): store to `CpuState.x[g]` immediately
- **lazy** (default): just mark the value **dirty** for `g` (`dirty[g] = Some(v)`). No code is emitted.

The dirty value is stored home only when it must be:
- at a block **exit** (`write_back_all()` before every terminator), because of the block-boundary rule
- before a **helper call** (the helper reads `CpuState`)
- when the value's register is **evicted**

So if a block writes `a4` five times, only the last value is stored, and only once. This complements the `dead_writes` pass, which can't remove writes that come before a load or store.

Going from eager to lazy (with the optimizer passes) took CoreMark from 10,168 to 13,498 iterations/s (+33%).

---

## 5. State maps: precise faults at zero cost

Lazy write-back creates a problem. Suppose a block does:

```
addi a4, a4, 8        ; a4 is now dirty, only in RAX
ld   a5, 0(a3)        ; this load faults!
```

At the fault, the guest must see `a4` already incremented (the `addi` completed), but `CpuState.x[14]` still holds the old value; the new one is only in RAX.

The simple fix would be to store every dirty register before every load or store. That would add lots of stores to the fast path.

Bridge-V's fix (decision D37): before emitting each load or store, the back end calls `dirty_state()`, which returns a list saying **where every dirty guest register lives right now**:

```rust
pub enum DLoc {
    Reg(Reg),      // in this host register
    Slot(u16),     // in spill slot k
    Const(u64),    // it is this constant
    Home(u8),      // equal to another guest register's home
}
```

This list is stored in the TB's `FaultSite` record (Rust memory, not generated code). If the load really faults:
1. the SIGSEGV handler copies all 16 host registers into `cpu.fault_regs`
2. the dispatcher finds the fault site for the faulting instruction address
3. `apply_site()` ([`dispatch.rs`](../../src/jit/dispatch.rs)) writes each dirty guest register home from the saved host register, slot or constant, and sets `pc` to the faulting instruction

```rust
for &(g, loc) in &site.dirty {
    let v = match loc {
        DLoc::Reg(r)   => cpu.fault_regs[r.num()],
        DLoc::Slot(k)  => cpu.spill[k],
        DLoc::Const(c) => c,
        DLoc::Home(h)  => cpu.x[h],
    };
    cpu.set_x(g, v);
}
cpu.pc = site.pc;
```

The fast path pays **nothing**: no extra instructions at all. The cost is only paid when a fault actually happens, which is rare. (Pinned registers need no map: `exit_jit` always stores R12–R15 home.)

---

## 6. Helper calls: `sync_for_call()`

Before calling `helper_interp_one` (for an `Interp` op), the allocator must prepare for a call that:
- destroys all pool registers (they are caller-saved)
- may read or write **any** guest register in `CpuState`

So `sync_for_call()`:
1. writes back every dirty guest register (`write_back_all`)
2. stores the pinned registers R12–R15 to their homes (the helper reads `CpuState`)
3. moves every value still needed after the call into a spill slot
4. forgets all register contents and all "this value equals home(g)" facts (the helper might change the homes)

After the call, `after_call()` reloads R12–R15 from `CpuState`, because the helper may have changed them (for example, a CSR instruction writing `a0`).

This is costly, which is why common instructions are translated natively and only rare ones go through the helper.

---

## 7. Fixed-register operations

Some x86 instructions only work with specific registers (`constraint()` in `liveness.rs`):

| Constraint | Instructions | What the back end does |
|---|---|---|
| `RaxRdx` | `mulh`, `mulhu`, `mulhsu`, all divisions and remainders | `vacate` RAX and RDX (move their values elsewhere or evict), copy operands to R10/R11, compute, bind the result to RAX or RDX |
| `Rcx` | variable shifts **without** BMI2 (the count must be in CL) | vacate RCX, put the count there |
| `Call` | `Interp` (helper call) | full sync (section 6) |

With BMI2 (`shlx`, `shrx`, `sarx`), shifts can use any registers, so this constraint disappears on most modern CPUs.

---

## 8. Walking through the example

The loop from file 07, with `a5` (x15) pinned in R15 and `a4` (x14) not pinned:

| pos | IR op | Allocator action | Code emitted |
|---|---|---|---|
| 1 | `v0 = x14` | `read_reg`: `v0` backed by `Home(14)`. Nothing emitted yet. | — |
| 2 | `v1 = load [v0+0]` | `get(v0)`: not in a register → take RAX, fill from home. `def(v1)`: RCX. Record fault site (no dirty registers). | `mov rax, [rbp-0x10]` then `mov rcx, [rbx+rax]` |
| 3 | `x15 = v1` | pinned: move `v1` into R15 | `mov r15, rcx` |
| 4 | `v3 = add v0, 8` | `v0`'s last use: its register RAX can hold the result | `add rax, 8` |
| 5 | `x14 = v3` | lazy: mark `v3` dirty for x14. Nothing emitted. | — |
| 6 | `v5 = const 0` | `Back::Const(0)`. Nothing emitted. | — |
| 7 | `br.Ne v1, v5` | `write_back_all`: store dirty x14. Constant 0 → `test`. | `mov [rbp-0x10], rax` then `test rcx, rcx`, `jne`, `jmp` |

Result: one fill, one write-back, no spills. `--stats` reports these counts summed over all TBs (`fills`, `spills`, `write-backs`, `moves`).

---

## 9. An idea that was tried and rejected

**Loop-resident registers** (decision D45): for a block that loops to itself, keep its busiest guest registers in pool registers across iterations, loading them once before the loop and storing them only on the exits. It doubled the speed of a tiny test loop (3,170 → 6,225 MIPS), but moved neither CoreMark (−0.4%) nor Dhrystone (−1.4%). It was reverted: the project's rule is that a change stays only if it wins on the real benchmarks. This is a good story for an interview: *measure, and be willing to throw work away.*

---

## Check yourself

1. How many x86 registers are available for guest values, and where do the others go?
2. What is the block-boundary rule? Why does it make chaining simple?
3. Why are the pinned registers R12–R15 and not RAX–RDX?
4. Describe linear scan in three sentences. Why is it a single walk for a basic block?
5. Which value does the allocator evict, and why is evicting a clean value free?
6. What does lazy write-back do? Give an example where it saves stores.
7. A load faults while `a4` is dirty in RAX. How does the guest end up seeing the right `a4`?
8. What must happen before and after a helper call, and why?
9. Why do division and multiply-high need special handling?
