# 10 · Block chaining and the jump cache: the biggest speed-up

## What you will learn

- why returning to the dispatcher after every block is slow
- how **block chaining** patches a block's exit jump to go straight to the next block
- why the patched field must be **4-byte aligned**, and how padding achieves it
- how links are recorded and **undone** (unlinking)
- the **budget** mechanism that keeps chained code interruptible, and how instructions are counted
- the **jump cache**, which handles indirect jumps (function returns) that can't be patched
- the extra rules in system mode

Files: [`src/jit/chain.rs`](../../src/jit/chain.rs), `link_last()` and `exec()` in [`src/jit/dispatch.rs`](../../src/jit/dispatch.rs), `exit_direct()`, `exit_cond()`, `jump_cache()` in [`src/backend/x86/lower_ir.rs`](../../src/backend/x86/lower_ir.rs).

---

## 1. The problem: the dispatcher is a bottleneck

Without chaining, every translated block ends by returning to the Rust dispatcher:

```
TB A runs → exit_jit → Rust: look up pc in a hash map → enter_jit → TB B runs → exit_jit → ...
```

Each round trip costs a lot:
- `exit_jit` + `enter_jit`: saving and restoring 6 registers, writing pinned registers back and reloading them
- the Rust dispatcher code and a hash-map lookup
- worst of all, an **indirect jump** (`jmp rsi` in `enter_jit`) whose target changes every time. The CPU's branch predictor keeps guessing wrong, costing roughly 15–20 cycles per miss.

A typical block is only 3–6 guest instructions, so this overhead dominates. Measured on CoreMark (Phase 5):

| | CoreMark it/s | dispatcher entries per million guest instructions |
|---|---:|---:|
| naive JIT, no chaining (`--no-chain`) | 791 | 180,762 |
| with chaining + jump cache | **7,024** (8.9× faster) | **10** |

Chaining alone gave almost a 9× speed-up. It is the single most important optimization in the project.

---

## 2. Exits and stubs

Every translated block ends with one or two **direct exits** (exits to a guest pc known at translation time):

```
    cmp   a, b
    jcc   rel32   ── slot 1 (branch taken)  → initially: stub_1 ;  after linking: TB(taken_pc)
    jmp   rel32   ── slot 0 (fall-through)  → initially: stub_0 ;  after linking: TB(fall_pc)
    ... cold stubs ...
stub_1:
    mov   qword [rbp + pc], taken_pc
    mov   eax, (tb_id << 2) | 1
    jmp   exit_jit
stub_0:
    mov   qword [rbp + pc], fall_pc
    mov   eax, (tb_id << 2) | 0
    jmp   exit_jit
```

- A conditional branch uses both slots. A plain `jal` or fall-through uses only slot 0.
- Each exit initially jumps to its own **stub**, which tells the dispatcher "I wanted to go to `pc`, via slot k of TB id".
- The generator records, for each slot, the offset of the jump's **rel32 field**, its stub's offset and its target pc (`ExitInfo`). The dispatcher stores them in the `TranslationBlock` as `ExitSlot { patch_at, stub, target_pc, linked }`.

---

## 3. Linking: the algorithm

**Step 1: remember the exit.** When a block comes back through slot 0 or 1 with `exit_reason = NONE`, `exec()` remembers it:
```rust
self.last_exit = (slot < 2 && reason == exit::NONE)
    .then_some((tb_id, slot, self.cache.generation));
```

**Step 2: link on the next dispatch.** `select()` finds or translates the TB for the new `pc`, then calls `link_last(id)`:

```rust
fn link_last(&mut self, id: u32) {
    let Some((from, slot, generation)) = self.last_exit.take() else { return };
    if generation != self.cache.generation || !self.opts.chain { return; }  // flushed? disabled?
    let (f, t) = (self.cache.get(from), self.cache.get(id));
    let paged = /* system mode (softmmu with real page tables) */;
    if f.key.flags != t.key.flags                                     // different privilege/MMU state
        || (paged && f.guest_pc >> 12 != t.guest_pc >> 12) {          // different page (system mode)
        return;
    }
    chain::link(&mut self.cache, &mut self.cm, from, slot, id);
}
```

**Step 3: patch.** `chain::link()`:
```rust
pub fn link(cache, cm, from, slot, to) -> bool {
    let ex = cache.get(from).exits[slot]?;
    if !from.valid || !to.valid || ex.linked == Some(to) { return false; }
    cm.patch_rel32(ex.patch_at, to_host);          // rewrite the 4-byte jump offset
    from.exits[slot].linked = Some(to);
    to.incoming.push((from, slot));                // remember who jumps into `to`
    true
}
```

From now on, when TB `from` takes that exit, the CPU jumps **directly** into TB `to`'s first instruction. No Rust, no hash lookup, and a direct jump that the branch predictor handles perfectly.

### 3.1 A real example (from `links.txt`)

The self-loop from file 07 at host offset `+3b0`:
```
tb_10990 slot 1 @+3d2: 0f 85 d8 ff ff ff -> tb_10990 @+3b0 (stub was +403)
tb_10990 slot 0 @+3db: e9 50 00 00 00    -> tb_10996 @+430 (stub was +418)
```
- The first time the loop's branch is taken, execution leaves through `stub_1`. The dispatcher looks up TB `0x10990`, which is the loop itself, and rewrites the rel32 at `+3d4` to `0x3b0 − (0x3d2 + 6) = −0x28` = `d8 ff ff ff`. Now the `jne` jumps straight back to the loop's own start.
- When the loop finally ends, the fall-through exit goes to its stub once and is then patched to the next block: `0x430 − (0x3db + 5) = 0x50`.

After that, the whole loop runs natively forever (or until the budget runs out; see section 5).

---

## 4. Why the rel32 field must be 4-byte aligned

`patch_rel32` rewrites 4 bytes. On x86, a **4-byte store to a 4-byte-aligned address is atomic**: another CPU core fetching those bytes sees either the old value or the new one, never half of each. If the field straddled an alignment boundary, a concurrently running thread could fetch a torn, garbage jump target. (Bridge-V's guest threads don't currently run generated code in parallel, but the design keeps this safe for the future: decision D7.)

**How the alignment is achieved.** Before each exit jump, the generator pads with NOPs:

```rust
fn exit_direct(&mut self, target: u64) {   // slot 0: jmp rel32 (opcode E9, 1 byte)
    self.a.align(4, 1);                     // pad so that (here + 1) % 4 == 0
    let at = self.a.jmp(label);             // the rel32 field starts right after E9
}
fn exit_cond(&mut self, cond, target) {     // slot 1: jcc rel32 (opcode 0F 8x, 2 bytes)
    self.a.align(4, 2);                     // pad so that (here + 2) % 4 == 0
    let at = self.a.jcc(cond, label);
}
```

`align(4, skew)` adds multi-byte NOPs until `here + skew` is a multiple of 4. The `skew` is the opcode length, so the rel32 field right after the opcode lands on a multiple of 4. In the example above, a 2-byte NOP (`66 90`) came before the `jne` and a 3-byte NOP (`0f 1f 00`) before the `jmp`. The dispatcher also asserts `patch_at % 4 == 0` for every exit it records.

---

## 5. The budget: keeping chained code interruptible

Once blocks are chained into a loop, the CPU could run them forever without ever returning to Rust. But Bridge-V must regain control regularly: to deliver timer interrupts, to let other guest threads run, and to enforce `--max-insns`.

**The solution (decision D12):** every TB starts with
```
sub  r9, n          ; n = number of guest instructions in this TB
jl   budget_stub    ; if the budget went negative: leave, having executed nothing
```

The dispatcher gives each entry a budget (by default the 100,000-instruction slice). Every block, chained or not, charges its instructions up front. When the budget runs out, the next block's prologue exits to its `budget_stub`, which refunds the charge, stores `pc` = this TB's start, sets `exit_reason = BUDGET`, and returns. Only 2 instructions per block, and even an infinite guest loop is interrupted within one slice.

### 5.1 Counting instructions for free (decision D30)

Generated code never updates `icount` (the retired-instruction counter). Instead:
- `exec()` sets `cpu.budget = cpu.budget_ref = budget` before entering.
- Each TB charges `n` up front.
- After returning, `cpu.icount += budget_ref − budget`.

Paths that retire *fewer* than `n` instructions give the rest back:
- `ecall` exits refund 1 (the `ecall` itself hasn't run yet)
- a fault at instruction `k` refunds `n − k`
- a helper call at index `k` refunds `n − k` before the call and charges it again after

So `icount` is exact at every exit, and the hot path does one subtraction per block. (The IR back end keeps the budget in register R9 rather than memory; see file 07, §5.1.)

### 5.2 The first TB always runs

```rust
let budget = (limit - cpu.icount).min(self.opts.slice).max(n);
```

The `.max(n)` guarantees the first block always has enough budget to run completely, so a tiny remaining budget can never stall progress.

---

## 6. Unlinking

Sometimes a TB must be destroyed, for example when the guest overwrites the code it was translated from (file 13). Any block that jumps directly into it must be redirected first, or it would jump into stale code. That's what the `incoming` list is for:

```rust
pub fn unlink_incoming(cache, cm, to) -> usize {
    for (from, slot) in take(&mut cache.get_mut(to).incoming) {
        let ex = cache.get_mut(from).exits[slot];
        ex.linked = None;
        cm.patch_rel32(ex.patch_at, ex.stub);   // point the jump back at its own stub
    }
}
```

After unlinking, `cache.invalidate(to)` removes the TB from the lookup map and marks it invalid. Its bytes stay in the buffer (bump allocation never frees) until the next full flush.

---

## 7. The jump cache: handling indirect jumps

Chaining only works for **direct** exits, whose target is known at translation time. An **indirect** jump (`jalr`) goes to an address computed at run time. The most common one is a function return: `ret` = `jalr x0, 0(ra)` jumps to wherever `ra` points, which is different for every caller. It can't be patched.

Returning to the dispatcher for every function return would be slow. So Bridge-V uses a **jump cache** (decisions D8, D31): a small hash table inside `CpuState` that generated code can search directly.

```rust
pub jmp_cache: [JcEntry; 4096],        // at CpuState offset 0x300, 64 KiB
pub struct JcEntry { pub pc: u64, pub host: u64 }   // guest pc → host address of its TB
```

### 7.1 The lookup, inline in generated code

With the target guest pc in RAX (`jump_cache()` in `lower_ir.rs`):

```
    mov  r10, rax
    shl  r10, 3
    and  r10d, 0xFFF0                ; byte offset of slot (pc >> 1) & 4095   (16-byte entries)
    cmp  rax, [rbp + r10 + jc_off]   ; entry.pc == target?
    jne  miss_stub                   ; no → exit with reason LOOKUP
    jmp  qword [rbp + r10 + jc_off + 8]   ; yes → jump straight into the target TB
```

Why `(pc >> 1) & 4095`? Instructions are at least 2 bytes apart, so bit 0 of a pc is always 0. Dropping it spreads nearby addresses over more slots. Multiplying by the 16-byte entry size and masking in one step gives `(pc << 3) & 0xFFF0`.

**A hit** is about 8 instructions and one indirect jump, with no Rust involved. **A miss** exits with `LOOKUP`; the dispatcher finds or translates the target and, in `exec()`, **fills the entry** for the TB it is about to enter, so the next return to the same place hits:

```rust
let e = &mut cpu.jmp_cache[jc_index(guest_pc)];
if e.pc != pc || e.host != host { *e = JcEntry { pc, host }; }
```

On the call-heavy `fib` test program, the hit rate is over 90%.

### 7.2 Keeping the jump cache valid

The jump cache holds raw host addresses. If code is flushed or invalidated, those addresses become stale and jumping to them would be a disaster. Bridge-V uses a **tag** instead of clearing the table on every change (D31):

- `cpu.jc_tag = (jit_id << 48) | version`
- every flush or invalidation bumps `version` (a cheap counter increment)
- before entering generated code, `exec()` compares `cpu.jc_tag` with the JIT's current tag. If they differ, it clears the whole table once and stores the new tag.

This "lazy clearing" also handles tests that create several JITs in one process (the `jit_id` part).

---

## 8. Extra rules in system mode

In system mode the guest OS can change the virtual→physical mapping (page tables, `satp`) without touching the code, so a translation keyed by virtual address could become wrong. Bridge-V adds rules (D48, D51):

- **Link only within one virtual page, and only between TBs with the same flags** (privilege level, MMU indices). A cross-page transfer must go through a check that re-validates the mapping.
- **Cross-page direct exits use the jump cache inline** instead of returning to the dispatcher (`jc_cross_page`), because the dispatcher fills jump-cache entries only after re-checking the mapping.
- **Jump-cache entries carry the TB flags**: the stored value is `pc ^ (flags << 56)`, and generated code XORs its own flags into the pc before comparing. An entry filled at one privilege level can never hit at another, so a privilege change needs no flush.
- A full TLB flush (a `satp` write, `sfence.vma` without an address) bumps `cpu.jc_gen`, which resets the jump cache. `sfence.vma` with an address clears only that page's entries.

For the Linux boot, these rules cut dispatcher entries from 6.5 million to 0.7 million and time to shell from 1.80 s to 1.30 s (Phase 9).

---

## 9. Ideas analysed and not built

- **A return-address stack** (predict `ret` targets with a shadow stack). Returns already hit the jump cache over 90% of the time, and a software stack would still end in an indirect jump. The real win would need host `call`/`ret` instructions, which conflicts with the fixed-RSP block-boundary rule.
- **Superblocks / traces** (translate hot paths across block boundaries). The closest cheap experiment (loop-resident registers, D45) didn't move the benchmarks.

Both are documented in [`phase-10-not-pursued.md`](../phase-reports/phase-10-not-pursued.md). Being able to say *why* you didn't build something is valuable in interviews.

---

## Check yourself

1. Why is returning to the dispatcher after every block so expensive? Give the measured numbers.
2. What is a stub, and what does it contain?
3. Walk through what happens the first and the second time a loop's branch is taken.
4. Why must the rel32 field be 4-byte aligned? How does `align(4, skew)` achieve it?
5. What is the `incoming` list for?
6. How does the budget prologue keep an infinite chained loop interruptible? How is `icount` computed?
7. Why can't `ret` be chained? How does the jump cache handle it, and what happens on a miss?
8. How does Bridge-V avoid jumping to stale host addresses through the jump cache?
9. Why does system mode link only within a single page?
