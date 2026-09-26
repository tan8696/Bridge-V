# Phase 06 report: Floating point in the JIT

| Field | Value |
|---|---|
| Phase | 6: Floating point in the JIT (see `docs/ROADMAP.md`) |
| Status | Complete |
| Dates | 2026-09-26 → 2026-09-26 |
| Branch / final commit | `claude/compassionate-babbage-ul3prl` @ the commit that adds this report (code: `b312367` + the `--no-inline-fp` / benchmark commit) |
| Sessions used | 1 (part of the Phase 4–6 session) |

## 1. Summary

Before this phase every F/D instruction in JIT code was a full-sync call into `helper_interp_one`, which runs the interpreter's SoftFloat path. Phase 6 inlines the common FP instructions as SSE2/FMA3 code in the IR back end (`--regalloc pinned|linear`), and keeps SoftFloat for everything x86 cannot match bit-exactly.

**What makes it exact:**
- Tail fix-ups handle what x86 gets wrong: NaN canonicalization, unboxed single-precision operands, float→int saturation, and NV for `(0 × ∞) + qNaN` in FMA.
- fflags come from MXCSR.
- Each TB exists in two variants. The fast one checks in its prologue that `mstatus.FS` is Dirty and, when needed, that `frm` is RNE.

**Evidence:**
- rv64uf/ud pass under jit and lockstep.
- A new FP fuzzer compares bit-exactly against SoftFloat, fflags included: 1,000,000 blocks, clean.
- On a new self-validating FP benchmark (nbody + sgemm + conversions), the full JIT runs **3,508 units/s**:
  - **60×** the same JIT with FP through the helper (58 units/s)
  - **27×** the interpreter
  - **5.6×** `qemu-riscv64`

## 2. Planned vs delivered

| Task ID | Task | Status | Notes |
|---|---|---|---|
| P6.1 | FP register handling | done | f registers stay in `CpuState`. Each inline op loads its operands into XMM0–XMM2, computes, and stores the result, so no XMM value lives across ops. Single-precision reads check the NaN box (an unboxed value reads as the canonical NaN); writes box. Loads, stores, `fmv` and sign injection are integer IR (`ReadF`/`WriteF`/`Unbox`), so they share the integer allocator. |
| P6.2 | Arithmetic fast paths | done | add/sub/mul/div/sqrt and fcvt.s.d/fcvt.d.s are inline when rm = RNE, or DYN with the prologue guaranteeing frm = RNE. The plan said "`TbFlags`"; a prologue guard plus two TB variants turned out simpler (D47). fmadd/fmsub/fnmsub/fnmadd use FMA3 when `cpuid` reports it, else the helper. |
| P6.3 | fflags via MXCSR | done | `enter_jit` loads MXCSR = 0x1F80 (flags clear, RNE, all masked). `exit_jit` ORs the mapped MXCSR flags into `fflags`. `helper_interp_one` folds and resets MXCSR on entry and resets it after the instruction (so fcsr reads and writes see exact values). Tininess matches: SoftFloat's RISC-V specialization detects it after rounding (`init_detectTininess softfloat_tininess_afterRounding`, the same as its 8086-SSE model of x86), and the fuzzer checks UF bit-exactly on subnormal-producing operands. |
| P6.4 | Conversions, compares, min/max, sign ops | done, min/max via helper | fcvt.{w,l}.{s,d} (RNE/DYN/RTZ) inline with a saturation fix-up; fcvt.{s,d}.{w,wu,l} inline. feq via `ucomis`, flt/fle via `comis` (signal on qNaN as RISC-V requires). fsgnj/fsgnjn/fsgnjx are integer bit operations. fmin/fmax, fclass, fcvt from/to unsigned 64-bit (and to unsigned 32-bit), and all non-RNE static rounding modes stay on the helper. |
| P6.5 | FP fuzzing | done | `tests/fuzz_fp.rs`, 1,000,000 cases clean (§6). |
| (P6.6) | FP benchmark | done | `guest/bench/fp/fpbench.c`, in `tools/build-bench.sh` and `tools/bench.py`; `--no-inline-fp` gives the A/B baseline. |

## 3. What was built

### 3.1 Emitter (`src/backend/x86/emit.rs`)
New typed forms, each golden-tested against `iced-x86` for all 16 XMM registers and memory operands (`tests/emitter_golden.rs::sse_scalar_forms`, `fma3_forms`):
- `movs_load`/`movs_store` (MOVSS/MOVSD)
- `sse_arith` (ADD/SUB/MUL/DIV/SQRT SS/SD)
- `comis` (UCOMIS/COMIS)
- `cvt_fp` (CVTSS2SD/CVTSD2SS)
- `cvt_int_to_fp` (CVTSI2SS/SD, 32/64-bit)
- `cvt_fp_to_int` (CVT[T]SS/SD2SI, 32/64-bit)
- `mov_to_xmm`/`mov_from_xmm` (MOVD/MOVQ)
- `ldmxcsr`/`stmxcsr`
- `fma231` (VEX-encoded VFMADD/VFMSUB/VFNMADD/VFNMSUB231 SS/SD)

### 3.2 IR (`src/ir/ops.rs`, `src/ir/lift.rs`, `src/ir/eval.rs`)
- **Data movement, as integer IR:**
  - `ReadF{dst,f}` / `WriteF{f,src,single}`: an f register as a 64-bit integer value. `single` NaN-boxes on write.
  - `Unbox{dst,src}`: a single-precision operand, or the canonical NaN if it is not boxed.
  - Consequence: `fld`/`fsd`/`flw`/`fsw` are ordinary `Load`/`Store` ops (they get fault sites and precise exceptions for free), and `fmv.x.w`/`fmv.w.x`/`fmv.x.d`/`fmv.d.x`/`fsgnj*` are integer ALU ops.
- **Computation:** `FArith{op,dbl,rd,rs1,rs2,rs3,dynrm,raw}`, `FCmp{op,dbl,dst,rs1,rs2,raw}`, `FToI{...}`, `IToF{...}`. They read and write f registers in `CpuState` directly, and keep the raw instruction for the evaluator.
- **Lift options:** `lift_with(insns, ff, pc, LiftOptions { inline_fp, fma })`. When an FP instruction becomes inline IR, the lifter sets `Block::fp_guard` (and `fp_dyn` if it used the dynamic rounding mode). These flags live on the block, not on an op, so dead-code elimination cannot delete the guard. (It did once: `fmv.x.w x0, f1` became dead and took the guard with it; §8.)
- **Evaluator:** `ir::eval` runs `FArith`/`FCmp`/`FToI`/`IToF` through the interpreter's `fp::exec`, so it stays the oracle for every pass (`tests/ir_passes.rs::inline_fp_lift_and_every_pass_match_the_interpreter`).

### 3.3 Lowering (`src/backend/x86/lower_ir.rs`)
Per op, `load_fop` → SSE op → `store_fresult`. After each op that can produce a NaN, `ucomis x, x ; jp fixup`. The fix-ups live at the TB tail, like exit stubs, and jump back:

| Fix-up | When | What it does |
|---|---|---|
| `Canon` | NaN result, or an unboxed single operand | loads the canonical NaN (`0x7FF8…` / `0x7FC00000`, boxed) into the result register. RISC-V never propagates payloads; x86 does, and its default NaN is negative. |
| `FmaNan` | NaN FMA result | RISC-V raises NV for `(0 × ∞) + c` even when `c` is a quiet NaN; x86 FMA3 doesn't. Checks the multiplicands, ORs NV into `fflags`, then canonicalizes. **Found by the fuzzer.** |
| `Saturate` | float→int produced the "integer indefinite" (`0x80…`) | NaN and +overflow become the maximum. −overflow and an exact minimum keep the minimum (spec §11.7). NV already came from MXCSR (x86 raises IE on the same inputs). |

Singles with a NaN-box check: `cmp dword [f_hi], -1 ; jne canon_fixup` before the operation.

### 3.4 fflags and MXCSR (`src/jit/trampoline.rs`, `src/cpu/fp.rs`)
- **`enter_jit`:** `mov dword [rsp], 0x1F80 ; ldmxcsr [rsp]`.
- **`exit_jit`:** `stmxcsr`, map IE→NV (4), ZE→DZ (3), OE→OF (2), UE→UF (1), PE→NX (0), and `or byte [fflags], dl`. DE is ignored.
- **`helper_interp_one`:** folds and resets MXCSR before the interpreted instruction (an `frflags` sees every earlier inline op) and resets it again afterwards. The helper's own flags never leak, and an `fsflags` that clears fflags is not undone by stale MXCSR bits.
- **Rust code:** it never runs with a non-default MXCSR (the rounding mode is always RNE, and only the sticky flag bits change). That keeps LLVM's FP-environment assumption intact (§17 pitfall).

### 3.5 Fast and slow TB variants (D47)
The inline code is only correct when:
- `mstatus.FS` is Dirty. Otherwise FP instructions must trap (FS = Off), or must set FS to Dirty (FS = Initial/Clean), which the interpreter path does.
- For dynamic-rm ops, `frm` is RNE.

So the TB cache is keyed by `(pc, fp_slow)`:
- `fp_slow(cpu)` = `FS != Dirty || frm != 0` picks the variant at dispatch.
- The fast variant's prologue re-checks the condition: `mov r10, [mstatus]; and r10d, 0x6000; cmp r10d, 0x6000; jne` and `cmp byte [frm], 0; jne`. Chaining and the jump cache can reach a TB from code that ran under a different state (after an `fsrm` in an earlier TB, say). A failed check exits with the new reason `FP_VARIANT` (7) before executing anything, and the dispatcher continues with the slow variant.
- The slow variant is the Phase 5 code (every FP instruction through the helper), so it is correct for any state.
- CSR instructions end a TB (§13.2), so `frm` cannot change in the middle of a fast TB.

In practice a Linux program sets FS = Dirty with its first FP instruction and never changes `frm`, so `fp-variant` exits are 0 on every benchmark.

## 4. Design decisions made

- **D47 (new): inline FP in the IR back end.** f registers in memory, XMM0–2 scratch, tail fix-ups, MXCSR→fflags folding at exits and in the helper, fast/slow TB variants with a prologue guard, and the list of helper-only ops.
  - *Alternatives:* keeping FP values in XMM registers across ops (an XMM allocator) would remove a store/load pair per op, but it touches the allocator, spill and fault-site machinery for a modest gain; deferred. Putting frm/FS into a `TbFlags` key would also work, but it needs the same prologue re-check for chained entries, so a separate flags type would add nothing.
- **`--no-inline-fp`:** a `JitOptions::inline_fp` switch that forces the slow variant everywhere, for A/B measurements (like `--no-chain`).
- **Stats fix:** "dispatcher entries per M insns" divided by instructions retired *between* `enter_jit` and `exit_jit`. `helper_interp_one` folds its share into `icount` early (D30), so FP-heavy runs under the helper were under-counted. The dispatcher now counts the `icount` delta per entry.

## 5. Worked example

The inner loop of `energy()` in fpbench (`guest/bench/fp/fpbench.c`, guest 0x10aa0, 17 instructions) as lowered by the default JIT (`--dump-ir`, `--dump-x86`, disassembled with `objdump -M intel`):

```
fld fa3, 8(a5) ; fld fa4, 0(a5) ; fld fa5, 16(a5)
fsub.d fa3, ft1, fa3, dyn ; fsub.d fa4, ft2, fa4, dyn ; fsub.d fa5, ft0, fa5, dyn
fld fa2, 48(a5) ; addiw a4, a4, 1 ; addi a5, a5, 56
fmul.d fa2, fa1, fa2, dyn ; fmul.d fa3, fa3, fa3, dyn
fmadd.d fa4, fa4, fa4, fa3, dyn ; fmadd.d fa5, fa5, fa5, fa4, dyn
fsqrt.d fa5, fa5, dyn ; fdiv.d fa5, fa2, fa5, dyn ; fsub.d fa0, fa0, fa5, dyn
bne a4, a2, -54
```

IR after the passes (excerpt; `load.u8` = unsigned 8-byte load):
```
v0 = x15
v1 = load.u8 [v0+8]      f13 = v1
v3 = load.u8 [v0+0]      f14 = v3
...
f13 = fSub.d f1, f13 (dyn rm)
...
f14 = fMadd.d f14, f14, f13 (dyn rm)
f15 = fSqrt.d f15 (dyn rm)
f15 = fDiv.d f12, f15 (dyn rm)
```

x86 (a5 is pinned in R15; `[rbp+0x128]` = f13):
```
  0: 49 83 e9 11                sub    r9, 0x11                ; budget (17 insns)
  4: 0f 8c ..                   jl     budget_stub
  a: 4c 8b 95 10 05 01 00       mov    r10, [rbp+0x10510]      ; mstatus
 11: 41 81 e2 00 60 00 00       and    r10d, 0x6000            ; FS
 18: 41 81 fa 00 60 00 00       cmp    r10d, 0x6000            ; Dirty?
 1f: 0f 85 ..                   jne    fp_variant_stub
 25: 80 bd c1 01 00 00 00       cmp    byte [rbp+0x1c1], 0     ; frm == RNE? (dyn-rm ops present)
 2c: 0f 85 ..                   jne    fp_variant_stub
 32: 4a 8b 44 3b 08             mov    rax, [rbx+r15+0x8]      ; fld fa3, 8(a5)
 37: 48 89 85 28 01 00 00       mov    [rbp+0x128], rax
 ...
 55: f2 0f 10 85 c8 00 00 00    movsd  xmm0, [rbp+0xc8]        ; ft1
 5d: f2 0f 5c 85 28 01 00 00    subsd  xmm0, [rbp+0x128]       ; - fa3
 65: 66 0f 2e c0                ucomisd xmm0, xmm0             ; NaN?
 69: 0f 8a ..                   jp     canon_fixup
 6f: f2 0f 11 85 28 01 00 00    movsd  [rbp+0x128], xmm0
 ...
11c: f2 0f 10 85 28 01 00 00    movsd  xmm0, [rbp+0x128]       ; fmadd.d fa4, fa4, fa4, fa3
124: f2 0f 10 8d 30 01 00 00    movsd  xmm1, [rbp+0x130]
12c: c4 e2 f1 b9 85 30 01 00 00 vfmadd231sd xmm0, xmm1, [rbp+0x130]
135: 66 0f 2e c0                ucomisd xmm0, xmm0
139: 0f 8a ..                   jp     fmanan_fixup
 ...
172: f2 0f 10 8d 38 01 00 00    movsd  xmm1, [rbp+0x138]       ; fsqrt.d
17a: f2 0f 51 c1                sqrtsd xmm0, xmm1
 ...
198: f2 0f 5e 85 38 01 00 00    divsd  xmm0, [rbp+0x138]       ; fdiv.d
```
Each FP instruction costs:
- a load of its operands
- one SSE/FMA instruction
- a `ucomisd`/`jp` pair (never taken on normal data)
- a store

Under the helper, the same instruction costs a full register sync plus a call into Rust, a decode, and a SoftFloat operation.

## 6. Tests and verification

**New or extended tests:**
- `tests/emitter_golden.rs`: `sse_scalar_forms`, `fma3_forms` (25 tests in the file).
- `tests/fuzz_fp.rs` (new):
  - **Blocks:** random F/D blocks (every OP-FP, FMA, FLD/FSD encoding).
  - **Operands:** biased to NaN payloads, signaling NaNs, ±0, ±∞, subnormals, extreme normals and integer-conversion boundaries, with unboxed singles.
  - **State:** random static and dynamic rounding modes (reserved ones included), random fflags and FS.
  - **Configurations:** linear, pinned, and linear without host features (no FMA3, so FMA goes to the helper).
  - **Variants:** the natural variant, and also the fast variant forced onto every state. The forced fast variant must either exit before executing anything (guard) or match the interpreter.
  - **Comparison:** f and x registers, fflags, frm, mstatus, pc, icount and memory, bit-exactly.
- `tests/ir_passes.rs::inline_fp_lift_and_every_pass_match_the_interpreter`.
- `tests/fuzz_blocks.rs` and `tests/jit_lowering.rs` pick the variant the dispatcher would. `infinite_chained_loop_is_preempted` now runs at every regalloc level.

**Runs:**
```
$ PROPTEST_CASES=1000000 cargo test --release --test fuzz_fp
test fp_blocks_match_softfloat ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 252.17s

$ BRIDGEV_REQUIRE_GUESTS=1 cargo test --release          (108 tests, all suites)
test result: ok. ... 0 failed          (every binary)

rv64uf-p-* / rv64ud-p-*: pass under interp, jit, lockstep, and every regalloc level
(tests/riscv_tests.rs); e.g. rv64ud-p-fadd under jit: fp-variant exits 0, inline fAdd.d with dyn rm.
```
CI (`.github/workflows/ci.yml`: fmt, clippy with and without `--features disasm`, the full test suite with guests) is green on `b312367`.

**Acceptance criteria:**
- ✅ rv64uf/ud pass under jit and lockstep (`tests/riscv_tests.rs`, all six engine configurations).
- ✅ FP fuzzer clean with ≥ 1e6 cases (1,000,000, above).
- ✅ Measured speedup on an FP-heavy benchmark (§7).

## 7. Performance

`python3 tools/bench.py --suite fpbench`. Raw data: [`../bench/2026-09-26-fp-b312367/results.json`](../bench/2026-09-26-fp-b312367/results.json). Host: Intel Xeon @ 2.10 GHz, 4 vCPU, pinned to CPU 2, kernel 6.18.44, rustc 1.94.1, qemu-riscv64 8.2.2. Binary: `b312367` plus the `--no-inline-fp` switch and stats fix (committed with this report). 5 runs after 1 warm-up, median (min–max). Shared cloud VM: expect several percent of noise.

The workload is `fpbench`. One unit is:
- 1000 nbody steps
- a 24×24 single-precision sgemm with row norms
- 4096 int↔FP conversions

Every unit is validated against integer and published references ("FP validated"). A unit is about 702 k guest instructions.

| config | units/s | vs interp | vs native | guest MIPS | units/run | valid |
|---|---:|---:|---:|---:|---:|---:|
| interp | 130 (124–134) | 1.0× | 0.008 | 91 | 863 | 5/5 |
| jit-naive | 55 (54–58) | 0.4× | 0.003 | 38 | 356 | 5/5 |
| jit+chain | 58 (56–61) | 0.4× | 0.003 | 41 | 368 | 5/5 |
| jit+pinned | 3,686 (3,629–3,919) | 28.4× | 0.215 | 2,584 | 23,804 | 5/5 |
| jit+linear | 3,508 (3,469–3,654) | 27.1× | 0.205 | 2,458 | 22,531 | 5/5 |
| jit-helper-fp (`--no-inline-fp`) | 58 (55–59) | 0.4× | 0.003 | 40 | 376 | 5/5 |
| qemu | 629 (606–687) | 4.9× | 0.037 | 442 (est.) | 4,175 | 5/5 |
| native | 17,107 (16,920–17,700) | 132.0× | 1.000 | — | 112,757 | 5/5 |

**Findings:**
- **Inline FP is a 60× win on FP-heavy code** (3,508 vs 58 units/s, same JIT otherwise).
  - The pre-Phase-6 binary (`2024851`) ran the same workload at 64 units/s: one ad-hoc run, not harness data.
  - The helper path is about 10% slower than before because of the MXCSR fold/reset in the helper, which exactness requires.
- **The full JIT is 5.6× faster than QEMU** on this workload (QEMU also uses softfloat helpers).
- **The helper path is slower than the interpreter** (40 vs 91 MIPS). A helper call costs a full state sync, a re-decode, the MXCSR handling and a call into Rust per FP instruction; the interpreter runs pre-decoded instructions in a loop. `jit-naive`/`jit+chain` (the Phase 3 back end, which never inlines FP) have the same problem. It only matters for these measurement configurations.
- **`pinned` beat `linear` here** (3,686 vs 3,508; the ranges barely overlap). Inline FP ops read and write f registers in memory, so the linear allocator's integer caching buys little, and its fault-site bookkeeping makes the TBs a bit longer (27.2 vs 30.0 host bytes/guest insn, but more stores around loads). Not investigated further; it's within the kind of difference D42/D45 treated as noise-adjacent.
- **We are at 20% of native.** The remaining gap is mostly the memory round-trip per FP op (§10).

## 8. Bugs found and fixed

| Symptom | Root cause | Fix | Regression test |
|---|---|---|---|
| Fuzzer: `fmadd` with `0 × ∞ + qNaN` lacked NV | x86 FMA3 raises IE only for an sNaN addend; RISC-V requires NV for 0 × ∞ regardless | `FmaNan` fix-up | `tests/fuzz_fp.proptest-regressions` |
| Fuzzer: a fast TB ran with FS = Initial | `fmv.x.w x0, f1` lifted to a dead value; DCE removed the op and the guard derived from it | guard recorded on `Block::fp_guard`/`fp_dyn` | same file |
| `ir_passes` FP test: wrong `fcvt.d.w` results in the evaluator | `IToF` evaluated through `fp::exec`, which read `x[rs1]` from the CPU instead of the IR value | inject the IR value into `x[rs1]` for the call, then restore | `tests/ir_passes.rs` |
| lib test SIGSEGV | a trampoline unit test's block clobbered RBP, which `exit_jit` now uses for the MXCSR fold | test block fixed | `jit::trampoline::tests` |
| `fuzz_blocks` / `jit_lowering` mismatches after the variant split | tests always asked for the fast variant | use `fp_slow(cpu)` like the dispatcher | those tests |
| Stats: 33,414 "dispatcher entries per M insns" with `--no-inline-fp` | helper-retired instructions missing from the denominator | count the `icount` delta per entry (§4) | visible in the results table |

## 9. Deviations from the plan / spec

- **P6.2's "DYN with frm = RNE checked at translate time via `TbFlags`"** became a prologue guard plus two TB variants (D47). The effect is the same, and chained entries stay correct.
- **§17 F2 listed fmin/fmax "careful inline".** They stay on the helper, along with fclass, the unsigned 64-bit conversions, fcvt.wu.* and non-RNE static rounding modes. They don't occur in the hot loops of the benchmarks; add them if a workload needs them.
- **The roadmap put the FP benchmark under P6.5.** It is reported here as P6.6.

## 10. Known limitations and technical debt

- **No XMM register allocation:** every FP op loads its operands from and stores its result to `CpuState`. That is the main cost left (§5). A per-TB XMM cache (like the integer lazy write-back) is the obvious next step.
- **Helper-only FP ops:**
  - fmin/fmax, fclass
  - fcvt.lu/wu.* and fcvt.*.lu
  - RMM/RTZ/RDN/RUP static rounding for arithmetic (RTZ *is* inline for float→int)
- **FMA needs FMA3.** Without it (`--no-host-features`, or old CPUs) FMA goes to SoftFloat.
- **The naive back end (`--regalloc none`) never inlines FP.**

## 11. How to reproduce

```
tools/setup.sh                                  # apt packages (once per container)
cargo build --release
tools/build-riscv-tests.sh && tools/build-guests.sh && tools/build-bench.sh
BRIDGEV_REQUIRE_GUESTS=1 cargo test --release   # everything, including rv64uf/ud under all engines
PROPTEST_CASES=1000000 cargo test --release --test fuzz_fp
python3 tools/bench.py --suite fpbench          # the table in §7 (about 8 minutes)
target/release/bridgev run --engine jit --dump-ir /tmp/ir --dump-x86 /tmp/x86 \
    guest/build/bench/fpbench-rv64.elf 2        # §5 (TB 0x10aa0)
```

## 12. Next steps

Phase 7 (privileged architecture, Sv39 and the inline software TLB) starts with P7.1–P7.4:
- the CSR gaps found by `rv64mi-p-breakpoint`/`illegal`
- the Sv39 walker (`rv64si-p-dirty`, `icache-alias` and every `-v-` test need it)
- routing interpreter memory accesses through the TLB
