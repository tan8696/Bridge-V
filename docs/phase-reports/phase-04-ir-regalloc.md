# Phase 4 report: IR, optimizer and register allocation

| Field | Value |
|---|---|
| Phase | 4: IR, optimizer and register allocation (see `docs/ROADMAP.md`) |
| Status | Complete with one dropped task (`--check-abi`, D41) |
| Dates | 2026-09-26 → 2026-09-26 |
| Branch / final commit | `claude/compassionate-babbage-ul3prl` @ the commit that adds this report (code: `f192e6e`, `d41abee`, `5a76e67`, plus the report commit) |
| Sessions used | 1 (spanning two context windows) |

## 1. Summary
Phase 4 replaces the 1:1 instruction lowering with a real compiler pipeline for each translation block: decode → **IR** (single-assignment values) → **optimizer** (forwarding, constant folding, dead write-back elimination, DCE) → **register allocator** (x2/x1/x10/x15 pinned in R12–R15, the other guest registers cached in a 7-register pool with lazy write-back and furthest-next-use eviction) → x86. Memory faults stay precise with no code on the hot path: each load/store records a *fault site* state map that the dispatcher replays after a SIGSEGV (D37).

Correctness evidence:
- A new random-block fuzzer runs every block under the interpreter and under five JIT configurations. It was clean over **1,000,000 random blocks** (5 million JIT executions) on the final code, and it found one real allocator bug, now fixed, whose case is committed as a regression.
- All riscv-tests and all 35 guest programs pass at every `--regalloc` level, under both jit and lockstep.

The most important result: **CoreMark runs at 12,754 iterations/s with the linear allocator, 1.75× the Phase 3 code** (`--regalloc none`, 7,305 it/s) on the same host. That is 23.6× the interpreter and 1.32× `qemu-riscv64` (Phase 3: 0.76×). All 35 CoreMark runs validated.

## 2. Planned vs delivered
| Task ID | Task | Status | Notes |
|---|---|---|---|
| P4.1 | IR definitions and printer | done | `src/ir/ops.rs`. The op set is smaller than §9's list (D34). `--dump-ir DIR` writes lifted and optimized IR per TB. |
| P4.2 | Lifter + IR evaluator | done | `src/ir/lift.rs`, `src/ir/eval.rs`. Non-native instructions become `Interp` (D14). |
| P4.3 | Optimizer passes | done | `src/ir/opt.rs`: `forward`, `dead_writes`, `fold`, `dce` (D35). Golden unit tests plus a property test of every pass, alone and cumulatively. |
| P4.4 | Liveness and intervals | done | `src/ir/liveness.rs`: use positions, intervals, fixed-register constraints. |
| P4.5 | Pinned registers | done | `enter_jit`/`exit_jit` load and store the pinned set; `--regalloc=pinned`, `--pin`. **`--check-abi` dropped** (D41). |
| P4.6 | Linear-scan allocator | done | `src/regalloc/linear_scan.rs` (D36). The tests are guest-code tests and fuzzing instead of synthetic-IR unit tests (§9 of this report). |
| P4.7 | Lowering v2 + stubs | done | `src/backend/x86/lower_ir.rs`: lea/imul-3/BMI2/setcc/test-imm/neg selection, exit and budget stubs at the tail, fault sites (D37). `none` keeps the Phase 3 backend. |
| P4.8 | Random block fuzzer | done | `tests/fuzz_blocks.rs` + `tests/common/rvgen.rs`. CI runs 2,000 blocks × 5 configs with a fixed seed. The 10⁶ run is recorded in §6. |
| P4.9 | Pinned-set profiling | done | `--stats=regs` (interpreter engine, D40). Measured alternatives and kept the default (D42). |

## 3. What was built

### 3.1 IR (`src/ir/ops.rs`, `lift.rs`, `eval.rs`)
- A block is a `Vec<Op>` over values `V(n)`, each defined once:
  - `Insn{pc,idx}` marks guest instruction boundaries.
  - `Const`, `ReadReg{g}`, `WriteReg{g}`.
  - `Bin`/`BinImm` (every RV64IM ALU op including the W forms).
  - `Load{size,signed}`, `Store`.
  - `Interp{raw}`: the D14 fallback, for CSR, AMO, FP and system instructions.
  - Terminators: `Branch`, `Jump`, `JumpInd`, `Exit{Ecall|FenceI|Fault}`.
- **Lifting** turns reads of x0 into `Const 0` and drops writes to x0. JALR becomes `ReadReg`, then `add imm`, then `and -2`, then the link write, then `JumpInd`. That order is correct even when `rd == rs1`.
- **`ir::eval`** executes a block directly against `CpuState` with exactly the interpreter's semantics (pc and icount on every exit kind). It is the oracle that separates front-end bugs from backend bugs.

### 3.2 Optimizer (`src/ir/opt.rs`)
All four passes are single linear sweeps:
1. **`forward`.** A read of a guest register whose value is already known in the block becomes that value, and repeated reads are CSE'd. `Interp` resets the knowledge, because a helper may change any register.
2. **`dead_writes`.** Removes a `WriteReg g` that is overwritten later, if no `Load`/`Store`/`Interp` lies between them. At a fault site the older value is architecturally visible.
3. **`fold`.** Constant evaluation of any op. `Sub` of a constant becomes `Add` of its negation. Register-register ops with a constant operand become immediate forms. Identities (`x+0`, `x|0`, `x^0`, shift by 0, `x&-1`, `x*1`) become renames, and `x&0` / `x*0` become `Const 0`. Branches on constants or on identical operands become `Jump`, and `JumpInd` to a constant becomes a chainable `Jump`.
4. **`dce`.** Removes unused pure ops. Loads are kept because they can fault.

### 3.3 Liveness (`src/ir/liveness.rs`)
One forward sweep gives each value its definition position and its sorted use positions. `next_use` (binary search) drives eviction. `constraint(op)` names the fixed registers an op needs:
- RDX:RAX for MULH\*, DIV\* and REM\*
- RCX for variable shifts without BMI2
- the whole pool at helper calls

### 3.4 Register allocator (`src/regalloc/linear_scan.rs`)
The allocator runs interleaved with code generation, in one pass over the block. That is linear scan with intervals visited in start order, which for straight-line code is simply op order. State per value:
- `reg`: the pool register holding it.
- `pin`: the pinned register holding it.
- `back`: where it can be recovered without a register: `Home(g)` (guest register g's `CpuState` slot), `Slot(k)` (spill slot) or `Const(c)`.
- `dirty`: the guest registers whose current value it is but which have not been stored home yet.

Main rules:
- **Reading a guest register** aliases an already-known value if there is one: the pinned register's current value, a dirty value, or the value last stored to its home. Otherwise it creates a value backed by the home. Nothing is loaded until the value is used.
- **Writing a non-pinned guest register** (with `lazy`, the `linear` level) only marks the value dirty. It is stored home at an exit, before a helper call, or when the register holding it is evicted. So a register written several times in a block is stored once. Writes to pinned registers move the value into R12–R15 immediately, so pinned state is exact at every fault site.
- **Eviction** picks the pool register whose value's next use is furthest away. A clean value with a backing is dropped for free. A dirty value is written home, which the exit would have done anyway. A temporary is spilled to `cpu.spill[k]`.
- **Protecting a home.** Before a store overwrites `home(g)`, a value still backed by that home that is still needed (a later use, or pending for another register, e.g. after `mv x7, x8`) is copied to a spill slot.
- **Helper calls** (`Interp`) are a full sync. The allocator writes back dirty registers, stores the pinned registers, saves live values to slots, and reloads the pinned registers after the call.
- **Two invariants learned the hard way (§8):** the current op's operands count as live while it is being lowered, and R10/R11 are never held across an allocator call.

### 3.5 Lowering v2 (`src/backend/x86/lower_ir.rs`)
- **Prologue and exits** are as in Phase 3: the budget `sub`/`jl`, and aligned rel32 exit slots with stubs at the TB tail (`exit_cond`, `exit_direct`), plus the jump-cache sequence for `JumpInd`. Every exit path writes back dirty registers first.
- **Instruction selection:**
  - `lea` for add/addi when the destination is a third register.
  - `neg`+`add` for `sub` when the destination is the subtrahend's register.
  - `imul r, r/m, imm32`.
  - BMI2 `shlx`/`shrx`/`sarx` (new VEX encoder `Asm::shiftx`, golden-tested for all register combinations), with the RCX path when BMI2 is absent.
  - `cmp`+`setcc`+`movzx` for slt/sltu.
  - `movsxd` after W ops.
  - Guarded DIV/REM (§7.5) through R10/R11.
  - `test r,r` / `cmp r, imm32` for branches against constants, with the condition mirrored when the constant is the first operand (`blt x0, r` becomes `jg`).
  - `neg` for `sub rd, x0, rs`.
- **Loads and stores** are one `[rbx + r + disp32]` instruction each. Each one records a fault site (D37).
- **Oversize blocks.** If a TB needs more than 64 spill slots, translation returns `OutOfSlots` and the dispatcher retranslates it at half the length (D38). This has not happened on any workload (`retranslations 0` in `--stats`).

### 3.6 Precise faults with lazily written registers (D37)
The SIGSEGV handler (`src/user/signal.rs`) copies the host GPRs from the signal context into `cpu.fault_regs`, then resumes at `fault_exit`. The dispatcher finds the TB from the faulting RIP, and then the fault site from the RIP offset. `resolve_site` then:
- writes every dirty guest register home, from the saved host register, a spill slot, a constant or another home
- sets `pc`
- refunds the budget for the instructions that did not retire
- computes `tval` from `fault_regs[addr] + off`, clamped to the first faulting byte for a split access

The hot path contains no code for any of this.

### 3.7 Tooling
- **`--regalloc none|pinned|linear`, `--pin x2,x1,x10,x15`** (up to 4, mapped to R12..R15; `--pin ""` pins nothing), and **`--dump-ir DIR`**.
- **`--stats`** now also prints the allocator's static counts (fills, spills, write-backs, moves and retranslations, summed over all emitted TBs).
- **`--stats=regs`** (interpreter only, D40) prints a markdown histogram of integer-register references (static per decoded block, dynamic per retired instruction), plus the dynamic share covered by the default and by the best 4-register pinned set.

## 4. Design decisions made (appended to CLAUDE.md §3)
- **D34: IR shape.** No `CallHelper`/`Ext`/`Amo`/`Fence` ops; everything non-native is `Interp`. *Alternative:* the full §9 op list. Rejected because those ops would have no native lowering yet.
- **D35: optimizer passes.** Dead write-backs across fault sites are left to the allocator's lazy write-back.
- **D36: allocator.** One interleaved pass with value backings and lazy write-back. *Alternative:* a separate interval-sorting pass. It gives the same result on straight-line code, with more code.
- **D37: precise faults via fault-site state maps** replayed by the dispatcher, instead of a cold stub per memory access. *Alternative:* §15's cold stubs. Those would need a check and branch after every access in direct mode, where the host MMU already does the check. Softmmu (Phase 7) will use real cold stubs.
- **D38: 64 spill slots** at 0x10300, and retranslation at half length on overflow.
- **D39: the three regalloc levels and `--pin`.**
- **D40: `--stats=regs`** runs on the interpreter only.
- **D41: `--check-abi` dropped.** Lockstep and the fuzzer compare full architectural state after every TB, and that state includes the pinned registers.
- **D42: the pinned set stays x2, x1, x10, x15** (see §7.3).

## 5. How it works: worked example (CoreMark `crcu8`, inlined in `crcu16`)
The hottest loop of CoreMark's CRC is 12 guest instructions at 0x11fac:
```
xor a5,a2,a0 ; andi a5,a5,1 ; subw a5,zero,a5 ; srliw a0,a0,1 ; and a5,a6,a5 ; addiw a4,a4,-1
xor a5,a5,a0 ; slli a0,a5,48 ; andi a4,a4,255 ; srli a2,a2,1 ; srli a0,a0,48 ; bnez a4, 0x11fac
```

**Lifted IR** (`--dump-ir`, abridged): every instruction reads its sources and writes its destination, 28 values in all. a5 is written 5 times, a0 3 times, a4 twice.
```
  --- #0 @0x11fac
    v0 = x12
    v1 = x10
    v2 = xor v0, v1
    x15 = v2
  --- #1 @0x11fb0
    v3 = x15
    v4 = and v3, 1
    x15 = v4
  --- #2 @0x11fb2
    v5 = const 0x0
    v6 = x15
    v7 = subw v5, v6
    x15 = v7
  ...
  --- #11 @0x11fce
    v26 = x14
    v27 = const 0x0
    br.Ne v26, v27 ? 0x11fac : 0x11fd0
```
**Optimized IR:** forwarding removed every re-read, and `dead_writes` removed 5 of the 8 register writes (no load or store lies in between). Only x15, x14, x12 and x10 are written, once each.
```
  --- #0 @0x11fac
    v0 = x12
    v1 = x10
    v2 = xor v0, v1
  --- #1 @0x11fb0
    v4 = and v2, 1
  --- #2 @0x11fb2
    v5 = const 0x0
    v7 = subw v5, v4
  --- #3 @0x11fb6
    v9 = srlw v1, 1
  --- #4 @0x11fba
    v10 = x16
    v12 = and v10, v7
  --- #5 @0x11fbe
    v13 = x14
    v14 = addw v13, -1
  --- #6 @0x11fc0
    v17 = xor v12, v9
    x15 = v17
  --- #7 @0x11fc2
    v19 = sll v17, 48
  --- #8 @0x11fc6
    v21 = and v14, 255
    x14 = v21
  --- #9 @0x11fca
    v23 = srl v0, 1
    x12 = v23
  --- #10 @0x11fcc
    v25 = srl v19, 48
    x10 = v25
  --- #11 @0x11fce
    v27 = const 0x0
    br.Ne v21, v27 ? 0x11fac : 0x11fd0
```
**Allocator decisions:**
- x10 (a0) is pinned in R14 and x15 (a5) in R15. The other registers live in `CpuState`, addressed as `[rbp + 8·g − 128]`.
- `v0` (a2) is filled into RAX and stays there until its last use at #9, where `srl` updates it in place.
- `v2`/`v4`/`v7` reuse RCX as each dies.
- `v9` goes to RDX, and `v10` (a6, only read) to RSI.
- `v17` is written to R15 at #6 and immediately reused as the shift source.
- `v21` (a4) is dirty in RCX until the exit, and `v23` (a2) is dirty in RAX.
- At the branch, the two dirty registers are written back (`mov [rbp-20h],rax` for x12 and `mov [rbp-10h],rcx` for x14). The constant 0 becomes `test rcx,rcx`.
- No spills, no moves between pool registers, 3 fills.

**x86 (hot path + cold stubs):** 193 bytes, versus 273 for `--regalloc none` and 241 for `pinned`.
```
  sub qword ptr [rbp+88h],0Ch     ; budget: 12 instructions
  jl  budget_stub
  mov rax,[rbp-20h]               ; fill a2
  mov rcx,rax
  xor rcx,r14                     ; a0 pinned in R14
  and rcx,1
  neg ecx                         ; subw a5, zero, a5
  movsxd rcx,ecx
  mov rdx,r14
  shr edx,1                       ; srliw a0, a0, 1
  movsxd rdx,edx
  mov rsi,[rbp]                   ; fill a6
  and rsi,rcx
  mov rcx,[rbp-10h]               ; fill a4
  add ecx,0FFFFFFFFh              ; addiw a4, a4, -1
  movsxd rcx,ecx
  xor rsi,rdx
  mov r15,rsi                     ; a5 pinned in R15: its only write
  shl rsi,30h
  and rcx,0FFh
  shr rax,1
  shr rsi,30h
  mov r14,rsi                     ; a0: its only write
  mov [rbp-20h],rax               ; write back a2
  mov [rbp-10h],rcx               ; write back a4
  test rcx,rcx                    ; bnez a4
  jne rel32                       ; slot 1 (rel32 field 4-byte aligned), linked to this TB itself
  nop
  jmp rel32                       ; slot 0 → TB 0x11fd0 once linked
budget_stub:                      ; cold
  add qword ptr [rbp+88h],0Ch ; mov qword ptr [rbp+80h],11FACh ; mov dword ptr [rbp+90h],5 (BUDGET)
  mov eax,0AAEh ; jmp exit_jit    ; (tb 683 << 2) | 2
stub_1: mov qword ptr [rbp+80h],11FACh ; mov eax,0AADh ; jmp exit_jit
stub_0: mov qword ptr [rbp+80h],11FD0h ; mov eax,0AACh ; jmp exit_jit
```
Once slot 1 is linked to the TB's own entry, each loop iteration is those 29 instructions. They include 3 loads and 2 stores to `CpuState` plus the budget read-modify-write, with no dispatcher involvement.

## 6. Tests and verification
**Inventory.** 103 tests (Phase 3: 88). New or extended:
- `tests/ir_passes.rs`: lift → eval, and each pass alone and cumulatively, against the interpreter on random blocks. 3,000 cases per run, and 100,000 once locally.
- `tests/fuzz_blocks.rs` + `tests/common/rvgen.rs`:
  - Blocks of 1–40 body instructions plus a terminator. The body covers every OP/OP-32/OP-IMM/OP-IMM-32 form, LUI/AUIPC, loads and stores through x5 into a scratch page, and CSR/AMO/FP-move helpers. The terminator is ECALL, JAL, JALR, any branch, or a faulting load/store through an unmapped x6.
  - Runs under five configurations: `none`, `pinned`, `linear`, `linear` with no pins, and `linear` without BMI2 with x5,x7,x8,x9 pinned. It compares x, f, pc, icount, the LR/SC reservation, the exit and a scratch-page hash.
  - A failure report contains the guest listing, the optimized IR and the x86 disassembly (§21.4). proptest shrinks the case and saves it to `tests/fuzz_blocks.proptest-regressions`.
- `src/ir/liveness.rs`, `src/ir/opt.rs` (4 golden tests), `src/stats.rs` (histogram counting), `src/jit/trampoline.rs::pinned_registers_round_trip`, `tests/emitter_golden.rs::bmi2_shifts_all_registers`.
- `tests/jit_lowering.rs::register_pressure_and_division_chains`:
  - 16 guest values live at once (more than the 7-register pool), consumed in another order, plus DIV/REM/DIVU/REMU/DIVW chains that reuse their own sources.
  - Checked against a Rust model for 14×14 operand pairs, at 3 levels × 3 pin sets, asserting that the linear allocator actually wrote back and refilled.
- `tests/riscv_tests.rs`: `none`/`pinned` levels under jit and lockstep, and linear with no pins, `max_block 3`, `slice 1`.
- `tests/user_programs.rs`: `none`/`pinned` under jit and lockstep, and lockstep with `--pin ""`.
- `tests/cli.rs`:
  - `lockstep_catches_injected_miscompilation` now runs at all 3 levels, on `fib-O2`.
  - New: `stats_regs_histogram`.
- CI: `PROPTEST_RNG_SEED=20260926` for reproducible property tests.

**Commands and results** (final code):
```
$ cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo clippy --all-targets --features disasm -- -D warnings
(clean)
$ PROPTEST_RNG_SEED=20260926 BRIDGEV_REQUIRE_GUESTS=1 cargo test     # debug, exactly as CI
test result: ok. 37 passed (lib) · 11 (cli) · 2 (decoder_vectors) · 3 (elf) · 23 (emitter_golden) · 1 (fuzz_blocks)
             · 1 (ir_passes) · 9 (jit_lowering) · 8 (riscv_tests) · 8 (user_programs); 0 failed
riscv_tests --nocapture: "Jit: 110 riscv-tests run, 0 failed", "Lockstep: 110 riscv-tests run, 0 failed" (every config)
$ PROPTEST_CASES=1000000 target/release/deps/fuzz_blocks-*
test jit_blocks_match_the_interpreter ... ok
test result: ok. 1 passed; 0 failed; ... finished in 226.11s
```
The 1,000,000-block run executes each block under all 5 configurations (5·10⁶ JIT block executions, plus the interpreter references). An earlier 10⁶ run, before the branch-immediate/`neg` selection change, was also clean (217.7 s).

**Acceptance criteria:**
- ✅ Fuzzer clean over ≥ 10⁶ random blocks (above).
- ✅ All suites pass at each regalloc level, under jit and lockstep: `riscv_tests::phase1_suites_pass_at_every_regalloc_level`, `user_programs::guest_programs_match_qemu_reference_regalloc_levels`, plus the default (`linear`) configurations of every Phase 2/3 test.
- ✅ Per-level speedup table recorded (§7).

## 7. Performance (ad-hoc, not a Phase 5 harness measurement)
Measurement conditions:
- Host: Intel Xeon @ 2.10 GHz, the same model as Phase 3 (a shared cloud VM, so noisy).
- Build: release, commit `5a76e67`.
- Method: `taskset -c 2`, 1 warm-up + 5 measured runs, median (min–max), all configurations in one batch (`/tmp` script reproduced in §11).
- CoreMark ran 150,000 iterations (≥ 10 s at every level). **All 25 CoreMark runs report "Correct operation validated".**

### 7.1 Per-level speedup
| Workload | `none` (Phase 3 code) | `pinned` | `linear` (default) | linear / none |
|---|---|---|---|---|
| CoreMark, iterations/s | 7,305 (7,211–7,420) | 9,312 (9,059–9,554) | **12,754** (12,641–13,384) | **1.75×** |
| CoreMark, guest MIPS | 2,621 (2,587–2,662) | 3,340 (3,249–3,427) | 4,572 (4,533–4,799) | 1.74× |
| fib-O2 32, MIPS | 3,527 (3,441–4,256) | 4,178 (4,018–4,364) | 4,408 (3,650–4,781) | 1.25× |
| loop.elf, MIPS | 3,247 (3,071–3,465) | 3,364 (3,151–3,960) | 3,480 (3,100–3,531) | ≈1 (noise) |

Context against earlier phases and references (CoreMark it/s):

| Reference | CoreMark it/s | Source |
|---|---|---|
| interpreter | 540 (531–555) | this session, second batch (6,000 iterations, ≥ 10 s), 5/5 validated |
| `qemu-riscv64` 8.2.2 | 9,676 (9,506–9,895) | this session, second batch (150,000 iterations), 5/5 validated |
| Phase 3 JIT | 7,260 | Phase 3 report; matches `none` here within noise |

Linear is **23.6× the interpreter** and **1.32× QEMU** (Phase 3: 12.3× and 0.76×). The interpreter measured 591 it/s in Phase 3; its only change since is one per-block `Option` check for `--stats=regs`, so the 9% difference is most likely VM variation (not investigated). The second batch ran about 15 minutes after the first.

Code size on CoreMark (2,000-iteration run, `--stats`):
- **`none`:** 31.3 bytes per guest instruction.
- **`pinned`:** 29.4 bytes, with 5,451 fills, 3,842 write-backs, 2,140 moves and 0 spills.
- **`linear`:** **26.4 bytes**, with 2,526 fills, 2,920 write-backs, 1,356 moves, 20 spills and 0 retranslations over 1,614 TBs.

Translation time is under 10 ms in all cases: 7.9 ms for linear versus 2.2 ms for none, on a run of about 10 s.

**Why each level helps:**
- **`pinned`.** sp, ra, a0 and a5 stop going through memory, both inside blocks and across chained blocks. The IR front end also removes x0 reads and the Phase 3 per-instruction RAX shuffling.
- **`linear`.** Each non-pinned register is loaded once and stored once per block, instead of once per use, and the optimizer folds constants and removes intermediate writes (§5).
- **`loop.elf`** is flat because its TB is two instructions (`addi t1,t1,1; bne t1,t0`). t1 and t0 are loop-carried values that live in `CpuState` at every TB boundary, so every level performs the same load, add, store and compare. Pinning them proves this: `--pin x6,x5` measured 4,605 MIPS median (4,561–4,655) against 3,209 (3,086–3,467) in the same batch, 1.43×. Values that stay live across blocks are exactly what the pinned set is for (§7.3).

### 7.2 Register-use histogram (`--stats=regs`, interpreter)
The table shows the dynamic share of all integer-register references that the default set and the best four covered, per program:

| Program | x2,x1,x10,x15 (default) | Best 4 (dynamic) | Best set's share |
|---|---|---|---|
| CoreMark (2,000 it.) | 39.9% | x15,x14,x13,x10 | 72.1% |
| qsort-O2 | 41.2% | x15,x10,x14,x2 | 46.6% |
| strings-O2 | 37.9% | x15,x16,x14,x10 | 59.0% |
| malloc-O2 | 33.8% | x15,x11,x14,x16 | 63.6% |
| printf_float-O2 | 37.8% | x15,x14,x16,x10 | 57.6% |
| fib-O2 25 | 36.7% | x2,x15,x10,x14 | 41.0% |

Full table for fib-O2 25 (top 8 of 31 rows):

| reg | static uses | % | dynamic uses | % |
|---|---:|---:|---:|---:|
| x2 (sp) | 1499 | 15.5 | 2108834 | 18.0 |
| x15 (a5) | 1733 | 18.0 | 1336347 | 11.4 |
| x10 (a0) | 742 | 7.7 | 715830 | 6.1 |
| x14 (a4) | 901 | 9.3 | 637061 | 5.4 |
| x9 (s1) | 648 | 6.7 | 563218 | 4.8 |
| x8 (s0) | 495 | 5.1 | 539363 | 4.6 |
| x21 (s5) | 154 | 1.6 | 473277 | 4.0 |
| x18 (s2) | 408 | 4.2 | 442503 | 3.8 |

### 7.3 Pinned-set experiment (P4.9, D42)
Same batch as §7.1:

| Pinned set | CoreMark it/s | fib-O2 32 MIPS | loop MIPS |
|---|---|---|---|
| x2,x1,x10,x15 (default) | 12,754 (12,641–13,384) | 4,408 (3,650–4,781) | 3,480 (3,100–3,531) |
| x15,x14,x13,x10 (CoreMark's top 4) | 13,087 (13,016–13,659) | 4,137 (3,651–5,101) | 3,645 (3,055–3,921) |
| x2,x15,x10,x14 (cross-program top 4) | 12,540 (12,356–12,821) | 4,256 (3,652–5,031) | 3,183 (3,071–3,557) |

CoreMark's own top 4 gains only +2.6%, with ranges overlapping; fib loses in the median; the cross-program set is slower on CoreMark. A high use count does not make a register worth pinning. Pinning pays for values live **across** blocks, which the per-block allocator cannot keep in registers. x1 (ra) is an example: few uses, but it carries every return through the jump cache. **Decision: the default stays.** Revisit with Dhrystone and the Phase 5 harness, possibly with a cross-block liveness histogram instead of use counts.

## 8. Bugs found and fixed
| Symptom | Root cause | Fix | Regression test |
|---|---|---|---|
| `regalloc: storing lost value v1` panic (fib, qsort under lockstep) | Home protection in `store_home` used "live after this op". A value that is only pending for another register (after `mv x7,x8`) lost its backing. | Protect when `needed` (live or dirty). | user-program suites at `linear` |
| `regalloc: value v20 has no location` (fib-O2, qsort-O2, printf_float-O0) | Loading operand *a* could evict operand *b* of the same op as dead: `next_use` excluded the current position, so the operand looked dead and was dropped unsaved. | Operands of the current op count as live (`live_now`). Only `release_if_dead` uses "after". | user-program suites, fuzzer |
| (found by analysis while hunting the above) | A `ReadReg` could alias a value that had died and released its register or slot, while its home or pinned register still held it. | Restore the home/pin as that value's backing on alias. | fuzzer |
| `mulh s0, ra, zero` computed ra·ra (fuzzer, case 1,842, config "linear, no pins") | `muldiv` parked the operands in R10/R11 and then vacated RDX, whose eviction copied a protected home through R11. | Vacate the fixed registers first, load the operands avoiding them, and never hold R10/R11 across an allocator call. The same fix went into `shift_cl`. | `tests/fuzz_blocks.proptest-regressions` (replayed first on every run) |
| `lockstep_catches_injected_miscompilation` passed silently at `linear` | Test gap: `hello` is asm whose ADDIs all constant-fold, so the injected +1 never reached code. | The test uses `fib-O2` and runs at all three levels. | itself |
| `exit_slots_are_aligned…` found 12 of 18 slots | Test gap: `bne x6,x6` is folded to a jump (same operands). | Use `bne x6,x7`; run at all three levels. | itself |
| Rewritten lockstep test skipped locally | Test gap: it asked for `fib.elf`, but the build produces `fib-O2.elf` etc. Without `BRIDGEV_REQUIRE_GUESTS=1`, a missing guest skips silently. | Use `fib-O2`; the final local run sets `BRIDGEV_REQUIRE_GUESTS=1` like CI. | itself (CI fails on missing guests) |
| clap panic on `--pin` | A `Vec<u8>` value_parser type mismatch. | `PinList` newtype with `parse_pin`. | `user_programs` (`--pin ""`) |

The fuzzer bug is the one to remember. Every suite and a 2,000-block smoke run had passed; the first long fuzz run hit it at block 1,842. It needs a MULH/DIV whose fixed-register eviction meets a home-protected value, which is far likelier with no pinned registers (more guest registers compete for the pool). Only the "no pins" configuration exposed it.

## 9. Deviations from the plan / spec
- **IR op set** (D34) and **fault handling without cold stubs in direct mode** (D37). CLAUDE.md §9/§10/§15 are amended by those entries.
- **"Cold stubs for helper calls."** A helper call is the instruction's only path, not a slow path, so the full sync is inline where the call is (D36). Cold stubs hold the exits and the budget check. TLB-miss cold stubs arrive with softmmu (Phase 7).
- **Allocator unit tests on synthetic IR** became guest-code tests plus the fuzzer. `register_pressure_and_division_chains` checks the listed cases (pressure > 7, fixed constraints, back-to-back DIVs) end to end through the real backend. The fuzzer covers random combinations.
- **`--check-abi` dropped** (D41).
- **`--stats=regs` is interpreter-only** (D40).
- **Poletto–Sarkar spills the interval with the furthest end.** This allocator evicts the value with the furthest *next use*, as §10 specifies. On straight-line code that is Belady's choice, and it is never worse.

## 10. Known limitations and technical debt
- **No cross-block allocation.** Only the pinned registers survive TB boundaries. Loop-carried values in other registers are loaded and stored every iteration (loop.elf, §7.1). Superblocks/traces (Phase 10) or a per-loop pinned set would address this.
- **Helper calls are expensive.** They spill every live value and write back everything. CSR/AMO/FP-heavy code gains little until those instructions are lowered natively (FP in Phase 6).
- **Codegen gaps seen in dumps:**
  - `mov r, a; op r, b` pairs where a three-operand form (`andn`, `lea` with shifts) or a better destination choice could save the `mov`.
  - A constant `a` in `Bin` ops is materialized in a register.
  - `slli+srli` pairs (zero-extension idioms) are not recognized as `movzx`.
  - Zero constants use `mov r32, 0` (it keeps flags) rather than `xor`.
  - These are candidates for the P5.3 tuning pass.
- **Spill slots are a fixed 64.** Running out triggers retranslation at half length (never observed).
- **The histogram counts uses, not cross-block liveness,** which §7.3 shows is the wrong proxy for pinning.

## 11. How to reproduce
```
tools/setup.sh && tools/build-guests.sh && tools/build-riscv-tests.sh
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
cargo build --release --tests
PROPTEST_CASES=1000000 $(ls -t target/release/deps/fuzz_blocks-* | grep -v '\.d$' | head -1)
B=target/release/bridgev
for c in "--regalloc none" "--regalloc pinned" "" "--pin x15,x14,x13,x10" "--pin x2,x15,x10,x14"; do
  for i in 0 1 2 3 4 5; do   # run 0 is the warm-up
    taskset -c 2 $B run --engine jit $c --stats guest/build/bench-coremark.bin 0x0 0x0 0x66 150000
    taskset -c 2 $B run --engine jit $c --stats guest/build/fib-O2.elf 32
    taskset -c 2 $B run --engine jit $c --stats guest/build/loop.elf
  done
done
taskset -c 2 $B run --engine jit --pin x6,x5 --stats guest/build/loop.elf
$B run --stats=regs guest/build/bench-coremark.bin 0x0 0x0 0x66 2000      # histogram
# IR and x86 of the worked example:
cargo build --release --features disasm --target-dir target/disasm
target/disasm/release/bridgev run --engine jit --dump-ir /tmp/tbs --dump-x86 /tmp/tbs \
    guest/build/bench-coremark.bin 0x0 0x0 0x66 10
cat /tmp/tbs/tb_0000000000011fac.ir /tmp/tbs/tb_0000000000011fac.txt
```

## 12. Next steps
Phase 5 (Milestone A):
- **P5.1:** build CoreMark reproducibly (not the ad-hoc `bench-coremark.bin`), Dhrystone, and native x86 builds.
- **P5.2:** the `bridgev bench` / `tools/bench.sh` harness for the §22 matrix, with JSON output. This report's table becomes its first real run.
- **P5.3:** the tuning pass, starting with the codegen gaps in §10 and a cross-block liveness histogram to re-test the pinned set.
