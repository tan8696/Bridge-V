# 12 · Exceptions, interrupts and precise faults

## What you will learn

- the difference between **exceptions** and **interrupts**
- what happens on a RISC-V **trap**: saving `pc`, changing privilege, jumping to the handler, delegation
- how interrupts are prioritised and when they are taken
- what **precise** exceptions are and why they are hard for a JIT
- the full path of a crash inside generated code: host SIGSEGV → signal handler → `fault_exit` → `resolve_host_fault` → exact guest exception
- how user mode and system mode deliver the result differently

Files: [`src/cpu/trap.rs`](../../src/cpu/trap.rs), [`src/user/signal.rs`](../../src/user/signal.rs), the fault functions at the end of [`src/jit/dispatch.rs`](../../src/jit/dispatch.rs), `deliver()` in [`src/interp/mod.rs`](../../src/interp/mod.rs).

---

## 1. Exceptions and interrupts

Both make the CPU stop what it is doing and jump to a handler. RISC-V calls either one a **trap**.

| | Exception | Interrupt |
|---|---|---|
| Caused by | the current instruction | something outside: a timer, a device, another CPU |
| Timing | synchronous: at a specific instruction | asynchronous: between instructions, whenever |
| Examples | illegal instruction, page fault, `ecall`, `ebreak`, misaligned access | timer tick, keypress on the serial port, disk done |

### 1.1 Exception causes (`mcause`/`scause` with the top bit clear)

| Code | Exception |
|---:|---|
| 0 | instruction address misaligned |
| 1 | instruction access fault |
| 2 | illegal instruction (`tval` = the instruction bits) |
| 3 | breakpoint (`ebreak`) |
| 4 / 6 | load / store address misaligned |
| 5 / 7 | load / store access fault (physical address not allowed) |
| 8 / 9 / 11 | `ecall` from U / S / M mode |
| 12 / 13 / 15 | instruction / load / store page fault (virtual address not allowed) |

Bridge-V supports misaligned loads and stores transparently, so causes 4 and 6 are never raised for them.

### 1.2 Interrupt causes (top bit set)

| Code | Interrupt |
|---:|---|
| 1 / 3 | supervisor / machine software interrupt (SSI / MSI), e.g. from another CPU |
| 5 / 7 | supervisor / machine timer interrupt (STI / MTI) |
| 9 / 11 | supervisor / machine external interrupt (SEI / MEI), from devices through the PLIC |

---

## 2. Taking a trap (`take_trap()` in `trap.rs`)

When a trap happens, the hardware (here, Bridge-V) does this:

```
choose the target level:
    if current privilege ≤ S  and  the cause is delegated (medeleg / mideleg bit set):  target = S
    else:  target = M
at the target level x (S or M):
    xepc   = pc                    // where it happened
    xcause = cause (| 1<<63 for interrupts)
    xtval  = extra info (bad address, instruction bits)
    xPIE   = xIE ; xIE = 0         // remember and disable interrupts
    xPP    = old privilege         // remember where we came from
    privilege = x
    pc = xtvec                     // or xtvec + 4*cause for vectored interrupts
    clear any LR/SC reservation
```

**Delegation** lets M-mode firmware hand most traps straight to the S-mode kernel. Bridge-V's built-in firmware delegates every synchronous exception except `ecall` from S-mode (which the firmware itself must serve), plus the supervisor interrupts (`mideleg = 0x222`).

**Returning:** `mret`/`sret` restore `privilege = xPP`, `xIE = xPIE`, set `xPIE = 1`, `xPP = U`, and jump to `xepc`. The handler usually adds 4 to `xepc` first if it wants to skip the faulting instruction (for `ecall`).

---

## 3. Interrupts: when and which

`pending_interrupt()` decides if an interrupt should be taken now:

```
pending = mip & mie                               // raised AND individually enabled
M-level interrupts (not delegated) are taken if:  privilege < M,  or  mstatus.MIE = 1
S-level interrupts (delegated)     are taken if:  privilege < S,  or  (privilege == S and mstatus.SIE = 1)
priority among several:  MEI > MSI > MTI > SEI > SSI > STI
```

**When does Bridge-V check?** Only between translation blocks (`deliver_interrupt()` at the top of the dispatcher loop). The budget (file 10) guarantees the dispatcher runs at least every 100,000 instructions, and in practice much more often. Instructions that can enable interrupts (CSR writes to `mstatus`, `mie`, …, and `mret`/`sret`) end a block or leave the TB so a newly enabled interrupt is noticed promptly (decision D51).

In system mode, the machine loop (file 16) updates `mip` between slices from the devices: timer deadline reached → STIP/MTIP; serial port has data → PLIC → SEIP.

---

## 4. Precise exceptions: the hard part for a JIT

An exception is **precise** if, when the handler starts:
- every instruction *before* the faulting one has completely finished
- the faulting instruction and everything after it have had **no effect**
- `pc` (`xepc`) points exactly at the faulting instruction

The guest OS depends on this. For example, Linux handles a page fault by mapping the page and re-running the instruction. If a register had already been changed, or `pc` pointed at the wrong instruction, the program would silently compute wrong results.

**Why it's hard for Bridge-V's JIT:**
1. **Lazy write-back** (file 08): at the moment of a fault, some guest registers exist only in x86 registers; `CpuState` still holds older values.
2. **Budget charging** (file 10): the block's prologue already counted all its instructions as retired.
3. **No checks in the fast path** (direct mode): a guest load is one x86 `mov`. There's no code to notice a problem, so the fault arrives as a host signal in the middle of generated code.

---

## 5. The path of a crash in generated code (direct mode)

Suppose the guest does `ld a5, 0(a4)` with a garbage `a4`. The generated `mov rcx, [rbx + rax]` touches a `PROT_NONE` page.

### Step 1: the host kernel sends SIGSEGV

The CPU raises a page fault, the host Linux kernel finds no valid mapping, and delivers SIGSEGV to Bridge-V's thread, with a snapshot of all registers at the moment of the fault (the `ucontext`).

### Step 2: Bridge-V's signal handler (`handler()` in `signal.rs`)

It was installed with `sigaction(SIGSEGV, SA_SIGINFO | SA_ONSTACK)` by `Jit::new()`.

```rust
extern "C" fn handler(sig, info, ctx) {
    let (start, end, fault_exit) = JIT_RANGE.get();          // set by exec() for this thread
    let rip = ctx.gregs[REG_RIP];
    if rip >= start && rip < end {                           // the crash is in OUR generated code
        let cpu = (ctx.gregs[REG_RBP] - 128) as *mut CpuState;   // RBP always = &CpuState + 128
        (*cpu).fault_rip  = rip;
        (*cpu).fault_addr = info.si_addr();
        for n in 0..16 { (*cpu).fault_regs[n] = ctx.gregs[GREG_OF[n]]; }   // all 16 host registers
        ctx.gregs[REG_RIP] = fault_exit;                     // resume at fault_exit, not the bad mov
        return;
    }
    // not ours: pass it on to the previous handler (e.g. Rust's stack-overflow handler)
}
```

Clever points:
- The handler finds `CpuState` through **RBP**, which the block-boundary rules guarantee always holds `&CpuState + 128` inside generated code.
- It only does plain memory stores, which are safe inside a signal handler ("async-signal-safe").
- By **changing RIP** in the saved context, returning from the handler resumes the thread at `fault_exit` instead of retrying the bad instruction.
- The code-buffer range is stored per thread (`thread_local!`), so each guest thread's JIT is handled correctly.

### Step 3: `fault_exit` → `exit_jit`

`fault_exit` sets `exit_reason = HOST_FAULT` and jumps to `exit_jit`, which returns normally to Rust. This works because generated code never pushes to the stack, so RSP is exactly where `enter_jit` left it.

### Step 4: `resolve_host_fault()` rebuilds the exact state

```rust
fn resolve_host_fault(&self, cpu, mem) -> Exception {
    let rip = cpu.fault_rip;
    let tb = self.cache.find_host(rip);                 // binary search: which TB contains rip?
    if !tb.fault_sites.is_empty() {                     // IR back end:
        return resolve_site(tb, cpu, mem, rip);         //   use the fault site's state map
    }
    // naive back end: all registers are already in CpuState; use the pcmap
    let e = tb.entry_for(rip);                          // which guest instruction?
    cpu.pc = e.guest_pc;
    cpu.budget += (tb.insns.len() - e.idx) as i64;      // refund instructions that never ran
    ...
}
```

`resolve_site` → `apply_site` (IR back end):
1. finds the `FaultSite` whose `rip_off` equals the faulting offset (every load and store recorded one, file 07 §5.3)
2. writes every dirty guest register home from where the state map says it is (a saved host register, a spill slot, a constant or another home)
3. sets `cpu.pc = site.pc`, the faulting guest instruction
4. refunds the budget for this instruction and all later ones in the block
5. computes the exact faulting guest address (`tval`): the address register's saved value + offset. For an access that straddles into a bad page, it reports the first bad byte, exactly like the interpreter does.

Then `exec()` adds `budget_ref − budget` to `icount`, which is now exact.

### Step 5: deliver the exception

`exec()` returns `BlockExit::Trap(load access fault at <address>)`, and `deliver()` does the same thing it would for the interpreter:
- **user mode:** `Err(Stop::Fault(e))`. The user-mode loop delivers a guest SIGSEGV to the program's own handler if it has one (file 15), or ends the program with exit status 128 + 11 (like a real crash).
- **system mode:** `cpu.take_trap(e)`. The guest kernel's page-fault handler runs.

The whole path is tested by `tests/cli.rs::jit_host_fault_is_precise`, and by the block fuzzer, whose random blocks often end in a faulting access.

---

## 6. Faults in the softmmu slow path (system mode)

In system mode, loads and stores go through the inline TLB check, and misses go to `helper_mmu_access` (file 11). If the page walk fails, the helper doesn't return normally: it sets `exc_cause`, `exc_tval` and `exit_reason = MMU_FAULT`. The slow-path stub saves R12–R15 into `fault_regs`, sets `fault_rip` to the access instruction's address, and exits. `resolve_mmu_fault()` then calls the same `apply_site()`, so the result is just as precise, with no signals involved.

---

## 7. Exceptions from helpers

Instructions run by `helper_interp_one` (CSR accesses, atomics, …) can also raise exceptions, for example an illegal CSR. The helper sets `exc_cause`, `exc_tval`, `pc` and `exit_reason = EXCEPTION`, returns 1, and the generated code jumps to its helper-exit stub. Because the call was preceded by a full register sync (file 08, §6), `CpuState` is already exact.

---

## 8. Other uses of the same signal machinery

- **Self-modifying code** in direct mode: code pages are write-protected on the host, so a guest store to one also arrives as SIGSEGV. `exec()` checks `is_smc_fault()` first and handles it as a code write, not an error (file 13).
- **Profiling** (`--profile-tbs`): a SIGPROF timer interrupts the process about 250–1,000 times per second; the handler records the interrupted RIP, and the statistics show which TBs are hottest.

---

## Check yourself

1. What is the difference between an exception and an interrupt? Give two examples of each.
2. List the steps of taking a trap into S-mode. What is delegation?
3. When does Bridge-V check for pending interrupts, and why is that often enough?
4. What makes an exception "precise"? Why does the guest OS need it?
5. Name the three JIT design choices that make precise exceptions difficult.
6. Trace a bad load in generated code from the host page fault to the guest's exception. What does each step do?
7. How does the signal handler find `CpuState`?
8. Why can the handler resume at `fault_exit` without knowing where in the block it was?
9. How are faults in the softmmu slow path different from direct-mode faults?
