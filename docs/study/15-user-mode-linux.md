# 15 · User mode: running RISC-V Linux programs

## What you will learn

- what "user-mode emulation" means, and what Bridge-V must imitate about Linux
- the ELF loader and the exact layout of a new process's stack
- how system calls are handled: pass-through, pointer checks, struct conversion, emulated state
- memory syscalls: `brk`, `mmap`, `munmap`, `mprotect`
- threads: one host thread per guest thread, with a fair global lock
- guest signals: handlers, signal frames and `sigreturn`
- dynamically linked programs (`ld.so`), and the GDB debugger stub

Files: [`src/user/`](../../src/user/) and [`src/elf.rs`](../../src/elf.rs).

---

## 1. The job

In user mode, Bridge-V runs **one** Linux program. There is no guest kernel. Bridge-V itself plays the role of the RISC-V Linux kernel, for that one process:

| A real kernel would… | Bridge-V does… |
|---|---|
| load the ELF file into a fresh address space | `loader::load()` |
| set up the stack with arguments and environment | `build_stack()` |
| handle `ecall` system calls | `Syscalls::dispatch()` |
| manage the heap and memory mappings | `brk`, `mmap`, `munmap`, `mprotect` over `DirectMem` |
| run threads | `user::thread` |
| deliver signals (segfaults, `raise`, `abort`) | `user::guest_signal` |

Most syscalls can be forwarded to the real x86-64 Linux kernel, because Linux's system-call *meanings* are the same on every architecture. Only the numbers, registers and some structure layouts differ.

---

## 2. Loading an ELF file

### 2.1 Parsing ([`src/elf.rs`](../../src/elf.rs))

`Elf::parse()` checks the header: `\x7fELF` magic, 64-bit (`ELFCLASS64`), little-endian, machine = 243 (`EM_RISCV`), type = executable (`ET_EXEC`) or position-independent (`ET_DYN`). It reads the **program headers**; the important kind is `PT_LOAD` (a segment to map: virtual address, file bytes, memory size, flags R/W/X). `PT_INTERP` names the dynamic loader. Every read is bounds-checked, so a corrupt file gives an error, never a crash. (A test feeds it random bytes.)

### 2.2 Mapping (`map_segments()`)

- A static program is loaded at the addresses it asks for (usually starting at `0x10000`).
- A position-independent executable (PIE) is loaded at `0x2a_aaaa_a000` (Linux's `ELF_ET_DYN_BASE` for riscv64).
- Two segments may share a page at their boundary; that page gets the **union** of their permissions.
- If a segment's memory size is bigger than its file size (the `.bss`: zero-initialised globals), the extra part stays zero, because `mmap`'d anonymous memory is zero-filled.

### 2.3 The address-space layout

```
0x3f_ffff_f000  ┌──────────────────────┐  STACK_TOP: the signal trampoline page starts here
                │ stack (8 MiB) ↓       │  from STACK_TOP − 8 MiB up to STACK_TOP
                ├──────────────────────┤
                │         …            │
0x3f_8000_0000  │ ld.so (dynamic only) │  INTERP_BASE
0x3f_0000_0000  │ mmap area ↓           │  MMAP_TOP: new mappings grow downward
                │         …            │
                │ heap (brk) ↑          │  grows upward from the end of the program
0x2a_aaaa_a000  │ PIE programs         │  ET_DYN_BASE
0x0001_0000     │ static programs      │
0               └──────────────────────┘
```

This mirrors what Linux does on a real RISC-V machine with Sv39, so programs see familiar addresses.

### 2.4 The initial stack (`build_stack()`)

When a Linux program starts, `sp` points at this structure (from low to high addresses):

```
sp →  argc                      e.g. 2
      argv[0]  ─────────────────┐ pointers to the strings at the top
      argv[1]  ───────────────┐ │
      NULL                    │ │
      envp[0] … envp[n−1]     │ │ (pointers)
      NULL                    │ │
      auxv: (AT_PHDR, …)      │ │ pairs of (type, value)
            (AT_PAGESZ, 4096) │ │
            (AT_ENTRY, entry) │ │
            (AT_RANDOM, ptr)  │ │ …16 entries in all
            (AT_NULL, 0)      │ │
      …                       │ │
      16 random bytes         │ │ (for stack-protector canaries)
      "20\0"  ◄───────────────┘ │
      "fib-O2.elf\0"  ◄─────────┘
      "/abs/path/fib-O2.elf\0"  (AT_EXECFN)
STACK_TOP
```

The **auxiliary vector** ("auxv") passes information from the kernel to the C library's start-up code: where the program headers are (`AT_PHDR`), the page size, the entry point, the user and group ids, which ISA extensions exist (`AT_HWCAP`: bits for I, M, A, F, D, C), a pointer to random bytes, and for dynamic programs where `ld.so` was loaded (`AT_BASE`). `sp` is rounded down to a 16-byte boundary, as the ABI requires.

---

## 3. System calls

### 3.1 The ABI

The guest puts the syscall number in `a7`, arguments in `a0`–`a5`, and executes `ecall`. The result comes back in `a0`; errors are negative `errno` values (e.g. −2 = `ENOENT`). Numbers come from Linux's "asm-generic" table, e.g. 63 = `read`, 64 = `write`, 93 = `exit`, 94 = `exit_group`, 214 = `brk`, 222 = `mmap`.

### 3.2 The path through the code

`ecall` ends the translation block → exit reason ECALL → `Jit::run` returns `Stop::Ecall` → `guest_thread()` calls `Syscalls::dispatch()` → a big `match nr { … }` in `handle()` → the result goes into `a0`, `pc += 4` (an `ecall` is never compressed), `icount += 1`. See file 05, §10.

### 3.3 Four kinds of syscall handling

**1. Pure pass-through** (integer arguments only): `close`, `lseek`, `dup`, `getpid`, `getuid`, … The arguments go straight to the host function.

**2. Pass-through with pointer translation**: `read`, `write`, `openat`, `getcwd`, `clock_gettime`, … Guest pointers are **checked and translated** first:
```rust
fn gptr(p: &Process, addr: u64, len: u64, need: u8) -> Result<*mut c_void, i64> {
    p.mem.slice(GuestVirt(addr), len, need)      // every page mapped, with permission `need`?
        .map(|s| s.as_ptr() as *mut c_void)       // yes: host pointer = base + addr
        .map_err(|_| -EFAULT)                     // no: return -EFAULT like the kernel would
}
```
This matters for safety: a buggy or malicious guest passing a garbage pointer gets `EFAULT`; it can never make Bridge-V read or write its own memory.

**3. Structure conversion.** Most kernel structures (`timespec`, `iovec`, `utsname`, `rlimit`, `statx`, `dirent64`) have the same layout on riscv64 and x86-64. `struct stat` does **not**: riscv64 uses the 128-byte asm-generic layout, x86-64 a 144-byte one with fields in a different order. So `fstat`/`newfstatat` call the host, then `stat_to_guest()` copies the fields one by one into the guest layout.

**4. Emulated in Bridge-V.** Some calls concern the emulated process itself, not the host:
- `exit`, `exit_group` → end the thread / process
- `brk`, `mmap`, `munmap`, `mprotect` → change `DirectMem` (section 4)
- `set_tid_address`, `clone`, `futex` → threads (section 5)
- `rt_sigaction`, `rt_sigprocmask`, `rt_sigreturn`, `kill`, `tgkill`, `sigaltstack` → guest signals (section 6)
- `uname` → reports machine `riscv64`
- `readlinkat("/proc/self/exe")` → the guest program's path, not Bridge-V's
- `riscv_flush_icache` (259) → like `FENCE.I` (file 13)
- `rseq`, `clone3`, `riscv_hwprobe` → `-ENOSYS`, on purpose: glibc falls back to older methods

Anything unknown logs a message once and returns `-ENOSYS` ("not implemented"). About 60 syscalls are supported in total. `--strace` prints every syscall with its arguments and result.

---

## 4. Memory syscalls

- **`brk(addr)`** moves the end of the heap. Growing maps new zeroed pages (only if the range is free); shrinking unmaps them. `malloc` uses it for small allocations.
- **`mmap(addr, len, prot, flags, fd, off)`**: with `MAP_FIXED`, use exactly `addr`; otherwise `find_free()` searches **downward from `0x3f_0000_0000`** for a free range. Anonymous mappings are zero-filled pages; file mappings are filled by copying the file's contents (private-mapping semantics).
- **`munmap`** unmaps and **flushes the engine's translations**, because code in the removed range may have been translated.
- **`mprotect`** changes guest and host permissions.

In direct mode these go straight to `DirectMem::map/unmap/protect`, which update both the guest permission table and the host page protection (file 11). With `--mem=softmmu`, the TLB is flushed after each of them, since the "page table" (the mapping state) changed.

---

## 5. Threads ([`src/user/thread.rs`](../../src/user/thread.rs), decision D55)

`pthread_create` in glibc calls `clone(CLONE_VM | CLONE_THREAD | …, stack, …)`. Bridge-V handles it by:
1. copying the calling thread's `CpuState` (a thread starts with the same registers)
2. setting the child's `a0 = 0`, `sp = the new stack`, `tp = the new TLS pointer`, `pc` past the `ecall`
3. writing the new thread id where `CLONE_PARENT_SETTID`/`CLONE_CHILD_SETTID` ask
4. starting a **new host thread** that runs `guest_thread()` with its **own engine and translation cache**

### 5.1 One thread at a time: the fair lock

All guest threads share one `DirectMem` and one `Syscalls` state. To keep this simple and correct, only **one** guest thread executes guest code at a time, protected by a **GIL** (global lock):
- a thread holds it for a **slice** (10 million instructions when there are other threads) or until its next syscall
- the lock is a **ticket lock**: each waiter takes a number and waits for its turn, so a thread that releases and immediately asks again goes to the back of the queue. Threads take fair turns.

```rust
pub fn lock(&self) -> GilGuard<T> {
    let mut t = self.turn.lock();
    let me = t.0;  t.0 += 1;                         // take the next ticket
    while t.1 != me { t = self.cv.wait(t); }         // wait until it is served
    ...
}
// on unlock: t.1 += 1; notify_all()                 // serve the next ticket
```

**Consequences:**
- guest atomics (`amoadd`, `lr`/`sc`) are automatically atomic, with no host atomic instructions needed
- a thread switch clears the LR reservation (an `sc` may fail spuriously, which RISC-V allows)
- no parallel speed-up: multi-threaded CoreMark runs at about 88% of single-thread throughput, while QEMU runs threads in parallel (38,314 it/s). Parallel execution would need a shared, synchronized translation cache and atomic AMOs: listed as future work.

### 5.2 Blocking syscalls and futexes

A thread waiting in `futex(WAIT)` or `nanosleep` must not hold the lock, or the thread that would wake it could never run. The syscall layer returns `SysOut::Block(nr, args)`; `guest_thread` releases the lock, calls the host syscall directly, then takes the lock again.

**Futexes** (the building block of mutexes and condition variables) work directly on the host: guest memory *is* host memory (at `base + addr`), so a guest futex word is a real host address, and the host kernel's `futex` does the waiting and waking.

When a thread exits, Bridge-V zeroes its `CLEARTID` word and does a host `FUTEX_WAKE` on it. That is how `pthread_join` learns the thread finished.

### 5.3 Keeping per-thread caches coherent

- If one thread writes code, `DirectMem::smc_epoch` changes; other threads flush their translation caches when they next run (file 13).
- With `--mem=softmmu`, if one thread changes the mappings, the others flush their TLBs.

---

## 6. Guest signals ([`src/user/guest_signal.rs`](../../src/user/guest_signal.rs), decision D56)

A C program can install a handler with `sigaction(SIGSEGV, …)` and catch its own crashes, or call `raise`/`abort`. Bridge-V delivers signals the way Linux does on riscv64:

1. **Sources:** synchronous faults (a guest exception becomes SIGSEGV, SIGBUS, SIGILL or SIGTRAP) and signals the process sends itself (`kill`, `tkill`, `tgkill`).
2. **If a handler is installed and the signal isn't blocked:** build an `rt_sigframe` on the guest stack (or the alternate stack from `sigaltstack`): a `siginfo_t` (128 bytes) plus a `ucontext` holding all the registers `pc, x1…x31`, the FP registers and `fcsr`, and the old signal mask, 1,088 bytes in total, in exactly the kernel's layout.
3. Set `a0 = signo`, `a1 = &siginfo`, `a2 = &ucontext`, `ra = the sigreturn trampoline`, `pc = the handler`.
4. When the handler returns, it "returns" to the **trampoline page** (mapped by the loader just above the stack) that does `li a7, 139; ecall` = `rt_sigreturn`. Bridge-V restores every register, the FP state and the mask from the frame, so execution continues exactly where it was interrupted (or wherever the handler changed `pc` to, for `siglongjmp`).
5. **If there's no handler** (or the signal is blocked or ignored for a fault), the process dies, and Bridge-V exits with 128 + signal number, just like a shell reports a crash.

Asynchronous host signals (like Ctrl+C) are not forwarded to the guest.

---

## 7. Dynamically linked programs (decision D57)

Most real programs are dynamically linked: the executable contains only its own code, and `ld.so` (the dynamic loader) loads `libc.so` and friends at start-up. For these, Bridge-V:
1. loads the program at `ET_DYN_BASE` if it is position-independent
2. reads its `PT_INTERP` (e.g. `/lib/ld-linux-riscv64-lp64d.so.1`), loads it from the **sysroot** (`--sysroot`, default `/usr/riscv64-linux-gnu`, where Ubuntu's cross-compiler installs the RISC-V libraries) at `0x3f_8000_0000`
3. starts execution in `ld.so`, with `AT_BASE`, `AT_PHDR` and `AT_ENTRY` telling it where the program is
4. redirects absolute paths in `openat`, `faccessat`, `newfstatat`, `readlinkat` and `statx` to the sysroot first, so `ld.so` finds the RISC-V `libc.so.6` and not the host's x86 one (like QEMU's `-L`)
5. supports file-backed `mmap` (how `ld.so` maps libraries) and `pread64`

---

## 8. The GDB stub (decision D59)

`bridgev run --gdb 1234 prog.elf` waits for a debugger on `127.0.0.1:1234` and speaks the **GDB Remote Serial Protocol** ([`src/user/gdb.rs`](../../src/user/gdb.rs)). With `gdb-multiarch -ex 'target remote :1234'` you can read registers and memory, set breakpoints, single-step and continue.

Under the debugger every instruction runs through the interpreter as a one-instruction block. That makes single-stepping exact and breakpoints trivial (no code patching needed: "continue" just steps until `pc` equals a breakpoint address). Debugging speed isn't a goal.

---

## 9. Correctness check

Every guest test program ([`guest/c/`](../../guest/c/): hello, printf with floats, malloc, qsort, setjmp, strings, stat, signals, threads, self-modifying code) is built in several variants (`-O0`/`-O2`, with/without compressed instructions, static and dynamic). [`tests/user_programs.rs`](../../tests/user_programs.rs) runs each one under every engine and both memory backends and requires **byte-identical stdout and the same exit code** as `qemu-riscv64` produced when the reference outputs were recorded.

---

## Check yourself

1. What parts of a Linux kernel does Bridge-V imitate in user mode?
2. Draw the initial stack of a program started with two arguments. What is the auxiliary vector for?
3. Why can most syscalls be passed straight to the host? Name one that can't, and why.
4. What does `gptr()` protect against?
5. Where does `mmap` put a new mapping when no address is requested?
6. Why does Bridge-V run only one guest thread at a time? What does that give for free, and what does it cost?
7. Why must blocking syscalls run without the lock?
8. Describe what happens when a guest program with a SIGSEGV handler dereferences a bad pointer.
9. How does a dynamically linked program find the RISC-V `libc` instead of the host's?
