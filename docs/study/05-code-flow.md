# 05 · Code flow: one program, from `main()` to exit

## What you will learn

This file follows one command through the real source code, function by function:

```
bridgev run --engine jit guest/build/hello-O2.elf
```

By the end you will know which function calls which, what data each step produces, and where control goes when the program makes a system call and when it exits. At the end there is a shorter trace for system mode (`bridgev boot`).

Keep [04-code-map.md](04-code-map.md) open for the file list. Code excerpts here are simplified: error handling and statistics are left out.

---

## 0. The whole flow on one screen

```
main()                                         src/main.rs
 └─ run_user()                                 src/main.rs
     └─ user::run()                            src/user/mod.rs
         ├─ loader::load()                     src/user/loader.rs     ← build memory, stack, CpuState
         └─ thread::run_process()              src/user/thread.rs
             └─ [new host thread] guest_thread()
                 ├─ make_engine() → Jit::new() src/jit/mod.rs, dispatch.rs
                 │    ├─ CodeMem::new()        src/jit/code_mem.rs    ← executable memory
                 │    ├─ Trampolines::generate()  src/jit/trampoline.rs ← enter_jit / exit_jit
                 │    └─ signal::install()     src/user/signal.rs     ← SIGSEGV handler
                 └─ loop {
                      engine.run()  = Jit::run()                      src/jit/dispatch.rs
                        └─ loop {
                             select()           ← find or translate the TB at pc, link the last exit
                               ├─ cold_run()     ← ran ≤ 32 times? then exec_cold() interprets it (file 23)
                               └─ tb_for_key()
                                   ├─ build_block_max()   src/interp/mod.rs   ← decode
                                   ├─ translate_insns()
                                   │    ├─ lift_with()     src/ir/lift.rs      ← to IR
                                   │    ├─ optimize()      src/ir/opt.rs       ← 4 passes
                                   │    └─ lower_ir::translate()  src/backend/x86/lower_ir.rs
                                   │         (uses Alloc from regalloc, Asm from emit.rs)
                                   ├─ cm.place()           ← copy bytes into the code buffer
                                   └─ cache.insert()       ← remember the TB
                             exec()             ← run it: enter_jit → TB → … → exit_jit
                             deliver()          ← ECALL? exception? continue?
                           }  returns Stop::Ecall / Fault / Limit
                      match stop {
                        Ecall → Syscalls::dispatch()           src/user/syscall.rs
                        Fault → guest signal or fatal exit
                        Exit  → end_process() → finish
                      }
                    }
```

Now each step in detail.

---

## 1. `main()`: parse the command line

[`src/main.rs`](../../src/main.rs)

```rust
fn main() -> ExitCode {
    match Cli::parse().command {                 // clap turns argv into this enum
        Command::Run { mode, engine, elf, args, .. } => {
            let jit = JitOptions { max_block, code_cache, chain: !no_chain, regalloc, pin, .. };
            match mode {
                Mode::Bare => run_bare(&elf, BareOptions { .. }, stats),
                Mode::User => run_user(&elf, args, RunOptions { engine, jit, .. }, stats),
            }
        }
        Command::Boot { .. } => run_boot(BootOptions { .. }, stats),
        Command::Mkinitramfs { .. } => ...,
        Command::Disasm { elf } => disasm_file(&elf),
    }
}
```

- `Cli` is a struct with `#[derive(Parser)]`: the `clap` library generates the whole argument parser from the struct's fields and their `///` comments (which become `--help` text).
- `JitOptions` (defined in `dispatch.rs`) collects every JIT flag. Defaults: max 128 instructions per block, 256 MiB code cache, chaining on, `regalloc = Linear`, pinned registers `x2, x1, x10, x15`, slice 100,000 instructions.

`run_user()` builds `argv` (the ELF path plus your arguments) and `envp` (your host environment variables), starts a timer, and calls `user::run()`. When that returns, it prints `--stats` if asked and returns the guest's exit code as Bridge-V's own exit code.

---

## 2. `user::run()`: load, then start the process

[`src/user/mod.rs`](../../src/user/mod.rs)

```rust
pub fn run(path, args, envs, opts) -> Result<RunResult> {
    let mut p = loader::load(path, args, envs, opts.sysroot)?;   // a ready-to-run Process
    p.cpu.softmmu = if opts.softmmu { 2 } else { 0 };           // direct memory by default
    let env = Env { user_mode: true, tohost: None, trace: opts.trace, sbi: false };
    if let Some(port) = opts.gdb { return gdb::serve(...); }     // --gdb: debugger mode
    thread::run_process(p, opts, env, strace)
}
```

`Env` tells the engine what world it is in. `user_mode: true` means "when the guest does `ecall` or crashes, stop and tell the caller" (instead of jumping into a guest kernel, which is what happens in system mode).

---

## 3. `loader::load()`: build the guest's world

[`src/user/loader.rs`](../../src/user/loader.rs)

Steps, in order:

1. **Read and parse the ELF.** `Elf::parse(&data)` checks the header (64-bit, little-endian, RISC-V) and reads the program headers.
2. **Reserve the address space.** `DirectMem::new()` asks the host for **256 GiB + two 4 GiB guard regions** of address space with `PROT_NONE` (no access) and `MAP_NORESERVE`. This costs no real memory; it only reserves addresses. Guest address `g` will always live at host address `base + g`.
3. **Map the segments.** `map_segments()` works out the permissions of every page (a page shared by two segments gets both permissions), maps runs of pages with `mem.map()` (real host memory with matching host protection), then copies the segment bytes in with `mem.write_bytes()`.
4. **Dynamic programs only:** if the ELF names an interpreter (`PT_INTERP`, e.g. `ld-linux-riscv64-lp64d.so.1`), load that too, at `0x3f_8000_0000`, and start there instead.
5. **Map the stack**: 8 MiB just below `0x3f_ffff_f000`, readable and writable.
6. **Map the signal trampoline page**: a few instructions (`li a7, 139; ecall`) that guest signal handlers return through (file 15).
7. **Build the initial stack** (`build_stack()`): copy the strings (program path, arguments, environment, 16 random bytes) to the top, then below them write `argc`, the `argv` pointers, `NULL`, the `envp` pointers, `NULL`, and the **auxiliary vector** (pairs like `AT_PAGESZ = 4096`, `AT_ENTRY = entry point`, `AT_RANDOM = pointer to the random bytes`). This is exactly the layout Linux gives a new process, so the C library's start-up code works unchanged.
8. **Create the CPU**: `CpuState::new_user(entry)` gives U-mode, FPU enabled, `pc = entry`. Then `cpu.x[2] = sp` (the stack pointer register).
9. Return a `Process { mem, cpu, brk, ... }`. `brk` (the end of the heap) starts right after the highest segment.

---

## 4. `thread::run_process()` and `guest_thread()`: the outer loop

[`src/user/thread.rs`](../../src/user/thread.rs)

Bridge-V supports guest threads, so even a single-threaded program runs inside this threading framework:

- `run_process` wraps the `Process` and a `Syscalls` object in a `Shared` struct protected by a **GIL** (a fair "global interpreter lock": only one guest thread runs guest code at a time).
- It `spawn`s a host thread (named `guest-<tid>`) that runs `guest_thread()`, then waits until the process finishes.

`guest_thread()` is **the main user-mode loop**:

```rust
fn guest_thread(proc, mut cpu, tid, ..) {
    let mut engine = make_engine(proc.opts.engine, &proc.opts.jit)?;   // each thread: its own JIT
    loop {
        let mut g = proc.gil.lock();                 // take our turn
        swap(&mut s.p.cpu, &mut cpu);                // put this thread's registers in place
        match engine.run(&mut s.p.cpu, &mut s.p.mem, &proc.env, left) {
            Stop::Ecall => match s.sys.dispatch(&mut s.p, engine) {   // a system call
                SysOut::Ret(v)  => { cpu.x[10] = v; cpu.pc += 4; cpu.icount += 1; }
                SysOut::Exit(c) => return end_process(proc, s, tid, c, engine),
                SysOut::Clone(c) => { /* start a new guest thread */ }
                SysOut::Block(nr, a) => { /* run a blocking syscall after dropping the lock */ }
                ..
            },
            Stop::Fault(e) => { /* deliver a guest signal, or die with 128 + signal */ }
            Stop::Limit => { /* --max-insns reached, or time slice for other threads */ }
            ..
        }
        swap(&mut s.p.cpu, &mut cpu);                // save this thread's registers
        drop(g);                                     // let other threads in
    }
}
```

Key idea: **the engine runs until it needs help, then returns a `Stop` value explaining why.** The outer loop handles the reason and calls the engine again.

`make_engine()` ([`src/jit/mod.rs`](../../src/jit/mod.rs)) creates `Interp::new()`, `Jit::new(opts)` or `Lockstep::new(opts)` behind a `Box<dyn Engine>`.

---

## 5. `Jit::new()`: prepare the translator

[`src/jit/dispatch.rs`](../../src/jit/dispatch.rs)

1. `CodeMem::new(256 MiB, DualMap)` ([`code_mem.rs`](../../src/jit/code_mem.rs)) creates an anonymous in-memory file with `memfd_create` and maps it **twice**: once read+write (where code is written) and once read+execute (where code runs). Never both at once. (File 09.)
2. Builds the `pinned` table: guest x2→R12, x1→R13, x10→R14, x15→R15.
3. `Trampolines::generate()` ([`trampoline.rs`](../../src/jit/trampoline.rs)) writes the first bytes of the code buffer:
   - a **helper table**: absolute addresses of the Rust functions `helper_interp_one` and `helper_mmu_access`
   - **`enter_jit`**: the doorway from Rust into generated code
   - **`exit_jit`**: the doorway back
   - **`fault_exit`**: where the SIGSEGV handler sends a crashing block

   These are "sealed" so a later cache flush never erases them.
4. Detects CPU features (`cpuid`: BMI2, FMA).
5. `signal::install()` registers the SIGSEGV handler (file 12).

---

## 6. `Jit::run()`: the dispatcher loop

This is the `Engine::run` implementation for the JIT (bottom half of `dispatch.rs`):

```rust
fn run(&mut self, cpu, mem, env, max_insns) -> Stop {
    let limit = cpu.icount + max_insns;
    loop {
        if cpu.icount >= limit { return Stop::Limit; }
        if cpu.softmmu != 0 { deliver_interrupt(cpu, env); }      // system mode only
        let exit = match self.select(cpu, mem) {                  // which TB runs next?
            Next::Tb(id) => {
                let n = self.cache.get(id).insns.len() as u64;
                let budget = (limit - cpu.icount).min(self.opts.slice).max(n);
                self.exec(cpu, mem, id, budget as i64)            // run native code
            }
            Next::Cold(i) => self.exec_cold(cpu, mem, i),         // not hot yet: interpret (file 23)
            Next::Straddle => self.interpret_one(cpu, mem),       // rare softmmu case
            Next::Fault(e) => BlockExit::Trap(e),                 // can't even fetch
        };
        if exit == BlockExit::Flush { self.fence_i(); }
        if let Err(stop) = deliver(exit, env, cpu) {              // needs the caller?
            return stop;                                          // e.g. Stop::Ecall
        }
    }
}
```

The **budget** is how many guest instructions the generated code may run before it must come back. It is normally 100,000 (the "slice"), so even an infinite guest loop returns control regularly.

---

## 7. `select()` → `tb_for_key()`: find or create a translation

```rust
pub fn select(&mut self, cpu, mem) -> Next {
    if !mem.smc_pages.is_empty() { self.drain_smc(cpu, mem); }  // code was overwritten (file 13)
    if cpu.softmmu == 0 {                                        // user mode, direct memory
        let slow = fp_slow(cpu);
        if let Some(i) = self.cold_run(TbKey::direct(cpu.pc, self.variant(slow))) {
            return Next::Cold(i);                                // ran ≤ --tier times: interpret it
        }
        let id = self.next_tb(cpu.pc, mem, slow);                // = tb_for_variant + link_last
        self.mark_new_code(cpu, mem);                            // write-protect new code pages
        return Next::Tb(id);
    }
    // system mode: translate pc through the MMU first, key the TB by physical page ...
}
```

- **`cold_run()`** (Phase 12, file 23): if the block has no translation yet, count this run. While it has run at most `--tier` times (default 32), the answer is `Next::Cold`, and the interpreter runs it. The next run translates it. So `hello` translates very little, while CoreMark's loops are translated after 32 runs.
- `tb_for_variant()` builds a `TbKey { pc, slow: <FP variant>, flags: 0, ppage: 0 }` and calls `tb_for_key()`.
- **`link_last(id)`** is block chaining: if the previous block left through a direct exit that isn't linked yet, patch that exit to jump straight to TB `id` next time (file 10).

`tb_for_key()`:

```rust
fn tb_for_key(&mut self, key, soft, build) -> u32 {
    if let Some(id) = self.cache.lookup_key(&key) { return id; }   // hit: already translated
    let block = build(self.opts.max_block);          // 1. DECODE: build_block_max(pc, mem, 128)
    let (host, out) = loop {
        let origin = self.cm.next_addr();            // where the code will live
        let out = match self.translate_insns(&insns, fetch_fault, key, origin, id, soft) {
            Ok(out) => out,                          // 2. TRANSLATE
            Err(OutOfSlots) => { insns.truncate(insns.len() / 2); continue; }  // too big: halve
        };
        match self.cm.place(origin, &out.code) {     // 3. PLACE in the code buffer
            Ok(host) => break (host, out),
            Err(Full) => self.flush_all(),           // buffer full: throw everything away, retry
        }
    };
    let id = self.cache.insert(TranslationBlock { guest_pc, insns, host, pcmap, exits, .. });
    self.page_tbs.entry(page).or_default().push(id); // remember which page it came from (SMC)
    id
}
```

**Decoding** reuses the interpreter's `build_block_max()` ([`src/interp/mod.rs`](../../src/interp/mod.rs)): fetch 16 bits, decode, repeat, and stop at the first branch/jump/system instruction, at a 4 KiB page boundary, or after 128 instructions.

**Translating** (`translate_insns()`):

```rust
if regalloc == None { return lower::translate(...); }     // old 1:1 back end
let mut ir = lift_with(insns, fetch_fault, pc, opts);     // Inst → IR           (file 07)
if regalloc == Linear { optimize(&mut ir); }              // forward, dead_writes, fold, dce
lower_ir::translate(&ir, origin, id, &self.tr, iopts)     // IR → x86 bytes      (files 07, 08)
```

`lower_ir::translate()` returns the machine code (`Vec<u8>`), a **pcmap** (which host offset belongs to which guest instruction), the two **exit slots** (where the patchable jumps are), and the **fault sites** (the state maps for precise faults).

---

## 8. `exec()`: run the native code

```rust
pub fn exec(&mut self, cpu, mem, id, budget) -> BlockExit {
    // make sure the jump cache belongs to this JIT and this code generation
    if cpu.jc_tag != self.jc_tag() { cpu.clear_jump_cache(); cpu.jc_tag = self.jc_tag(); }
    cpu.jmp_cache[jc_index(guest_pc)] = JcEntry { pc, host };   // fill the entry for this TB
    cpu.budget = budget;  cpu.budget_ref = budget;
    cpu.exit_reason = exit::NONE;
    cpu.mem_base = mem.base();                                 // for RBX
    cpu.helper_mem = mem as *mut DirectMem as u64;             // for Rust helpers
    signal::set_jit_range(start, end, self.tr.fault_exit);    // tell the SIGSEGV handler
    let code = unsafe { self.tr.enter(cpu, host) };            // ***** RUN *****
    signal::clear_jit_range();
    let exit = match cpu.exit_reason {                         // why did it come back?
        exit::NONE | exit::BUDGET | exit::LOOKUP | exit::FP_VARIANT => BlockExit::Continue,
        exit::ECALL      => BlockExit::Ecall,
        exit::EXCEPTION  => BlockExit::Trap(Exception { cause: cpu.exc_cause, tval: cpu.exc_tval }),
        exit::HOST_FAULT => BlockExit::Trap(self.resolve_host_fault(cpu, mem)),   // file 12
        ..
    };
    cpu.icount += (cpu.budget_ref - cpu.budget) as u64;       // count retired instructions
    let slot = (code & 3) as u8;                               // exit code = (tb_id << 2) | slot
    self.last_exit = (slot < 2 && reason == exit::NONE).then_some(((code >> 2) as u32, slot, gen));
    exit
}
```

`self.tr.enter(cpu, host)` casts the address of `enter_jit` to a function pointer of type `extern "sysv64" fn(*mut CpuState, u64) -> u64` and calls it. From here on, **the real CPU is executing bytes Bridge-V generated**.

### 8.1 What the CPU does inside (the generated code)

```
enter_jit:                              (from trampoline.rs)
    push rbp, rbx, r12, r13, r14, r15   ; save the caller's callee-saved registers
    sub  rsp, 8                         ; keep the stack 16-byte aligned
    ldmxcsr [default]                   ; clean FP state
    lea  rbp, [rdi + 128]               ; RBP = &CpuState + 128
    mov  rbx, [rbp + mem_base]          ; RBX = guest memory base
    mov  r12, [rbp + x2] ... r15        ; load the 4 pinned guest registers
    mov  r9,  [rbp + budget]            ; R9 = instruction budget
    jmp  rsi                            ; jump into the TB

TB (e.g. the loop at 0x10990):          (from lower_ir.rs)
    sub  r9, 3        ; charge 3 guest instructions
    jl   budget_stub  ; out of budget → leave before doing anything
    ... the translated instructions ...
    jne  stub_1       ; taken branch  (patched later to jump straight to the target TB)
    jmp  stub_0       ; fall-through  (patched later too)
stub_1:
    mov  qword [rbp + pc], 0x10990      ; tell Rust where we were going
    mov  eax, (tb_id << 2) | 1          ; exit code: which TB, which exit
    jmp  exit_jit

exit_jit:
    mov  [rbp + x2], r12 ... r15        ; write the pinned registers back
    mov  [rbp + budget], r9
    (fold FP exception flags into fflags)
    add  rsp, 8 ; pop r15 ... rbp ; ret ; back to Rust, RAX = exit code
```

Once blocks are chained, `jne stub_1` becomes `jne <start of the next TB>`, and the CPU can run thousands of blocks between one `enter_jit` and the next `exit_jit`.

---

## 9. Back in `Jit::run`: `deliver()`

[`src/interp/mod.rs`](../../src/interp/mod.rs), shared by all engines:

```rust
pub fn deliver(exit: BlockExit, env: &Env, cpu: &mut CpuState) -> Result<(), Stop> {
    match exit {
        BlockExit::Continue | BlockExit::Flush => Ok(()),              // keep going
        BlockExit::Ecall if env.user_mode => Err(Stop::Ecall),         // user mode: return
        BlockExit::Ecall => { cpu.take_trap(ecall exception); Ok(()) } // system mode: trap
        BlockExit::Trap(e) if env.user_mode => Err(Stop::Fault(e)),
        BlockExit::Trap(e) => { cpu.take_trap(e, false); Ok(()) },
        BlockExit::Wfi => Err(Stop::Wfi),
    }
}
```

So in user mode, an `ecall` makes `Jit::run` return `Stop::Ecall` to `guest_thread`.

---

## 10. A system call: `write(1, "Hello, world!\n", 14)`

1. glibc's `write` puts `64` (the `write` syscall number) in `a7`, `1` in `a0`, the buffer address in `a1`, `14` in `a2`, and runs `ecall`.
2. The block containing `ecall` was lifted to `Op::Exit { kind: Ecall, pc }`. Its stub gives back one unit of budget (the `ecall` hasn't retired yet), stores `pc` and `exit_reason = ECALL`, and jumps to `exit_jit`.
3. `exec` returns `BlockExit::Ecall`; `deliver` returns `Err(Stop::Ecall)`; `Jit::run` returns `Stop::Ecall`.
4. `guest_thread` calls `Syscalls::dispatch()` ([`src/user/syscall.rs`](../../src/user/syscall.rs)). It reads `nr = x[17]` and `a = x[10..16]`, then:
   ```rust
   64 => {   // write(fd, buf, count)
       let buf = gptr(p, a[1], a[2], prot::R)?;       // check: is [a1, a1+a2) readable guest memory?
       Ok(host_ret(unsafe { libc::write(a[0] as i32, buf, a[2] as usize) }))
   }
   ```
   `gptr` turns the guest address into a host pointer (base + address) *after* checking permissions, so a bad pointer gives `-EFAULT` instead of crashing Bridge-V.
5. The result (14) goes into `a0`; `pc += 4` skips the `ecall`; `icount += 1`.
6. The loop calls `engine.run()` again, which continues at the instruction after `ecall`.

Most syscalls pass straight through like this because riscv64 and x86-64 Linux share the same structure layouts. A few need conversion. For example `struct stat` is 128 bytes on riscv64 but 144 on x86-64, so `stat_to_guest()` copies it field by field.

---

## 11. The end: `exit_group(0)`

1. glibc's `exit` runs `ecall` with `a7 = 94` (`exit_group`).
2. `dispatch` returns `SysOut::Exit(0)`.
3. `guest_thread` calls `end_process()`, which records `RunResult { exit_code: 0, icount, engine_stats }` and wakes the main thread.
4. `run_process` returns it to `user::run`, then to `run_user`, which prints the stats (with `--stats`) and returns `ExitCode::from(0)`.

With `--stats` you see a line like:
```
bridgev: 53821813220 guest instructions in 11.500 s (4680.2 MIPS)
bridgev: jit: 1691 TBs translated (9503 guest insns, 231 KiB host code, 24.9 bytes/insn), ...
         540245 dispatcher entries (10 per M guest insns); ... chain: on, 1733 links
```
(from a CoreMark run). "10 dispatcher entries per million guest instructions" means chaining kept the CPU in generated code 99.999% of the time.

---

## 12. When something goes wrong: a bad memory access

If the guest loads from an unmapped address, the generated `mov rcx, [rbx+rax]` touches an inaccessible host page and the host kernel sends **SIGSEGV**:

1. `handler()` in [`src/user/signal.rs`](../../src/user/signal.rs) sees that the faulting instruction address (RIP) is inside the code buffer. It saves RIP, the fault address and all 16 host registers into `CpuState`, then changes RIP to `fault_exit`.
2. `fault_exit` sets `exit_reason = HOST_FAULT` and jumps to `exit_jit`.
3. `exec` calls `resolve_host_fault()`: finds the TB containing RIP, finds the **fault site** record for that exact instruction, writes the unsaved guest registers back from the saved host registers (the site's **state map**), sets `pc` to the faulting guest instruction, and refunds the budget for instructions that never ran.
4. The result is `BlockExit::Trap(load access fault at <address>)`, exactly what the interpreter would report. In user mode that becomes a guest SIGSEGV (file 12, file 15).

---

## 13. The same trace for the interpreter (`--engine interp`)

`Interp::run()` ([`src/interp/mod.rs`](../../src/interp/mod.rs)) has the same outer shape, but instead of translating it looks up a cached **decoded** block and executes it in Rust:

```rust
loop {
    if cpu.icount >= limit { return Stop::Limit; }
    deliver_interrupt(cpu, env);
    let block = self.block(cpu, mem);                 // cache by pc, else build_block()
    let exit = exec_block(cpu, mem, &block.insns, block.fetch_fault, env.trace);
    if exit == BlockExit::Flush { self.flush(); }
    if let Err(stop) = deliver(exit, env, cpu) { return stop; }
}
```

`exec_block` calls `step()` for each instruction; `step()` is a big `match` over `Inst`. File 06.

---

## 14. System mode in brief (`bridgev boot`)

```
main() → run_boot()                                      src/main.rs
   ├─ a host thread reads stdin into the UART's receive queue
   └─ machine::boot()                                    src/system/machine.rs
        ├─ read kernel Image, map 512 MiB RAM at 0x8000_0000, copy the kernel in
        ├─ generate the devicetree (fdt.rs), load initrd, (optional firmware)
        ├─ add devices: CLINT, PLIC, UART, syscon, (virtio-blk)
        ├─ create harts: CpuState in S-mode at the kernel entry, a0 = hart id, a1 = DTB address
        └─ loop {                                         ← the "machine loop"
              devices → mip bits (timer, UART, disk interrupts)
              pick the next runnable hart (round-robin)
              if all harts wait in WFI: sleep until the next timer deadline or keypress
              stop = hart.engine.run(cpu, mem, env, 100_000)   ← same engines as user mode
              match stop {
                  Ecall → sbi::call()   (the guest kernel asked the "firmware" for something)
                  Wfi   → mark the hart waiting
                  Limit → next slice
              }
              if the power-off device was written: stop
           }
```

The engine is exactly the same code. The differences are all in `CpuState` and `Env`: `softmmu = 1` (every memory access goes through the emulated MMU), `user_mode = false` (exceptions trap into the guest kernel instead of stopping), and `sbi = true` (S-mode `ecall`s stop the engine so the Rust SBI can serve them). File 16 covers this in detail.

---

## Check yourself

1. List, in order, the functions called between `main()` and the first byte of generated code executing.
2. What does `loader::load()` produce, and what are the steps in building the initial stack?
3. What does `Engine::run()` return, and who handles each kind of `Stop`?
4. In `tb_for_key()`, what happens if the code buffer is full? If a block needs too many spill slots?
5. What does the exit code in RAX contain, and how does `exec` use it?
6. Walk through a `write` system call from the `ecall` instruction to the next guest instruction.
7. What is different between the user-mode flow and the system-mode flow? What is the same?
