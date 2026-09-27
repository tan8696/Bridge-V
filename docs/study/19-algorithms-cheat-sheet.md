# 19 · Algorithms cheat sheet

Every algorithm in Bridge-V, in short pseudocode, in the order data flows through the system. Use this for revision after reading files 05–16. Each entry names the function and file, and the study file with the full explanation.

Notation: `n` = number of guest instructions in a block, `ops` = number of IR ops.

---

## A. Decoding

### A1. Instruction length — `insn_len()` · `isa/decode.rs` · file 02
```
lo = first 16 bits
if lo & 0b11 != 0b11:           length 2 (compressed)
elif lo & 0b11100 != 0b11100:   length 4
else:                           unsupported (illegal)
```

### A2. Field and immediate extraction — `bits()`, `imm_i()`, `imm_b()` · `isa/decode.rs`
```
bits(x, hi, lo) = (x >> lo) & ((1 << (hi-lo+1)) - 1)
imm_i(x) = (x as i32) >> 20                     // arithmetic shift = sign extension
imm_b(x) = sext( x[31]<<12 | x[7]<<11 | x[30:25]<<5 | x[11:8]<<1 , 13 bits)
sext(v, n) = ((v << (64-n)) as i64) >> (64-n)
```

### A3. Decode — `decode()` / `rvc::expand()` · file 02
```
match opcode = x[6:0]:  0x37 LUI, 0x17 AUIPC, 0x6F JAL, 0x67 JALR, 0x63 BRANCH (by funct3),
                        0x03 LOAD, 0x23 STORE, 0x13/0x1B OP-IMM(-32), 0x33/0x3B OP(-32),
                        0x0F FENCE, 0x73 SYSTEM, 0x2F AMO, FP opcodes …
anything reserved → Inst::Illegal(raw)          // never panics
compressed: match (quadrant = x[1:0], funct3 = x[15:13]) → the equivalent 32-bit Inst
```

### A4. Block formation — `build_block_by()` · `interp/mod.rs` · file 06
```
insns = []; a = pc
loop:
    half = fetch16(a)           or return (insns, fetch_fault = e)
    d = decode (fetch the 2nd half only for 32-bit instructions)
    a += d.len; insns.push(d)
    if d.ends_block() or len(insns) ≥ max or page(a) != page(pc): return insns
```
Cost O(n).

---

## B. Execution loops

### B1. Interpreter — `Interp::run()` · file 06
```
loop:
    if icount ≥ limit: return Limit
    take a pending interrupt (system mode)
    block = cache[(pc, fetch_idx)] or build_block and cache it (and mark its page as code)
    exit = exec_block(block)         // for each insn: step(); icount += 1 if retired
    if FENCE.I or code pages were written: flush the cache
    deliver(exit) → continue, trap into the guest, or return a Stop
```

### B2. JIT dispatcher — `Jit::run()` · file 05
```
loop:
    if icount ≥ limit: return Limit
    take a pending interrupt (system mode)
    id = select(cpu, mem)            // drain SMC; cold? (B4) else find/translate TB; link last exit
    budget = max(min(limit − icount, slice), len(TB))
    exit = exec(id, budget)          // run native code; decode exit_reason
    deliver(exit)
```

### B3. Find or translate — `tb_for_key()` · file 05, 07
```
if map[key] exists: return it
insns = build_block(pc, 128)
loop:
    out = translate(insns) ; on OutOfSlots: insns = first half; retry
    host = code_mem.place(out) ; on Full: flush_all(); retry
record TB (pcmap, exits, fault sites); page_tbs[page].push(id)
```

### B4. Interpreter tier — `cold_run()` / `exec_cold()` · `jit/dispatch.rs` · file 23
```
cold_run(key):                               // called by select() before translating
    if tier == 0 or map[key] exists: return None      // translate / use the TB
    c = cold[key] (create with runs = 0)
    c.runs += 1
    if c.runs > tier: drop c.block; return None       // hot now: translate it
    last_exit = None                                   // nothing to chain to
    return Some(c)                                     // still cold: interpret it
exec_cold(c):
    if c.block is None:                                // first run, or its page was written
        c.block = build_block(pc, 128); cold_pages[page].push(c); mark page as code
    exec_block(c.block)                                // the interpreter's own function
```
Cost model (ski rental): translating costs T ≈ 8 µs, an interpreted run I ≈ 0.1 µs. Break-even k ≈ T / I ≈ 50–90 runs. Interpreting up to a threshold near it is never worse than about 2× the best choice in hindsight. Default: 32.

---

## C. Translation

### C1. Lift — `Lifter::insn()` · `ir/lift.rs` · file 07
```
push Insn{pc, idx}
read(x0) → Const 0 ;  write(x0) → nothing
ALU / load / store / branch / jal / jalr / lui / auipc(→ Const pc+imm) → IR ops
ecall → Exit(Ecall) ; fence.i → Exit(FenceI)
anything else → Interp{raw}  (+ Jump next if it ends the block)
fell off the end → Jump(next pc) or Exit(Fault)
```

### C2. Forwarding — `forward()` · `ir/opt.rs` · O(ops)
```
cur[g] = None ; rename[v] = v
for op: rewrite uses via rename
    ReadReg{dst,g}: if cur[g]=v: rename[dst]=v, drop op   else cur[g]=dst
    WriteReg{g,src}: if cur[g]==src: drop op               else cur[g]=src
    Interp: cur = all None
```

### C3. Dead register writes — `dead_writes()` · O(ops)
```
pending[g] = None
for i, op:
    WriteReg{g}: if pending[g]=j: delete op j ;  pending[g] = i
    ReadReg{g}:  pending[g] = None
    Load|Store|Interp: pending = all None          // fault sites observe everything
```

### C4. Constant folding — `fold()` · O(ops)
```
k[v] = known constant
Bin(c1, c2)            → Const(op.eval(c1, c2))   // eval = the interpreter's alu()
Bin(x, c) (c fits i32) → BinImm(x, c) ;  sub x,c → add x,−c ;  commutative (c, x) → (x, c)
BinImm identity (add/or/xor 0, shift 0, and −1, mul 1) → rename dst = x
and/mul by 0 → Const 0
Branch(c1, c2) → Jump(taken or fall) ;  Branch(v, v) → Jump
JumpInd(c) → Jump(c)                               // becomes chainable
```

### C5. Dead code elimination — `dce()` · O(ops), backwards
```
for op in reverse:
    if op is pure and its result is unused: delete
    else mark its operands used
(loads are not pure: they may fault)
```

### C6. Liveness — `Liveness::compute()` · `ir/liveness.rs`
```
for i, op: for u in op.uses(): uses[u].push(i) ;  def[op.def()] = i
next_use(v, pos) = first entry of uses[v] > pos  (binary search)
```

---

## D. Register allocation (`regalloc/linear_scan.rs`, file 08)

### D1. One forward walk
```
for pos, op:
    operands: r = get(v)          // in a register? else alloc() + fill from backing
    result:   d = def(dst, prefer = an operand's register if it dies here)
    emit x86
    release values whose last use is pos
```

### D2. alloc / evict — furthest next use (Belady-style)
```
alloc(): a free pool register, else
         victim = argmax over pool of next_use(owner(r), pos)   (never-used-again = ∞)
         evict(victim)
evict(r): v = owner(r)
          if v is still needed:
              if v is dirty for guest g: store home(g)           // would happen at exit anyway
              if v still live and has no backing: store to a new spill slot
          free r
```
Backings: `Home(g)`, `Const(c)`, `Slot(k)` make eviction free.

### D3. Lazy write-back
```
WriteReg{g, v}: pinned g → move into R12–R15 now
                else dirty[g] = v                     // no code emitted
before an exit / helper call: store every dirty[g] home once
```

### D4. State map at each memory access
```
record FaultSite { host offset, guest idx+pc, address reg, offset, size,
                   dirty: [(g, where: Reg | Slot | Const | Home)] }
```

### D5. Helper call sync — `sync_for_call()` / `after_call()`
```
store dirty registers ; store pinned R12–R15 home ; spill every value live after the call
forget everything ; call ; reload R12–R15 from home
```

---

## E. Code generation

### E1. ModRM/SIB choice — `modrm_rm()` · `backend/x86/emit.rs` · file 03
```
register operand:                mod=11
no base:                         mod=00 rm=100, SIB base=101, disp32
mod = 00 if disp==0 and base∉{RBP,R13}; 01 if disp fits i8; else 10
if index or base∈{RSP,R12}:      rm=100 + SIB(scale, index or 100, base)
REX = 0100 W R X B, needed if 64-bit, any register ≥ 8, or byte reg SPL/BPL/SIL/DIL
```

### E2. Label fixups — `Asm::finish()`
```
jcc/jmp to an unbound label: emit placeholder, record (field offset, label)
finish(): for each fixup: rel = label_pos − (field + 4)  (or +1 for rel8); write it
```

### E3. Aligned exit slots — `align(4, skew)`
```
pad with NOPs until (here + opcode_length) % 4 == 0 ; then emit jmp (skew 1) or jcc (skew 2)
```

### E4. Division guard — `div_signed()` · file 07
```
if divisor == 0:   q = −1,   r = dividend
elif divisor == −1: q = −dividend (MIN stays MIN), r = 0
else: cqo ; idiv
```

---

## F. Running and chaining (file 09, 10)

### F1. Code buffer — `CodeMem`
```
dual map one memfd: RW view (write) + RX view (execute)
place: off = align16(used); if off+len > size: Full; copy via RW; used = off+len
flush: used = prefix (trampolines survive); all TBs forgotten; generation++
```

### F2. Budget and icount (D30)
```
TB prologue:  budget −= n ; if budget < 0: exit BUDGET (refund n)
early exits refund the unexecuted part (ecall: 1, fault at k: n−k)
after returning: icount += budget_ref − budget
```

### F3. Chaining — `link_last()`, `chain::link()`
```
exit through slot s of TB f with reason NONE → last_exit = (f, s, generation)
next select → TB t:
    if same generation, chaining on, same flags, (system mode: same page):
        rel32 at f.exits[s].patch_at = t.host − (field + 4)   // one aligned atomic store
        f.exits[s].linked = t ; t.incoming.push((f, s))
```

### F4. Unlinking — `unlink_incoming()`
```
for (f, s) in t.incoming: f.exits[s].linked = None ; patch rel32 back to f's stub
then invalidate t (remove from the map)
```

### F5. Jump cache
```
slot = (pc >> 1) & 4095         byte offset = (pc << 3) & 0xFFF0   (16-byte entries)
generated code: if jc[slot].pc == pc (^ flags<<56 in system mode): jmp jc[slot].host
                else exit LOOKUP
dispatcher: before entering TB for pc, jc[slot(pc)] = {pc, host}
validity: cpu.jc_tag = jit_id<<48 | version ; mismatch → clear the whole table
```

---

## G. Memory (file 11)

### G1. Direct mode
```
reserve 2^38 + 8 GiB PROT_NONE ; base = start + 4 GiB
guest g ↔ host base+g ; host protection mirrors guest permissions
JIT: mov r, [rbx + reg + off]   (the host MMU checks; faults arrive as SIGSEGV)
```

### G2. Sv39 walk — `walk()` · `mem/mmu.rs`
```
if M-mode index or satp Bare: identity
check va is sign-extended from bit 38 (or 47 for Sv48)
a = satp.PPN << 12 ; level = 2
loop: pte = load(a + VPN[level]*8)            (non-RAM → access fault)
      invalid or (W and not R) or reserved bits → page fault
      if R or X: leaf → break
      level == 0 → page fault ; level −= 1 ; a = pte.PPN << 12
privilege check (U bit, SUM, never execute U pages from S)
superpage alignment check ; permission check (R / W / X, MXR)
set A (and D for stores) ; return physical page (+ low VPN bits for superpages)
```

### G3. TLB fill — `fill()`
```
w = walk(va) ; perms = w.perms ∩ physical perms (RAM / device)
tag(p) = vpage | (MMIO flag) if allowed, else INVALID
entry = { addr_read: tag(R), addr_write: tag(W) | (CODE flag if code page), addr_code: tag(X),
          addend: host(ppage) − vpage }        // W only granted once D is set
```

### G4. Inline TLB probe (9 instructions)
```
va = base + off ; idx_off = (va >> 7) & (255 << 5)
if (va & (−4096 | size−1)) != tlb[mmu][idx].tag: slow path     // miss, misaligned, flag set
host = va + tlb[mmu][idx].addend ; access [host]
```

### G5. Flush rules
```
satp write, sfence.vma x0, MXR change → flush_all (mmu_gen++, jc_gen++)
sfence.vma addr → flush that page in all 4 TLBs + its jump-cache entries
                  (flush_all if addr lies in a cached superpage range)
```

---

## H. Traps and faults (file 12)

### H1. Trap entry — `take_trap()`
```
target = S if prv ≤ S and the cause is delegated, else M
xepc = pc ; xcause = cause (|1<<63 if interrupt) ; xtval = tval
xPIE = xIE ; xIE = 0 ; xPP = prv ; prv = target
pc = xtvec (vectored interrupts: base + 4·cause) ; clear reservation
```

### H2. Interrupt choice — `pending_interrupt()`
```
p = mip & mie
M-level (not delegated) taken if prv < M or MIE ; S-level (delegated) if prv < S or (prv = S and SIE)
priority: MEI > MSI > MTI > SEI > SSI > STI
```

### H3. Host fault → precise guest fault
```
SIGSEGV handler: if RIP in code buffer:
    cpu = RBP − 128 ; save RIP, fault address, all 16 registers ; RIP = fault_exit
fault_exit: exit_reason = HOST_FAULT → exit_jit → Rust
resolve: TB = binary search by host address ; site = fault_sites[rip − TB.host]
         write dirty guest registers home from the state map ; pc = site.pc
         budget += n − site.idx ; tval = saved address register + offset
```

---

## I. Self-modifying code (file 13)
```
mark: translating from page P → CODE bit on P (direct: host page read-only; softmmu: TLB_CODE)
detect: any write path to a CODE page → clear the bit, restore write access, smc_pages.push(P)
stop right after the store (interpreter / helper / host fault / softmmu slow path)
drain: for TB in page_tbs[P]: unlink_incoming(TB) ; invalidate(TB) ; jump cache version++
FENCE.I: only reset the jump cache (nothing stale is left)
```

---

## J. Verification (file 17)

### J1. Lockstep per block
```
snapshot ; enable write log + MMIO recording
interpreter runs the TB ; save its result + written values
restore snapshot ; undo writes ; MMIO in replay mode
JIT runs the TB with budget = n
compare state, exit, memory, MMIO sequence → report the first difference
```

### J2. Differential fuzzing
```
repeat N times: random block + random registers
    run in interpreter ; run in JIT (each configuration) ; compare everything
on failure: shrink to a minimal case ; save it in the regression file
```

---

## K. User mode and system mode (files 15, 16)

### K1. Initial stack — `build_stack()`
```
push strings (exe path, argv, envp) and 16 random bytes at the top
sp = align16(top − 8·(1 + argc + 1 + envc + 1 + 2·(auxv+1)))
write: argc, argv ptrs, 0, envp ptrs, 0, auxv pairs, AT_NULL 0
```

### K2. mmap placement — `find_free()`
```
cand = MMAP_TOP − len
scan pages of [cand, cand+len) from the top: if one is mapped, cand = that page − len, rescan
```

### K3. Fair lock (GIL) — `Gil::lock()`
```
lock: my = next++ ; wait until serving == my
unlock: serving++ ; wake all
```

### K4. Machine loop — `machine::boot()`
```
loop:
    devices → each hart's mip (CLINT msip/mtimecmp, PLIC outputs, SBI timer)
    h = next runnable hart (round-robin) ; none → sleep until timer/input/disk
    stop = engine.run(h, slice)
    Ecall → SBI call (+ IPI / fence / hart start-stop / shutdown) ; Wfi → h waits
    power-off requested → end
```

---

## L. Floating-point fix-ups (file 14)
```
after an SSE op: if result is NaN (ucomis x,x → PF): result = canonical NaN
single operand: if upper 32 bits != all ones: operand = canonical NaN
float → int: if result == 0x8000…: NaN or +overflow → MAX ; −overflow → MIN
FMA: if NaN result and (0 × ∞): set NV
fast FP TB prologue: require FS = Dirty (and frm = RNE for dynamic-rm ops) else exit FP_VARIANT
exit_jit: fflags |= map(MXCSR flags)
```
