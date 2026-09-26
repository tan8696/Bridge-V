# Phase 02 report: Naive JIT

| Field | Value |
|---|---|
| Phase | 02: Naive JIT (see `docs/ROADMAP.md`) |
| Status | **Complete** (deviations noted in §9) |
| Dates | 2026-09-26 → 2026-09-26 |
| Branch / commits | `claude/compassionate-babbage-ul3prl`: `bcddaf0` (emitter, P2.1–P2.2), `a9a140e` (JIT end to end, P2.3–P2.8), `217901d` (tests, lockstep speed-up), plus the commit containing this report |
| CI | Run #12 on `217901d`: **success**, both jobs, <https://github.com/tan8696/Bridge-V/actions/runs/36249804336>. Run #10 (`bcddaf0`) failed on `cargo fmt --check` (§8, bug 1) |
| Sessions used | 1 (continued from Phase 1) |

## 1. Summary
Bridge-V is now a **real dynamic binary translator**. Guest RISC-V basic blocks are compiled into x86-64 machine code by a hand-written encoder, placed in a W^X code buffer, and executed directly on the host CPU.
- Every Phase-1 test passes under the JIT and under the new **lockstep** engine: all 110 riscv-tests and all 31 guest programs, with byte-exact qemu reference output. Lockstep checks the JIT against the interpreter after every translation block.
- CoreMark ran for 266 million TBs under lockstep with no divergence.
- The emitter is verified by **331,560** structural encode→decode checks against `iced-x86`.
- Guest memory faults inside JIT code (host SIGSEGV) are turned back into **precise** guest exceptions: same cause, `tval`, `pc` and instruction count as the interpreter.
- First speedup numbers (§7): the naive JIT is **2.20× the interpreter on CoreMark** (1,121 vs 510 iterations/s, 402 vs 182 guest MIPS) and 1.22× on the 2-instruction `loop.elf`. That loop is dispatcher-bound, which is exactly what Phase 3's chaining removes.

## 2. Planned vs delivered
| Task | Planned | Delivered | Status |
|---|---|---|---|
| P2.1 | x86-64 emitter, labels, golden tests | `backend/x86/{emit,regs}.rs`, 22 golden tests (331,560 checks) | Done (structural rather than text comparison, §9) |
| P2.2 | cpuid features, `--no-host-features` | `backend/x86/features.rs` via `is_x86_feature_detected!` (runs cpuid); flag plumbed | Done (the naive lowering uses no optional feature yet, §10) |
| P2.3 | Dual-mapped code memory, mprotect fallback, hello-JIT, no RWX | `jit/code_mem.rs`, 4 tests incl. both modes and a `/proc/self/maps` RWX check | Done |
| P2.4 | Trampolines, helper table, callee-saved test | `jit/trampoline.rs`: `enter_jit`, `exit_jit`, `fault_exit`, helper table; `asm!` harness test | Done |
| P2.5 | TB cache, block formation, flush-when-full | `jit/cache.rs`; block rules reused from the interpreter's builder with a `--max-block` limit; tests | Done |
| P2.6 | Naive lowering, div guards, MULH*, exits, helper fallback, pcmap | `backend/x86/lower.rs` | Done |
| P2.7 | Dispatcher, SIGSEGV handler | `jit/dispatch.rs`, `user/signal.rs`; faults become precise guest exceptions (more than the planned "report and exit 139") | Done |
| P2.8 | Lockstep engine + broken-lowering test | `jit/lockstep.rs`, hidden `--inject-bug` flag, CLI test | Done |
| P2.9 | `--dump-x86`, `--perf-map`, `--stats` | All three; x86 text disassembly behind the `disasm` cargo feature (D26) | Done |

## 3. What was built

### 3.1 Emitter (`src/backend/x86/emit.rs`, `regs.rs`)
- Typed API with `Reg` (RAX…R15), `Mem { base, index: Option<(Reg, Scale)>, disp }`, `Rm`, `Cond`, `Size`, and `Alu`/`Shift`/`Unary` operation enums. No raw bytes are pushed anywhere outside `emit.rs` (CLAUDE.md §25).
- One core encoder, `emit_op`, handles legacy prefixes, `66`, REX (W/R/X/B, plus the bare `0x40` needed for SPL/BPL/SIL/DIL), opcode, ModRM, SIB and displacement. Every §11.1 gotcha lives in exactly one place:
  - RSP/R12 bases force a SIB byte.
  - RBP/R13 bases force at least a disp8.
  - RSP is rejected as an index; R12 is allowed as one.
  - A no-base `[disp32]` goes through a SIB byte, because `mod=00 rm=101` means RIP-relative.
  - `/digit` forms never trigger byte-register REX.
- Instructions:
  - `mov` in every form, with `mov_imm` choosing the shortest of `B8+r id`, `C7 /0 id` and `movabs`
  - `movsx`/`movzx`/`movsxd`, `lea`
  - the ALU group (rr/rm/mr/ri with imm8/imm32 selection), `test`
  - shifts (imm and CL), `imul` (2 and 3 operands), `not/neg/mul/imul/div/idiv`, `cqo`/`cdq`
  - `setcc`, `cmovcc`
  - `jcc`/`jmp` (rel8/rel32, to labels or absolute addresses), `call` (rel32, r/m, `[rip+d32]`), `jmp [rip+d32]`
  - `ret`, `push`/`pop`, 1–9-byte NOPs, `align(align, skew)` (to align a rel32 field, ready for Phase 3)
  - `xchg`, `lock xadd`, `lock cmpxchg`, `mfence`, `int3`, `ud2`
- Labels are resolved in `finish()`. A short branch that ends up out of range panics instead of silently truncating. `write_rel32` is the patch primitive for Phase 3.

### 3.2 Code memory (`src/jit/code_mem.rs`)
- `DualMap` (default): `memfd_create` + `ftruncate`, mapped `MAP_SHARED` twice, RW and RX.
- `Mprotect`: one private mapping that is RX, except for the duration of a copy, when the touched pages are RW.
- The buffer is capped at 1 GiB, so every rel32 reaches (§11.2). The default is 256 MiB (`--code-cache`).
- Bump allocation on 16-byte boundaries. `seal_prefix()` protects the trampolines across `reset()` (a full flush). `place()` asserts that the code was assembled for exactly the address it lands at, so absolute rel32 targets are always right.

### 3.3 Trampolines and helpers (`src/jit/trampoline.rs`)
- Prefix layout: helper table (8-byte absolute addresses, called with `call [rip+d32]`), then `enter_jit`, `exit_jit` and `fault_exit`, each 16-byte aligned.
- `enter_jit` pushes the six callee-saved registers, subtracts 8 so RSP is 16-byte aligned for helper calls, sets `rbp = cpu + 128` and `rbx = cpu.mem_base`, then does `jmp rsi`.
- `helper_interp_one(cpu, raw, pc)` decodes `raw` and runs the interpreter's `step`. It returns 0 to continue, or 1 to exit with `pc`/`exit_reason` (and `exc_*`) set. It is wrapped in `catch_unwind` → abort, so it can never unwind into JIT frames.

### 3.4 Lowering (`src/backend/x86/lower.rs`)
Naive by design: every guest register lives in `CpuState`, at `[rbp + 8*i - 128]` (disp8).
- Each instruction loads its operands into RAX/RCX, computes, and stores the result.
- `x0` reads become `mov r32, 0`. Writes to `x0` are skipped, but loads into `x0` still access memory, since they can fault.
- Loads and stores are `[rbx + rax + imm]` in the direct backend.
- Division uses guard sequences, so x86 never raises #DE: x/0 gives −1 or x, MIN/−1 gives MIN or 0, for both 64-bit and W forms.
- `MULHSU = mulhu − (a<0 ? b : 0)`.
- W ops are 32-bit x86 ops followed by `movsxd`.
- Branches are `cmp` + `jcc` to a taken stub, then `jmp` to a fall-through stub.
- JALR computes `(rs1+imm) & ~1` into `cpu.pc` before writing rd.
- ECALL and FENCE.I exit with a reason. Everything else (CSR, AMO, FP, MRET/SRET/WFI/SFENCE.VMA, EBREAK, illegal) calls the helper.
- Exit stubs sit at the TB tail and store, in order:
  1. `icount += n`
  2. `pc`
  3. `exit_reason` (if nonzero)
  4. `exc_*` (fetch fault only)

  Then they do `mov eax, (tb_id<<2)|slot` and `jmp exit_jit` (D25).
- `icount` is flushed before every helper call, so `rdcycle`/`minstret` read exact values. `pcmap` records `(host_off, idx, guest_pc, pending)` for every guest instruction (D28).

### 3.5 Dispatcher and faults (`src/jit/dispatch.rs`, `src/user/signal.rs`, `src/mem/direct.rs`)
- The loop mirrors the interpreter's: instruction limit, then the `tohost` check, then `tb_for(pc)` (lookup or translate), then `exec_tb`, then `deliver` (shared with the interpreter; traps into guest M-mode in bare mode, stops on ECALL or fault in user mode).
- When the buffer is full, `tb_for` flushes everything and retranslates. FENCE.I (exit reason FLUSH), `mmap`/`munmap` of executable memory and `riscv_flush_icache` flush through the new `Engine` trait.
- **Host protection now mirrors guest permissions** (D27). A JIT store to a read-only guest page, or a load from an unmapped one, raises a host SIGSEGV. The handler checks that RIP is inside this thread's code buffer (a thread-local range). It then stores RIP and `si_addr` into the `CpuState` found through RBP, and resumes at `fault_exit`. Any other SIGSEGV is chained to the previous handler (Rust's stack-overflow handler).
- Back in Rust, `resolve_host_fault` maps RIP → TB (binary search by host address) → `pcmap` entry. It restores `pc` and `icount`, and computes `tval` from `x[rs1] + imm`, or from the first inaccessible byte of a page-crossing access, just as `DirectMem::check` does.

### 3.6 Lockstep (`src/jit/lockstep.rs`)
For each TB:
1. Snapshot the CPU.
2. Run `interp::exec_block` over **the TB's own decoded instructions**, with `DirectMem::write_log` enabled.
3. Read back the new value at every logged address.
4. Restore the CPU and undo the writes.
5. Run the JIT TB.
6. Compare x/f, pc, icount, fflags/frm, privilege, reservation, all CSRs (field-by-field report, then a whole-struct check), the exit kind, and memory at every logged address.

The hot path allocates nothing and formats nothing unless the states differ. On divergence it prints the guest disassembly, the host code (a hex dump, or text with `--features disasm`) and the differences, then stops with `Stop::Diverged` (exit 1). Lockstep forces a deterministic `time` CSR (D29).

### 3.7 Plumbing
- `interp::Engine` trait (`run`, `flush`, `stats`) with three implementations. `jit::make_engine(EngineKind, &JitOptions)`. `interp::{exec_block, step, deliver, tohost_written}` are shared by all engines, so there is one definition of block semantics.
- CLI flags: `--engine interp|jit|lockstep`, `--max-block`, `--code-cache`, `--wx dualmap|mprotect`, `--no-host-features`, `--dump-x86 DIR`, `--perf-map`, `--deterministic`, and the hidden `--inject-bug`. `--stats` now prints the engine's counters too.
- `CpuState` gains `exc_cause`, `exc_tval`, `fault_rip`, `fault_addr`, `helper_mem` (0x248–0x268, `offset_of!`-asserted) and derives `Clone`. `Csrs` derives `PartialEq` and gains `deterministic_time`.

## 4. Design decisions made (appended to CLAUDE.md §3)
- **D25:** exit code `(tb_id << 2) | slot`, plus `exit_reason` values NONE/ECALL/EXCEPTION/FLUSH/HOST_FAULT.
- **D26:** `iced-x86` is also an optional runtime dependency (`disasm` feature).
- **D27:** host protection mirrors guest permissions. SIGSEGV → `fault_exit` → pcmap → precise exception. Execute-only pages stay host-readable.
- **D28:** Phase 2 TBs are keyed by pc only. Privileged, CSR and FP instructions go through the helper. `icount` is synced at exits and before helpers.
- **D29:** lockstep forces a deterministic `time`.

## 5. How it works: worked example
**Hello JIT** (`jit::code_mem::tests::hello_jit_dualmap`): `Asm::new(cm.next_addr())`, `mov_r32_imm(Rax, 42)`, `ret` → bytes `B8 2A 00 00 00 C3`. They are placed through the RW view, then `transmute`d to `extern "sysv64" fn() -> u64` at the RX address. Calling it returns 42. Patching byte 1 to 99 through the RW view makes the same function return 99, which proves the two views alias.

**A real TB** (`bridgev run --engine jit --dump-x86 DIR guest/build/hello.elf`, built with `--features disasm`). Guest block at `0x111ae` (the first `write` syscall setup):
```
  0x111ae: 4505      addi a0, zero, 1        (c.li)
  0x111b0: fffff597  auipc a1, 1048575
  0x111b4: ff858593  addi a1, a1, -8
  0x111b8: 4619      addi a2, zero, 6        (c.li)
  0x111ba: 04000893  addi a7, zero, 64
  0x111be: 00000073  ecall
```
Host code (103 bytes; RBP = &CpuState + 128, so `a0` = x10 is `[rbp-30h]` and `a7` = x17 is `[rbp+8]`):
```
  b800000000               mov eax,0                      ; rs1 = x0
  4883c001                 add rax,1
  488945d0                 mov [rbp-30h],rax              ; a0 = 1
  48c745d8b0010100         mov qword ptr [rbp-28h],101B0h ; AUIPC folded: pc + imm is a constant
  488b45d8                 mov rax,[rbp-28h]
  4883c0f8                 add rax,-8
  488945d8                 mov [rbp-28h],rax              ; a1 = 0x101a8
  ...                                                     ; a2 = 6, a7 = 64 likewise
  e900000000               jmp +0                         ; ECALL: exit through its stub
  4883859800000005         add qword ptr [rbp+98h],5      ; icount += 5 (ECALL itself not retired)
  48c78580000000be110100   mov qword ptr [rbp+80h],111BEh ; pc = the ECALL
  c7859000000001000000     mov dword ptr [rbp+90h],1      ; exit_reason = ECALL
  b802000000               mov eax,2                      ; (tb 0 << 2) | slot 2
  e969ffffff               jmp exit_jit
```
The dispatcher sees `ECALL`, services `write(1, "hello\n", 6)`, sets `pc += 4`, and enters the next TB (`tb 1`, exit code `6`), which calls `exit(0)`. `jmp +0` and `mov eax,0; add rax,1` show how naive Phase 2 is. Phase 3 turns the `jmp` into a patchable chain slot, and Phase 4's constant folding removes the rest.

**A precise fault** (`guest/asm/fault.S`): `li t1,7; addi t1,t1,35; lla t0,_start; sd t1,8(t0)` is one TB. The `sd` becomes `mov [rbx+rax+8], rcx`. The text page is host-read-only, so the host raises SIGSEGV. The handler redirects to `fault_exit`, and the pcmap entry for instruction index 5 (pending = 5) gives `pc = 0x111d6`, `icount += 5`, and `tval = x5 + 8 = 0x111bc`. All three engines print `guest store/AMO access fault (cause 7, tval 0x111bc) at pc 0x111d6` after 10 instructions and exit 139, which matches qemu's 139.

## 6. Tests and verification
| Suite (command) | Engines / configs | Result |
|---|---|---|
| `cargo test --test emitter_golden` | encoder vs iced-x86 | 22 tests, **331,560** structural checks (full memory-operand sweep: 132,736 forms), 0 failures |
| `cargo test --test riscv_tests` | interp, jit, lockstep, lockstep with `--wx mprotect --no-host-features --max-block 3` | **110/110** `rv64u{i,m,a,f,d,c}-p-*` in each of the 4 configs |
| `cargo test --test user_programs` | interp, jit, lockstep, jit with `--wx mprotect --no-host-features --max-block 7` | **31/31** programs byte-exact vs qemu in each of the 4 configs (30 from Phase 1 + `fault.elf`) |
| `cargo test --test jit_lowering` | JIT vs `interp::alu/aluw` | 5,740 register-ALU cases (18 OP + 10 OP-32 ops × 14² edge values + aliasing), all immediate forms and 0..63 shift amounts; block formation; 4 KiB cache flushed repeatedly |
| `cargo test --test cli` | | 8 tests: lockstep catches `--inject-bug` (exit 1, report shows the TB and `interp`/`jit` values); stats and dump; **precise host fault** identical across engines incl. `host-fault 1` |
| `cargo test --lib` | | 28 unit tests: code memory (hello JIT in both W^X modes, no RWX, bump/full/reset), trampolines (enter/exit, **callee-saved preservation via `asm!` harness**, prefix survives reset), TB cache, features, plus Phase 1 |
| Lockstep on CoreMark (release, CLI) | | **266,625,730 TBs identical**, 1.47 G instructions, "Correct operation validated" |
| Lockstep on `loop.elf` (release, CLI) | | 100,000,001 TBs identical |

Commands (all green at the commit of this report): `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, the same with `--features disasm`, `cargo test` (76 tests in 10 test binaries, 0 failed), and `tools/ref-check.sh` (31 passed, 0 failed).

## 7. Performance (ad-hoc, not a Phase 5 harness measurement)
Host: Intel Xeon @ 2.10 GHz (this container; the §4 machine was 2.80 GHz), shared cloud VM, so noisy. Commit `217901d`, release profile, `taskset -c 2`, 1 warm-up + 5 measured runs.

| Workload | Engine | min | median | max |
|---|---|---|---|---|
| `loop.elf` (10⁸ × `addi`+`bne`, 2-insn TBs) | interp | 135.3 MIPS | **146.5 MIPS** | 153.2 MIPS |
| `loop.elf` | jit | 171.8 MIPS | **178.1 MIPS** (1.22×) | 181.6 MIPS |
| CoreMark (rv64gc -O2, auto-calibrated), iterations/s | interp | 500.3 | **510.5** | 514.3 |
| CoreMark iterations/s | jit | 1 082.7 | **1 121.1** (**2.20×**) | 1 145.0 |
| CoreMark guest MIPS | interp | 178.1 | **181.5** | 181.9 |
| CoreMark guest MIPS | jit | 398.5 | **402.2** (2.22×) | 410.2 |

- CoreMark CRCs are `0xe714/0x1fd7/0x8e3a` in every run. One of the 5 measured JIT runs was **invalid**: it reported "Errors detected" without "Correct operation validated". I investigated rather than dropping it silently:
  - Auto-calibration sometimes picks 11,000 iterations instead of 20,000. At that run's 1,133.6 it/s that means 11,000 / 1,133.6 = 9.7 s, and CoreMark rejects runs shorter than 10 s (`core_main.c:376`).
  - Reproduced with a fixed iteration count (`bench-coremark.bin 0x0 0x0 0x66 10000`): the JIT run took 8.9 s and printed "ERROR! Must execute for at least 10 secs" and "Errors detected", but with correct CRCs.
  - `crcfinal` = **0x988c** under both the JIT and the interpreter at 10,000 iterations.
  - The 12 further JIT runs made while investigating were all validated.
  - The JIT row therefore reports the median of the **4 valid** runs of the batch. P5.1 will pass the iteration count explicitly so this cannot recur.
- JIT CoreMark `--stats`:
  - 1,591 TBs translated (8,937 guest instructions, 199 KiB host code, **22.9 host bytes per guest instruction**)
  - translation time 2.6 ms total
  - 2.02 G TB entries for 11.2 G instructions: **5.5 instructions per dispatcher round trip**
- Why the speedup is only 1.2–2.2×: every TB returns to the dispatcher (hash lookup, `enter_jit` with 6 pushes and 6 pops, an indirect `jmp rsi` whose target changes every time), and every guest register access is a memory round trip. On `loop.elf` the dispatcher is almost all of the time. These are exactly the costs Phases 3 (chaining, jump cache) and 4 (register allocation) remove.
- The roadmap target "naive JIT ≥ 3× interp" (CLAUDE.md §22, a hypothesis) is **not met** on CoreMark. This is recorded as measured. The Phase 2 acceptance criterion only asked for the number to be recorded.

## 8. Bugs found and fixed
| # | Symptom | Root cause | Fix | Guard |
|---|---|---|---|---|
| 1 | CI run #10 failed at `cargo fmt --check` | I edited `tests/emitter_golden.rs` (the immediate-mask comparison) after the last `cargo fmt` and committed | Formatted in `a9a140e` (runs #11 and #12 green). `cargo fmt --check` is now part of the pre-commit sequence alongside clippy | CI fmt step |
| 2 | Golden test failures on `imul ax,r12w,-100` and `add ax,-1` | Test-model bug, not an encoder bug: iced returns widening immediates (imm8→16/32) sign-extended to 64 bits | Compare immediates at the operand's width | The test |
| 3 | Lowering would have counted helper-executed instructions twice | `call_interp` set `pending = 1` and `insn` then added 1 again | Caught in self-review before the first run. `call_interp` leaves the count to `insn`. Covered by lockstep's icount comparison and `rdcycle`-using riscv-tests | Lockstep |
| 4 | Lockstep on `loop.elf` took more than 120 s | Per-TB `format!` of 64 register names, plus cloning the decoded instructions and the write log | Fast equality path, borrowed instructions, reused buffers: 7.2 s for 10⁸ TBs | — |
| 5 | Shell self-kill (again) while stopping a benchmark | The `pkill -f` pattern also matched the invoking shell's own command line | Separate commands; `[x]yz` patterns only when the rest of the line cannot match | Working rule |
| 6 | Compile errors while developing | `asm!` with `clobber_abi` needs explicit output registers; borrow conflicts in `DirectMem::store` logging and label creation; clippy `not_unsafe_ptr_arg_deref` (the helper is now `unsafe extern "sysv64"`) and `deref_addrof` on the static `sigaction` | Fixed as described | Compiler, clippy |
| 7 | A CoreMark JIT run was not validated | Not a JIT bug: auto-calibration chose too few iterations, so the run was shorter than 10 s (reproduced; CRCs and `crcfinal` identical to the interpreter), see §7 | P5.1 fixes the iteration count | Recorded |

## 9. Deviations from the plan / spec
- **Golden tests compare structure, not text** (P2.1). Each decoded instruction's mnemonic, operand kinds, registers, memory base/index/scale/displacement/size, immediates, branch targets and length are compared with what was requested. This is stricter than comparing formatter output and doesn't depend on iced's text conventions.
- **P2.7 goes further than planned:** instead of "report the guest fault and exit 139", faults are precise, recoverable guest exceptions (a bare-mode guest would receive the trap in M-mode). User mode still exits 139, as before.
- **Exit code** is `tb_id`-based, not `tb_ptr`-based (D25). CLAUDE.md §8.4 is updated.
- **`--trace insn`** is interpreter-only for now (rejected with exit 2 for `jit`/`lockstep`).
- **CoreMark binary** is still an ad-hoc build (`guest/build/bench-coremark.bin`, the command from the Phase 1 report). The official build is P5.1.
- New files: `src/backend/x86/disasm.rs`, `src/jit/lockstep.rs`, `tests/emitter_golden.rs`, `tests/jit_lowering.rs`, `guest/asm/fault.S`, `tests/data/expected/asm-fault.{out,code}`. CLAUDE.md §6 is updated.

## 10. Known limitations and technical debt
- **Naive code quality** (by design): 22.9 host bytes per guest instruction, every register access goes through memory, and `jmp +0` goes to adjacent stubs.
- **No chaining:** every TB returns to the dispatcher (Phase 3).
- **Host features are detected but unused.** The Phase 2 lowering is baseline x86-64, so `--no-host-features` currently changes nothing. It is plumbed and tested for the phases that use BMI2 and FMA.
- **Wild guest addresses** more than 4 GiB outside the 2³⁸ reservation can alias host memory, as in qemu-user. `--mem=direct-checked` is planned (§14.1).
- **Execute-only guest pages are host-readable,** so a JIT load from one does not fault (the interpreter does fault). No known program depends on this.
- **Lockstep blind spot:** it checks every address the interpreter wrote, but not stray JIT writes to other addresses. A memory checksum option is possible later.
- **FENCE.I flushes the whole cache** (no page-level invalidation until Phase 8). There is no SMC detection for stores without FENCE.I (Phase 8).
- **TBs are not keyed by privilege or FS** (D28). This is correct only while privileged, CSR and FP instructions go through the helper. Phase 7 adds `TbFlags`.

## 11. How to reproduce
```
tools/setup.sh && tools/build-guests.sh && tools/build-riscv-tests.sh
cargo fmt --check && cargo clippy --all-targets -- -D warnings \
  && cargo clippy --all-targets --features disasm -- -D warnings && cargo test
cargo build --release
target/release/bridgev run --engine jit --stats guest/build/hello.elf
target/release/bridgev run --engine lockstep --stats guest/build/loop.elf
target/release/bridgev run --mode bare --engine jit guest/build/riscv-tests/rv64ui-p-add
cargo build --release --features disasm --target-dir target/disasm
target/disasm/release/bridgev run --engine jit --dump-x86 /tmp/tbs guest/build/hello.elf
# CoreMark (ad-hoc build, see the Phase 1 report §7):
taskset -c 2 target/release/bridgev run --engine jit --stats guest/build/bench-coremark.bin
```

## 12. Next steps
Phase 3 (block chaining and the jump cache):
- **P3.1** exit slots with 4-byte-aligned rel32 fields (`Asm::align(4, opcode_len)` already exists)
- **P3.2** patch and unpatch with incoming lists (`write_rel32` + `CodeMem::patch`)
- **P3.3** a budget prologue so chained loops stay preemptible
- **P3.4** an inline jump cache for JALR
- then measure `--no-chain` against chaining on `loop.elf` and CoreMark
