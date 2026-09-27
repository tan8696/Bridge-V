# 14 · Floating point: small differences, big consequences

## What you will learn

- the basics of IEEE 754 floating point (sign, exponent, mantissa, special values)
- rounding modes and exception flags
- the RISC-V rules that differ from x86: **NaN-boxing**, **canonical NaNs**, **saturating conversions**, FMA flags, the FS field
- Bridge-V's two-step approach: exact **SoftFloat** first, fast **inline SSE** second
- the fix-up code that makes x86 results match RISC-V bit for bit
- the two **variants** of every FP-using block, and how the dispatcher picks one

Files: [`src/cpu/fp.rs`](../../src/cpu/fp.rs) (interpreter, SoftFloat), the FP part of [`src/ir/lift.rs`](../../src/ir/lift.rs), `farith()`/`fcmp()`/`ftoi()`/`itof()` and the fix-ups in [`src/backend/x86/lower_ir.rs`](../../src/backend/x86/lower_ir.rs). Design decisions D13 and D47.

---

## 1. IEEE 754 in five minutes

A floating-point number is stored as three fields:

```
double (64 bits):  [sign: 1][exponent: 11][mantissa (fraction): 52]
float  (32 bits):  [sign: 1][exponent: 8 ][mantissa: 23]
value ≈ (−1)^sign × 1.mantissa × 2^(exponent − bias)
```

Special values:
| Value | Encoding | Example use |
|---|---|---|
| ±0 | exponent 0, mantissa 0 | `-0.0` exists and is different from `+0.0` in some operations |
| subnormal | exponent 0, mantissa ≠ 0 | tiny numbers close to 0 |
| ±∞ | exponent all ones, mantissa 0 | `1.0 / 0.0` |
| NaN | exponent all ones, mantissa ≠ 0 | "not a number": `0.0 / 0.0`, `sqrt(-1)` |

NaNs come in two kinds: **quiet** (normal) and **signaling** (raises an exception when used). The mantissa bits of a NaN are called its **payload**.

### 1.1 Rounding

Most results can't be represented exactly, so they are rounded. RISC-V has five **rounding modes**:

| Code | Name | Meaning |
|---|---|---|
| 0 | RNE | round to nearest, ties to even (the default) |
| 1 | RTZ | round towards zero (truncate) |
| 2 | RDN | round down (towards −∞) |
| 3 | RUP | round up (towards +∞) |
| 4 | RMM | round to nearest, ties away from zero (x86 doesn't have this one) |
| 7 | DYN | use the dynamic mode from the `frm` register |

Each FP instruction carries a 3-bit `rm` field: either a fixed mode or DYN.

### 1.2 Exception flags

FP operations don't trap on RISC-V. Instead they set sticky bits in `fflags` (they stay set until software clears them):

| Flag | Meaning | x86 MXCSR equivalent |
|---|---|---|
| NV | invalid operation (e.g. 0/0, sqrt(−1)) | IE |
| DZ | divide by zero | ZE |
| OF | overflow | OE |
| UF | underflow | UE |
| NX | inexact (the result was rounded) | PE |

`fcsr` = `frm` (3 bits) + `fflags` (5 bits).

---

## 2. Where RISC-V and x86 disagree

x86's SSE instructions compute the same IEEE results in most cases, but a few details differ. Each one must be fixed, or programs get subtly different answers.

1. **NaN results.** RISC-V always produces the **canonical NaN** (`0x7FF8000000000000` for double, `0x7FC00000` for float) and never passes a payload through. x86 propagates the payload of an input NaN, and its default NaN is *negative* (`0xFFF8…`). So every NaN result on x86 must be replaced.
2. **NaN-boxing.** RISC-V's FP registers are 64 bits. A single-precision value is stored with the upper 32 bits **all ones** ("boxed"). If an instruction reads a single from a register that isn't properly boxed (e.g. it holds a double), it must treat the value as the canonical NaN.
3. **Float → integer conversion.** For NaN or out-of-range inputs, x86 `cvttsd2si` returns `0x8000…` (the "integer indefinite"). RISC-V **saturates**: too large → the maximum integer, too small → the minimum, NaN → the maximum.
4. **`fmin` / `fmax`.** They differ from x86 `minsd`/`maxsd` on NaNs and on −0 vs +0.
5. **FMA flags.** For `0 × ∞ + qNaN`, RISC-V raises NV; x86's FMA3 instruction doesn't. (The FP fuzzer found this one.)
6. **Rounding mode RMM** has no x86 equivalent at all.
7. **`mstatus.FS`** (FP state: Off, Initial, Clean, Dirty). With FS = Off, every FP instruction is illegal; Linux relies on this to save FP registers lazily. Any FP write must set FS = Dirty.

---

## 3. Step 1: exact software floating point (Phase 1)

The interpreter uses **Berkeley SoftFloat 3e** with its RISC-V specialization ([`third_party/berkeley-softfloat-3`](../../third_party/), compiled by `build.rs`). SoftFloat computes IEEE arithmetic in software, with integer operations, for every rounding mode, and it produces RISC-V's exact NaNs, flags and saturation. It is what Spike (the official RISC-V reference simulator) uses, so it is the gold standard.

[`src/cpu/fp.rs`](../../src/cpu/fp.rs) wraps it and adds what SoftFloat doesn't know about: NaN-boxing, the dynamic rounding mode, the FS check, `fmin`/`fmax`, `fclass`, sign injection and moves.

This is correct but slow (thousands of host instructions per FP operation). In Phase 5, before inline FP, every FP instruction in the JIT also went through the helper: a full register sync plus SoftFloat. The FP benchmark ran at only 58 units/s.

**A Rust pitfall:** the Rust compiler assumes the x86 FP control register (MXCSR) is always in its default state. Changing the rounding mode and then doing FP math in Rust would be undefined behaviour. So Rust code in Bridge-V never changes MXCSR around its own FP math; non-default rounding goes through SoftFloat.

---

## 4. Step 2: inline SSE with fix-ups (Phase 6)

The IR back end translates the common FP instructions directly to SSE2 / FMA3 instructions, **only for round-to-nearest-even** (static `rm = RNE`, or `DYN` when `frm` is RNE; `RTZ` too for float → int):

| Inline (fast) | Still through the helper (SoftFloat) |
|---|---|
| `fadd`, `fsub`, `fmul`, `fdiv`, `fsqrt` (S and D) | other rounding modes (RTZ, RDN, RUP, RMM) |
| `fmadd`, `fmsub`, `fnmadd`, `fnmsub` (needs FMA3) | `fmin`, `fmax`, `fclass` |
| `fcvt.s.d`, `fcvt.d.s` | unsigned float → int, u64 → float |
| `feq`, `flt`, `fle` | |
| `fcvt.{w,l}.{s,d}` (float → signed int) | |
| `fcvt.{s,d}.{w,wu,l}` (int → float) | |
| `flw`, `fld`, `fsw`, `fsd`, `fmv`, sign injection (as plain integer IR) | |

FP registers stay in `CpuState.f[]`; each inline operation uses XMM0–XMM2 as scratch.

### 4.1 An inline `fadd.d` with its fix-ups

```
movsd   xmm0, [rbp + f[rs1]]       ; load operand 1
addsd   xmm0, [rbp + f[rs2]]       ; x86 addition (sets MXCSR flags)
ucomisd xmm0, xmm0                 ; is the result a NaN? (NaN ≠ NaN sets the parity flag)
jp      .canon                     ; rare: go fix it (cold code at the block's end)
.back:
movsd   [rbp + f[rd]], xmm0        ; store the result
...
.canon:                            ; cold fix-up
movabs  r11, 0x7FF8000000000000    ; the canonical double NaN
movq    xmm0, r11
jmp     .back
```

The common case is 4–5 instructions; the NaN fix-up is out of the way in the block's cold tail (`emit_fixups()`).

### 4.2 The other fix-ups

- **Single-precision operands** (`load_fop`): check that the upper 32 bits of `f[rs]` are all ones (`cmp dword [f_hi], -1`). If not, the operand becomes the canonical NaN.
- **Single-precision results** (`store_fresult`): write the 32-bit result and set the upper half to all ones (boxing).
- **Float → int** (`ftoi`): after `cvttsd2si` / `cvtsd2si`, compare the result with the integer indefinite (`0x8000…`). If equal, a cold **Saturate** fix-up decides: NaN or positive overflow → maximum; negative overflow → minimum (and the true minimum stays).
- **FMA** (`FmaNan`): if the result is NaN, check whether the multiplicands were 0 and ∞ and set NV if so, then canonicalize.
- **Comparisons**: `feq` uses a quiet compare (`ucomisd`) and must return 0 for unordered (NaN) inputs; `flt`/`fle` use a signaling compare (`comisd`), mirroring RISC-V's exception rules.

### 4.3 Flags: MXCSR → fflags

x86 records FP exceptions in MXCSR, not per instruction. Bridge-V lets them accumulate while generated code runs:
- `enter_jit` loads the default MXCSR (flags clear, RNE).
- `exit_jit` reads MXCSR, maps IE/ZE/OE/UE/PE to NV/DZ/OF/UF/NX, and ORs them into `cpu.fflags`.
- `helper_interp_one` does the same fold before running an instruction (so SoftFloat sees exact `fflags`) and resets MXCSR afterwards.

Because `fflags` is sticky, folding later instead of after every instruction gives the same final value.

---

## 5. Two variants of every FP block

Inline code is only correct when:
- `mstatus.FS` is **Dirty** (otherwise FP instructions must trap or must mark the state dirty), and
- `frm` is **RNE**, if the block uses the dynamic rounding mode.

So a block containing FP can be translated two ways (decision D47), and TBs are keyed by `(pc, fp_slow)`:
- **fast variant**: inline SSE. Its prologue checks the conditions:
  ```
  mov r10, [rbp + mstatus] ; and r10d, FS ; cmp r10d, FS ; jne fp_variant_stub
  cmp byte [rbp + frm], 0  ; jne fp_variant_stub        (only if it has dynamic-rm ops)
  ```
  If they don't hold, it exits with reason `FP_VARIANT` **before executing anything**.
- **slow variant**: every FP instruction goes through the helper (SoftFloat), which handles every case.

`fp_slow(cpu)` in `dispatch.rs` picks the variant that matches the current state. The `Block::fp_guard` / `fp_dyn` flags remember that the guard is needed even if optimization later removed all the FP ops (so dead-code elimination can't drop the guard). `--no-inline-fp` forces the slow variant everywhere, for comparison.

---

## 6. Testing and results

[`tests/fuzz_fp.rs`](../../tests/fuzz_fp.rs) generates random FP blocks with operands biased towards the nasty cases (NaNs with payloads, signaling NaNs, ±0, ±∞, subnormals, extreme values, integer conversion boundaries), random rounding modes (including invalid ones), random `fflags` and `FS`. The JIT must match the SoftFloat interpreter **bit for bit**: every f and x register, `fflags`, `frm`, `mstatus`, `pc`, `icount` and memory. It was run for **1,000,000 cases**, and it found the FMA NV difference described above.

Performance on the FP benchmark (`guest/bench/fp/fpbench.c`: an n-body simulation, a matrix multiply, and conversions):

| | units/s |
|---|---:|
| JIT, all FP through the helper (`--no-inline-fp`) | 58 |
| JIT, inline FP (Phase 6) | 3,508 (**60×**) |
| JIT at the final commit | 3,711 |
| QEMU | 636 (Bridge-V is **5.83×** faster) |
| native x86-64 | 16,589 |

---

## Check yourself

1. What are the three fields of a double? What bit patterns mean ∞ and NaN?
2. What does "ties to even" mean? Why does RISC-V have a DYN rounding mode?
3. What are NaN-boxing and the canonical NaN? What does x86 do differently?
4. What does RISC-V return for `fcvt.w.d` of NaN? Of 1e30? What does x86 return?
5. Why is SoftFloat used, and why is it not enough on its own?
6. Explain the inline `fadd.d` sequence, including the `jp` fix-up.
7. How do x86's exception flags end up in RISC-V's `fflags`?
8. Why do FP blocks have two variants? What does the fast variant's prologue check?
