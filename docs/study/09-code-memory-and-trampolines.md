# 09 · Code memory and trampolines: running bytes you just wrote

## What you will learn

- how a program can execute code it generated at run time
- **W^X** ("write xor execute") and why memory must never be writable and executable at once
- Bridge-V's **dual mapping** trick, and the slower `mprotect` alternative
- how the code buffer is allocated (bump pointer, alignment, flush)
- the three **trampolines**: `enter_jit`, `exit_jit`, `fault_exit`
- the **block-boundary ABI**: the rules every translated block must follow
- how generated code calls back into Rust (the helper table)

Files: [`src/jit/code_mem.rs`](../../src/jit/code_mem.rs), [`src/jit/trampoline.rs`](../../src/jit/trampoline.rs).

---

## 1. The basic trick

Every JIT does this:

```rust
// 1. get memory                  2. write machine code into it
// 3. make it executable          4. call it like a function
let f: extern "sysv64" fn() -> u64 = unsafe { std::mem::transmute(address) };
let result = f();
```

The smallest example in the project is the test `hello_jit_dualmap` in `code_mem.rs`:

```rust
let mut a = Asm::new(cm.next_addr());
a.mov_r32_imm(Reg::Rax, 42);     // B8 2A 00 00 00   mov eax, 42
a.ret();                         // C3               ret
let addr = cm.place(origin, &a.finish()).unwrap();
let f: extern "sysv64" fn() -> u64 = unsafe { std::mem::transmute(addr as *const u8) };
assert_eq!(f(), 42);             // the CPU ran our 6 bytes
```

`transmute` reinterprets the address as a function pointer. This is `unsafe` because Rust can't check that the bytes really are a valid function; Bridge-V's tests and design have to guarantee it.

---

## 2. W^X: never writable and executable at the same time

If some memory is both writable and executable, any bug that lets an attacker write bytes there lets them run their own code. So security-conscious systems enforce **W^X**: a page may be writable **or** executable, never both. Some hardened kernels refuse RWX mappings entirely.

A JIT needs to *write* code and then *execute* it, and for chaining it must even *rewrite* code that has already run. Bridge-V solves this without ever creating an RWX page, and a test (`rwx_mappings()` in `code_mem.rs`) checks `/proc/self/maps` to prove it.

### 2.1 Dual mapping (the default, decision D6)

The same physical memory is mapped at **two different virtual addresses**:

```rust
let fd = memfd_create("bridgev-jit", MFD_CLOEXEC);      // an anonymous in-memory file
ftruncate(fd, SIZE);                                    // make it SIZE bytes long
let rw = mmap(NULL, SIZE, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);  // view 1: writable
let rx = mmap(NULL, SIZE, PROT_READ | PROT_EXEC,  MAP_SHARED, fd, 0);  // view 2: executable
```

```
         ┌──── RW view @ 0x7f..A000 ────┐          ┌──── RX view @ 0x7f..B000 ────┐
Rust ───►│ write / patch code here      │   same   │ the CPU executes from here    │
         └──────────────────────────────┘  pages   └───────────────────────────────┘
                      │                                          ▲
                      └──────────── physical memory ─────────────┘
```

- Code is **written and patched** through `rw`.
- Code is **executed** through `rx`.
- Each view on its own is W^X-legal.
- There is **no `mprotect` system call per block**, so translation and chain patching stay cheap.

**Important:** all addresses handed out (`next_addr()`, `place()`) are **RX-view addresses**, and all jump math uses them, because that is where the code actually runs. Writes translate an RX address to an offset and write at `rw + offset`.

x86 keeps its instruction cache coherent with data writes automatically, so no "flush the instruction cache" step is needed (ARM hosts would need one).

### 2.2 The `mprotect` alternative (`--wx=mprotect`)

One private mapping, normally read+execute. To write, the pages are switched to read+write with `mprotect`, the bytes copied, then switched back. It is simpler but costs two system calls per write (including every chain patch), so it is slower. It exists for comparison and for systems where `memfd_create` isn't allowed.

---

## 3. Allocation: a bump pointer

```rust
pub fn place(&mut self, origin: u64, code: &[u8]) -> Result<u64, Full> {
    assert_eq!(origin, self.next_addr());           // code was assembled for this address
    let off = self.used.next_multiple_of(16);        // each TB starts 16-byte aligned
    if off + code.len() > self.size { return Err(Full); }
    self.write_at(off, code);
    self.used = off + code.len();
    Ok(origin)
}
```

- **Bump allocation**: new code always goes right after the previous code. There is no "free" for a single TB. This keeps allocation to a few instructions, and it means TB addresses increase with TB id, so "which TB contains this address?" is a binary search (`TbCache::find_host`).
- **16-byte alignment** of each TB start helps the CPU's instruction fetch.
- **Why assemble for a known address?** The code contains relative jumps to the trampolines and absolute references to the helper table. `Asm::new(origin)` is told the final address up front, so those are correct from the start.
- **When full**: `flush_all()` resets `used` to the end of the trampolines (`prefix`), forgets every TB, bumps a generation counter, and invalidates the jump cache. Old bytes stay in memory but are overwritten by later code. A flush happens only from the dispatcher, when no generated code is running.
- **Size**: default 256 MiB (`--code-cache`), maximum 1 GiB, so every `rel32` jump inside the buffer can reach every other byte (±2 GiB).

### 3.1 Patching a jump: `patch_rel32`

```rust
pub fn patch_rel32(&mut self, field: u64, target: u64) {
    assert!(field % 4 == 0, "unaligned rel32 field");
    let rel = target.wrapping_sub(field + 4) as i64;          // relative to the next instruction
    assert!(rel == rel as i32 as i64, "rel32 out of range");
    // one aligned 32-bit atomic store through the RW view
    AtomicU32::from_ptr(self.rw.add(off) as *mut u32).store(rel as u32, Ordering::Release);
}
```

For a `jmp rel32` or `jcc rel32`, the rel32 field is the **last 4 bytes** of the instruction, so "the next instruction" starts at `field + 4`. That is why the formula is `target − (field + 4)` whatever the opcode's length. File 10 explains why the field must be 4-byte aligned.

---

## 4. The three trampolines

A **trampoline** is a small piece of glue code. `Trampolines::generate()` writes these at the very start of the buffer, then `seal_prefix()` protects them from flushes:

```
B+0x000  helper table: [0] &helper_interp_one  [1] &helper_mmu_access   (8 bytes each)
B+0x010  enter_jit
B+0x050  exit_jit
B+…      fault_exit
B+…      first TB …
```

### 4.1 `enter_jit(cpu, tb_address)`: from Rust into generated code

It is called from Rust as a normal System V function with `RDI = &CpuState` and `RSI = TB address`:

```
enter_jit:
    push rbp ; push rbx ; push r12 ; push r13 ; push r14 ; push r15   ; save callee-saved regs
    sub  rsp, 8                  ; stack alignment: entry RSP ≡ 8 (mod 16); +6 pushes +8 → ≡ 0
    mov  dword [rsp], 0x1F80     ; the default MXCSR value (FP control/status)
    ldmxcsr [rsp]                ; clean FP rounding mode and flags for guest FP code
    lea  rbp, [rdi + 128]        ; RBP = &CpuState + 128 (biased so x[0..31] fit in disp8)
    mov  rbx, [rbp + mem_base]   ; RBX = host address of guest address 0
    mov  r12, [rbp + x2]         ; the four pinned guest registers
    mov  r13, [rbp + x1]
    mov  r14, [rbp + x10]
    mov  r15, [rbp + x15]
    mov  r9,  [rbp + budget]     ; the instruction budget
    jmp  rsi                     ; into the TB
```

Why save six registers? The System V ABI says a function must preserve RBX, RBP, R12–R15. Generated code uses all of them, so `enter_jit` saves the caller's values and `exit_jit` restores them. To Rust, the whole journey through generated code looks like one ordinary function call.

### 4.2 `exit_jit`: back to Rust

Every block exit (a stub) puts an **exit code** in RAX and jumps here:

```
exit_jit:
    mov  [rbp + x2], r12  ...  mov [rbp + x15], r15   ; write pinned registers home
    mov  [rbp + budget], r9                          ; save the remaining budget
    (read MXCSR, turn x86 FP exception flags into RISC-V fflags, OR them into cpu.fflags)
    add  rsp, 8
    pop  r15 ; pop r14 ; pop r13 ; pop r12 ; pop rbx ; pop rbp
    ret                                              ; returns RAX to the Rust caller
```

The **exit code** is `(tb_id << 2) | slot`:
- `slot` 0 or 1: a **direct exit** (fall-through or taken branch) that isn't linked yet. The dispatcher may chain it (file 10).
- `slot` 2: a **special exit**; the reason is in `cpu.exit_reason`.

Using a TB *index* instead of a pointer (decision D25) means the value can be bounds-checked and can never dangle after a flush.

### 4.3 `fault_exit`: after a crash

When generated code touches bad memory, the host sends SIGSEGV. The signal handler (file 12) saves the registers and changes the interrupted instruction pointer to `fault_exit`:

```
fault_exit:
    mov  dword [rbp + exit_reason], HOST_FAULT
    mov  eax, 2                  ; slot 2 = special
    jmp  exit_jit
```

This works because generated code **never pushes anything on the stack**: RSP is still exactly what `enter_jit` left, so `exit_jit`'s pops restore everything correctly.

---

## 5. Exit reasons

`CpuState.exit_reason` (defined in `exit` in [`src/cpu/state.rs`](../../src/cpu/state.rs)):

| Value | Name | Meaning |
|---:|---|---|
| 0 | NONE | normal exit; continue at `cpu.pc` |
| 1 | ECALL | an `ecall` at `pc` (not yet retired) |
| 2 | EXCEPTION | exception `exc_cause` / `exc_tval` at `pc` |
| 3 | FLUSH | `fence.i` retired |
| 4 | HOST_FAULT | SIGSEGV inside generated code (`fault_rip`, `fault_addr`) |
| 5 | BUDGET | a block's prologue found the budget used up; nothing ran |
| 6 | LOOKUP | an indirect jump's target wasn't in the jump cache |
| 7 | FP_VARIANT | a fast-FP block was entered in the wrong FP state; nothing ran |
| 8 | MMU_FAULT | a softmmu slow path raised a page/access fault |
| 9 | SMC | a helper's store hit a code page |
| 10 | SMC_STORE | the same, from a softmmu inline store |
| 11 | WFI | `wfi` with nothing pending: the machine may sleep |

The dispatcher sets `exit_reason = NONE` before every entry, and `exec()` in `dispatch.rs` turns the reason into a `BlockExit`.

---

## 6. The block-boundary ABI (the rules every TB obeys)

At **every** entry to and exit from a block, including chained jumps and jump-cache jumps (CLAUDE.md §8.3):

1. RBP = `&CpuState + 128`; RBX = guest memory base (direct mode).
2. The pinned guest registers are **only** in R12–R15. Their `CpuState` slots are stale until `exit_jit` writes them.
3. Every other guest register is up to date in `CpuState`.
4. No values are held in pool or scratch registers.
5. RSP is exactly what `enter_jit` left (so it is 16-byte aligned for calls).

Inside a block anything goes (values cached in registers, dirty), but every path out of the block restores these rules first. Because every block starts and ends in the same known state, **any block can jump directly into any other block**. That is what makes chaining possible.

---

## 7. Calling Rust from generated code

Generated code calls two Rust functions:
- `helper_interp_one(cpu, raw, pc)`: run one instruction in the interpreter (file 07, §5.4)
- `helper_mmu_access(cpu, va, info, val)`: the softmmu slow path (file 11)

Rust functions may be more than 2 GiB away from the code buffer, beyond `call rel32`'s reach. So their absolute addresses sit in the **helper table** at the start of the buffer, and the code calls them with `call [rip + disp32]` (`FF 15` + disp32): "call the address stored at this nearby location".

Rules for these helpers:
- `extern "sysv64"`: they use the calling convention the generated code expects.
- **They must never unwind (panic) into generated code**, because there are no Rust unwind tables for it. Each helper wraps its body in `catch_unwind` and calls `abort()` if something panics. The release build also uses `panic = "abort"`.
- They get `&CpuState` from `RBP − 128`, and guest memory from `cpu.helper_mem` (a raw pointer the dispatcher sets before each entry).

---

## 8. Floating-point state at the boundary

x86 keeps FP rounding mode and exception flags in the **MXCSR** register. Rust code assumes the default MXCSR, and guest FP code accumulates exception flags there. So:
- `enter_jit` loads the default MXCSR (round-to-nearest, all flags clear).
- `exit_jit` reads MXCSR, maps x86's flags (IE, ZE, OE, UE, PE) to RISC-V's (NV, DZ, OF, UF, NX), and ORs them into `cpu.fflags`.
- `helper_interp_one` does the same before and after running an instruction.

File 14 has the details.

---

## Check yourself

1. What does W^X mean, and why is RWX memory dangerous?
2. Explain the dual mapping. Which view do you write through, which do you execute, and which addresses does the jump math use?
3. Why is code allocated with a bump pointer, and what happens when the buffer is full?
4. Why does `patch_rel32` compute `target − (field + 4)`?
5. What does `enter_jit` do, instruction by instruction? Why push six registers?
6. What is in RAX when generated code returns, and why a TB index instead of a pointer?
7. List the five block-boundary rules. Why do they make chaining possible?
8. Why does `fault_exit` work without knowing where in a block the crash happened?
9. Why can't generated code use `call rel32` to reach Rust helpers?
