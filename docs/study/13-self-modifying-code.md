# 13 · Self-modifying code: when the program rewrites itself

## What you will learn

- what self-modifying code (SMC) is and why real programs do it
- why SMC is dangerous for a translator (stale translations)
- how Bridge-V tracks which pages hold translated code
- how a write to such a page is detected, in both memory backends
- how the stale translations are thrown away safely (invalidate + unlink)
- what `FENCE.I` is, and why Bridge-V makes it cheap

Design decision D49 (Phase 8). The logic is spread over several files; [`src/mem/smc.rs`](../../src/mem/smc.rs) is a comment that lists them.

---

## 1. The problem

A translator keeps translations of code it has already seen. That assumes the code doesn't change. But sometimes it does:

- **JIT compilers inside the guest** (a JavaScript engine, the Java VM) write new machine code and run it.
- **Loaders** (the Linux kernel, `ld.so`, `dlopen`) copy code into memory, sometimes over memory that held other code before.
- **Patching**: debuggers inserting breakpoints, kernels patching themselves at boot.

If the guest overwrites instructions that Bridge-V already translated, and Bridge-V keeps running the old translation, the program runs **stale code**, with wrong results and no error. A correct translator must notice every write to translated code and throw away the affected translations.

RISC-V officially says: after writing code, execute `FENCE.I` before running it; only then must the new code be visible. But x86-style programs (and some buggy ones) don't always do that, so Bridge-V handles writes **eagerly**, whether or not `FENCE.I` follows.

---

## 2. The approach: code pages and eager invalidation

Bridge-V works at the granularity of **4 KiB pages**:

1. **Mark.** When an engine decodes or translates code from a page, that page is marked as a **code page**.
2. **Detect.** Any write to a code page is detected (section 3).
3. **Report.** The write path removes the mark and adds the page to `DirectMem::smc_pages`.
4. **Invalidate.** Before running anything else, the engine throws away every translation made from that page.
5. **Stop.** The engine stops **right after** the store, so even the rest of the currently running block (which may be stale) doesn't execute.

Pages that contain only data are never marked, so programs that don't modify code pay nothing. CoreMark has **zero** code-page writes.

---

## 3. Detecting the write

### 3.1 Marking: `DirectMem::mark_code()`

```rust
pub fn mark_code(&mut self, addr: u64) -> bool {
    let page = addr / PAGE_SIZE;
    let b = self.prot.get(page);
    if b & MAPPED == 0 || b & CODE != 0 { return false; }   // unmapped, or already marked
    self.prot.set(page, b | CODE);
    if b & prot::W != 0 {                                    // guest-writable code page:
        // make the HOST page read-only, so a JIT store to it faults
        self.set_host_prot(page_start, page_end, host_prot(b & RWX & !W));
    }
    true
}
```

The JIT calls it (through `mark_new_code()`) for the page(s) of every new TB; the interpreter calls it (through `mark_block_code()`) for every newly decoded block.

### 3.2 Every write path checks the mark

Every way to write guest memory goes through `uncode_range()` / `uncode()`: `store()`, `slice_mut()` (used by syscalls like `read`), `write_bytes()` (the loader), and `map`/`unmap`/`protect` (remapping counts as a change too).

```rust
fn uncode(&mut self, page: u64) {
    if self.prot.get(page) & CODE == 0 { return; }
    self.prot.set(page, b & !CODE);                   // no longer a code page
    restore host write permission if the guest may write
    self.smc_pages.push(page * PAGE_SIZE);            // report it
    self.smc_epoch += 1;                              // for other threads' engines
    self.smc_log.push(page * PAGE_SIZE);              // for other harts in system mode
}
```

### 3.3 Detection in each engine

| Where the store happens | How it is detected |
|---|---|
| the interpreter (`step`) | `mem.store()` reports the page; `exec_block` sees `smc_pages` is non-empty and stops the block right after the store |
| an `Interp` helper call in JIT code | the same `mem.store()`; `helper_interp_one` sees it and exits with reason `SMC` (pc = next instruction, store retired) |
| a JIT store, **direct mode** | the host page is read-only, so the store raises **SIGSEGV**. `exec()` checks `is_smc_fault()`: "is the fault address a code page that the guest is allowed to write?" If yes, `smc_host_fault()` rebuilds the state at the store (like a normal fault, file 12), then runs **just that one store in the interpreter**, which unprotects the page and reports it. Execution continues after the store. |
| a JIT store, **softmmu** | at TLB fill time, the write tag of a code page gets the `TLB_CODE` flag (file 11, §5.4), so the inline check fails and the store takes the slow path. `helper_mmu_access` performs the store, which reports the page, and returns `exit_reason = SMC_STORE`; the dispatcher applies the fault site's state map and continues after the store. |

---

## 4. Invalidating: `Jit::drain_smc()`

The JIT keeps `page_tbs: page → [TB ids]`, filled in `tb_for_key()` whenever a TB is created. Draining the reported pages:

```rust
pub fn drain_smc(&mut self, cpu, mem) {
    for &p in &mem.smc_pages {
        for id in self.page_tbs.remove(&p).unwrap_or_default() {
            if !self.cache.get(id).valid { continue; }
            chain::unlink_incoming(&mut self.cache, &mut self.cm, id);  // point predecessors back at their stubs
            self.cache.invalidate(id);                                  // remove from the pc → TB map
            if last_exit came from id { self.last_exit = None; }        // don't link from a dead TB
        }
    }
    forget_smc_pages(cpu, mem);    // clear the list (and the TLB_CODE flags in softmmu)
    self.jc_version += 1;          // the jump cache may point at dead TBs: make it stale
}
```

It runs at the start of `select()` and after every `exec()`, so no stale TB can run after the write is noticed.

Order matters:
1. **Unlink first**: any block that jumps directly into a dead TB gets its jump reset to its exit stub (file 10, §6). Otherwise a chained jump could still reach the stale code.
2. **Then invalidate**: the next lookup for that pc misses and retranslates the new code.
3. **Stale the jump cache**: bumping the version makes the next `exec()` clear the table, since it may hold the dead TB's host address.

The dead TB's bytes stay in the code buffer (bump allocation never frees) until the next full flush. That is safe, because nothing can reach them any more.

The interpreter's version is simpler: it drops its whole decoded-block cache (`self.flush()`).

---

## 5. A block that modifies itself

What if a store overwrites an instruction **later in the same block** that is running right now? Because every engine **stops right after any store to a code page** (not only stores to the current block), the rest of the block never runs. Execution resumes at the next instruction, which is looked up again and freshly decoded from the new bytes. This is stricter than RISC-V requires (RISC-V only promises visibility after `FENCE.I`), but it is simple and always correct.

---

## 6. `FENCE.I` and `riscv_flush_icache`

`FENCE.I` is RISC-V's "I have modified code, make it visible" instruction. User programs can also call the `riscv_flush_icache` system call (number 259), and a guest kernel sends remote `FENCE.I` requests to other CPUs through the SBI.

Because Bridge-V already invalidates eagerly, there is nothing stale left by the time `FENCE.I` runs. So `Jit::fence_i()` only resets the jump cache (cheap). Before Phase 8, `FENCE.I` flushed the entire translation cache, which was expensive for programs that execute it often.

For debugging there is `--smc=flush-on-fence`, which goes back to flushing everything on `FENCE.I`. If a program behaves differently with and without it, the eager tracking has a bug.

---

## 7. Multiple threads and harts

- **User-mode threads** (file 15) each have their own JIT and translation cache. A code write by one thread bumps `DirectMem::smc_epoch`; when another thread next takes the lock and sees a different epoch, it flushes its whole cache.
- **System-mode harts** (SMP, file 16) each have their own engine too. `DirectMem::smc_log` records every code-page write in order; before a hart runs, the machine gives its engine the pages written since it last ran.

---

## 8. Testing

- [`guest/c/smc.c`](../../guest/c/smc.c): a C program that patches its own code (with and without `FENCE.I`) and must produce the same output as QEMU under every engine and both memory backends.
- [`tests/softmmu.rs`](../../tests/softmmu.rs): one of the random page kinds is a writable alias of the block's own code page, so the fuzzer produces self-modifying stores and checks both engines stop and re-decode identically.
- Lockstep mode re-marks pages the reference run wrote before the JIT run, so both runs see the same code-page state.

---

## Check yourself

1. Give two real situations where a program writes code and then runs it.
2. What goes wrong if a translator ignores self-modifying code?
3. How does Bridge-V know which translations came from a given page?
4. How is a JIT store to a code page detected in direct mode? In softmmu?
5. Why must incoming chains be unlinked before a TB is invalidated?
6. Why is it safe to leave a dead TB's bytes in the code buffer?
7. What happens if a store overwrites a later instruction in the same block?
8. Why is `FENCE.I` cheap in Bridge-V, and what does `--smc=flush-on-fence` do?
