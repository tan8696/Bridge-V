# 06 · The interpreter: the simple, always-correct engine

## What you will learn

- why a translator needs an interpreter at all
- what a **pre-decoded** interpreter is, and why it is faster than a naive one
- the algorithm: block building, block cache, `exec_block`, `step`
- how the interpreter counts instructions and reports exits
- how the JIT reuses the interpreter's code (for decoding, for rare instructions, and for testing)

All the code is in one file: [`src/interp/mod.rs`](../../src/interp/mod.rs) (about 800 lines).

---

## 1. Three jobs

The interpreter is the slowest engine (about 30× slower than the JIT), but the project depends on it for three jobs:

1. **The golden model.** It is written to be obviously correct: each RISC-V instruction is a few lines of plain Rust. Everything the JIT does is checked against it (lockstep mode, fuzzers, file 17).
2. **The speed baseline.** "The JIT is 29.5× faster than the interpreter" needs an interpreter to compare against.
3. **The fallback.** Rare or complicated instructions (CSR access, atomics, some FP, privileged instructions) are not translated to x86. The generated code calls `helper_interp_one`, which runs the interpreter's `step()` on that single instruction (design decision D14). So every instruction the interpreter supports also works under the JIT, automatically.

---

## 2. Naive vs pre-decoded interpreters

A **naive** interpreter decodes every instruction every time it runs:

```
loop {
    raw = fetch(pc)
    inst = decode(raw)     ← repeated for every execution, even in a loop run a million times
    execute(inst)
}
```

Bridge-V's interpreter is **pre-decoded**: it decodes a whole basic block once, keeps the decoded `Vec<Decoded>` in a hash map keyed by `pc`, and reuses it every time execution reaches that `pc` again. Decoding cost is paid once per block, not once per execution.

---

## 3. The algorithm

### 3.1 The run loop

```rust
pub fn run(&mut self, cpu, mem, env, max_insns) -> Stop {
    let limit = cpu.icount + max_insns;
    loop {
        if cpu.icount >= limit { return Stop::Limit; }         // time slice used up
        if let Some(v) = tohost_written(mem, env) {            // bare-metal tests only
            return Stop::Tohost(v);
        }
        deliver_interrupt(cpu, env);                            // system mode: take pending IRQ
        let block = self.block(cpu, mem);                       // decoded block at cpu.pc
        let exit = exec_block(cpu, mem, &block.insns, block.fetch_fault, env.trace);
        if exit == BlockExit::Flush { self.flush(); }           // FENCE.I: forget decoded code
        if !mem.smc_pages.is_empty() {                           // our code was overwritten
            self.flush(); forget_smc_pages(cpu, mem);
        }
        if let Err(stop) = deliver(exit, env, cpu) { return stop; }
    }
}
```

Interrupts, time limits and the `tohost` check happen only **between blocks**. That is enough: blocks are short (at most 64 instructions in the interpreter), so the delay is tiny.

### 3.2 Getting a block: `block()`

```rust
fn block(&mut self, cpu, mem) -> Rc<Block> {
    let key = (cpu.pc, fetch_mmu_index_if_softmmu);
    if let Some(b) = self.cache.get(&key) { return b.clone(); }     // cache hit
    let b = Rc::new(build_block(pc, mem));                           // decode it
    self.cache.insert(key, b.clone());
    mark_block_code(cpu, mem, pc, &b.insns);   // remember "this page holds code" (file 13)
    b
}
```

`Rc` is a reference-counted pointer, so the cache and the running code can share one decoded block cheaply.

### 3.3 Building a block: `build_block_by()`

```rust
fn build_block_by(pc, max, no_straddle, fetch) -> Block {
    let mut insns = Vec::new();
    let mut a = pc;
    loop {
        let lo = match fetch(a) { Ok(v) => v, Err(e) => return Block { insns, fetch_fault: Some(e) } };
        let d = decode_parts(lo, || fetch(a + 2))?;   // 16- or 32-bit instruction
        a += d.len;
        let end = d.inst.ends_block();                // branch, jump, ecall, csr, fence.i, ...
        insns.push(d);
        if end || insns.len() >= max || page_of(a) != page_of(pc) {
            return Block { insns, fetch_fault: None };
        }
    }
}
```

A block stops at the first of:
- an instruction that ends a block (`Inst::ends_block()`: branches, `jal`, `jalr`, `ecall`, `ebreak`, `mret`, `sret`, `wfi`, `fence.i`, `sfence.vma`, CSR instructions, illegal instructions)
- the instruction limit
- the end of the 4 KiB page (a block never spans two pages, because the next page might be unmapped or, in system mode, mapped differently)
- a **fetch fault**: if the next instruction can't be read, the block keeps the instructions before it and records the fault. The fault is raised only if execution actually reaches that point.

CSR instructions end a block because they can change things that affect how later code behaves (privilege, interrupts, the MMU, the FP rounding mode).

### 3.4 Running a block: `exec_block()`

```rust
pub fn exec_block(cpu, mem, insns, fetch_fault, trace) -> BlockExit {
    let mut pc = cpu.pc;
    for d in insns {
        match step(cpu, mem, d, pc) {
            Flow::Next => {
                pc += d.len;  cpu.icount += 1;
                if !mem.smc_pages.is_empty() { cpu.pc = pc; return BlockExit::Continue; } // SMC
            }
            Flow::Jump(target) => { cpu.icount += 1; cpu.pc = target; return BlockExit::Continue; }
            Flow::Ecall        => { cpu.pc = pc; return BlockExit::Ecall; }      // not retired yet
            Flow::Trap(e)      => { cpu.pc = pc; return BlockExit::Trap(e); }    // not retired
            Flow::Flush        => { cpu.icount += 1; cpu.pc = pc + d.len; return BlockExit::Flush; }
            Flow::Wfi          => { cpu.icount += 1; cpu.pc = pc + d.len; return BlockExit::Wfi; }
        }
    }
    cpu.pc = pc;
    match fetch_fault { Some(e) => BlockExit::Trap(e), None => BlockExit::Continue }
}
```

Note the careful bookkeeping:
- `icount` (retired instructions) goes up only for instructions that **completed**. A trapping instruction did not complete, so `pc` stays pointing at it. This is what "precise exceptions" means (file 12).
- `pc` is kept in a local variable and written to `cpu.pc` only when leaving. That is a small speed trick.
- `exec_block` is **shared**: the JIT's lockstep checker and its fault-recovery code also call it.

### 3.5 Executing one instruction: `step()`

`step()` is the **reference definition of every RISC-V instruction** in Bridge-V. A few representative arms:

```rust
pub fn step(cpu, mem, d, pc) -> Flow {
    let next_pc = pc + d.len;
    match d.inst {
        Inst::Lui { rd, imm }    => cpu.set_x(rd, imm as u64),
        Inst::Auipc { rd, imm }  => cpu.set_x(rd, pc + imm),
        Inst::Jal { rd, imm }    => { cpu.set_x(rd, next_pc); return Flow::Jump(pc + imm); }
        Inst::Jalr { rd, rs1, imm } => {
            let target = (cpu.x[rs1] + imm) & !1;   // read rs1 BEFORE writing rd
            cpu.set_x(rd, next_pc);
            return Flow::Jump(target);
        }
        Inst::Branch { op, rs1, rs2, imm } => {
            let (a, b) = (cpu.x[rs1], cpu.x[rs2]);
            let taken = match op { Beq => a == b, Blt => (a as i64) < (b as i64), Bltu => a < b, .. };
            if taken { return Flow::Jump(pc + imm); }
        }
        Inst::Load { op, rd, rs1, imm } => {
            let addr = cpu.x[rs1] + imm;
            match mmu::load(cpu, mem, addr, size) {             // checked access
                Ok(v)  => cpu.set_x(rd, extend(v)),
                Err(e) => return Flow::Trap(e),                  // e.g. load access fault
            }
        }
        Inst::Op { op, rd, rs1, rs2 } => cpu.set_x(rd, alu(op, cpu.x[rs1], cpu.x[rs2])),
        Inst::Ecall  => return Flow::Ecall,
        Inst::FenceI => return Flow::Flush,
        Inst::Csr { .. } => { /* read old value, maybe write new one, illegal if not allowed */ }
        Inst::Amo { .. } => exec_amo(...),
        Inst::FLoad {..} | Inst::Fp {..} | .. => fp::exec(cpu, mem, &d.inst, d.raw)?,
        Inst::Illegal(_) => return Flow::Trap(Exception::illegal(d.raw)),
        ..
    }
    Flow::Next
}
```

`cpu.set_x(rd, v)` ignores writes to x0, which keeps "x0 is always zero" true without any special case elsewhere.

`alu()` and `aluw()` hold the arithmetic rules, including the tricky cases (file 02, §8). They are short enough to read in full, and there are unit tests for the edge cases at the bottom of the file (`division_edge_cases`, `mul_high_variants`, `w_ops_sign_extend`).

### 3.6 Memory accesses go through `mmu::load` / `mmu::store`

Even in user mode, the interpreter never touches guest memory with a raw pointer. `mmu::load()` ([`src/mem/mmu.rs`](../../src/mem/mmu.rs)) checks:
- user mode, direct memory: is the page mapped and readable (`DirectMem::check`)?
- system mode: translate the virtual address through the software TLB / page tables first (file 11).

Any failure becomes an `Exception` (access fault or page fault), never a host crash. The interpreter is **memory-safe against any guest program**, however broken.

### 3.7 Atomics: `exec_amo()`

- `lr` loads a value and records a **reservation** (`res_addr`, `res_val`, `res_valid`).
- `sc` succeeds only if the reservation is still valid for that address. It writes 0 to `rd` on success, 1 on failure. Any `sc` clears the reservation.
- `amoadd`, `amoswap`, … load the old value, compute, store the new value, and return the old value.

Because only one guest thread runs at a time (file 15), these plain read-modify-writes are atomic.

---

## 4. The `Engine` trait and `Stop`

Every engine implements:

```rust
pub trait Engine {
    fn run(&mut self, cpu, mem, env, max_insns) -> Stop;  // run until something needs attention
    fn flush(&mut self);                                   // guest code changed: drop caches
    fn fence_i(&mut self) { self.flush(); }
    fn stats(&self) -> String { String::new() }
}

pub enum Stop {
    Ecall,             // user mode: a system call at cpu.pc
    Fault(Exception),  // user mode: a fatal exception
    Limit,             // the instruction budget ran out
    Tohost(u64),       // bare-metal test finished
    Diverged,          // lockstep found a JIT bug
    Wfi,               // system mode: the CPU is waiting for an interrupt
}
```

Because all three engines speak this same interface, the user-mode loop, the system-mode machine loop and the tests can use any engine.

---

## 5. How fast is it?

On CoreMark the interpreter runs about 450–520 iterations per second, roughly **180 million guest instructions per second** (Phase 5). The fully optimized JIT runs about 4.8 billion per second.

Why is the interpreter slower?
- **Dispatch overhead.** Every instruction goes through a `match` (an indirect jump that the CPU's branch predictor often guesses wrong), plus bounds and permission checks.
- **Registers live in memory.** Every guest register read is a load from `cpu.x[i]`; every write is a store.
- **No optimization across instructions.** `lui` + `addi` are two separate steps instead of one constant.

The JIT removes all three. The interpreter's advantages are simplicity and certainty, which is exactly what a golden model needs.

---

## Check yourself

1. Give three reasons the project keeps an interpreter.
2. What does "pre-decoded" mean, and what does it save?
3. List the four conditions that end a block.
4. Why is `pc` not advanced past an instruction that traps?
5. Why does `step()` read `rs1` before writing `rd` for `jalr`?
6. How does the interpreter stay safe against a guest that dereferences a garbage pointer?
7. What is the `Engine` trait, and why is it useful?
