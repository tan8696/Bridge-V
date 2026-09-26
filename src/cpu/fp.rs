//! Floating-point execution backed by Berkeley SoftFloat 3e with the RISC-V specialization
//! (CLAUDE.md §17, D13).
//!
//! SoftFloat's rounding-mode numbering (RNE, RTZ, RDN, RUP, RMM = 0..4) and exception-flag
//! bits (NX, UF, OF, DZ, NV = 1, 2, 4, 8, 16) coincide with RISC-V's `frm` and `fflags`, and the
//! RISC-V specialization already produces canonical NaNs, detects tininess after rounding and
//! saturates float→int conversions as RISC-V requires. This module adds NaN-boxing, the
//! dynamic rounding mode, `mstatus.FS` gating and the operations SoftFloat does not provide
//! in RISC-V form (FMIN/FMAX, FCLASS, sign injection, moves).

use crate::cpu::state::CpuState;
use crate::cpu::trap::Exception;
use crate::isa::inst::{FmaOp, FpFmt, FpOp, Inst, IntTy};
use crate::mem::direct::DirectMem;

#[repr(C)]
#[derive(Clone, Copy)]
struct F32 {
    v: u32,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct F64 {
    v: u64,
}

// SoftFloat's API (softfloat.h). On x86-64 glibc, `uint_fast8_t` is u8, `int_fast32_t` and
// `int_fast64_t` are i64, `uint_fast32_t` and `uint_fast64_t` are u64, and `bool` is C _Bool.
unsafe extern "C" {
    fn f32_add(a: F32, b: F32) -> F32;
    fn f32_sub(a: F32, b: F32) -> F32;
    fn f32_mul(a: F32, b: F32) -> F32;
    fn f32_div(a: F32, b: F32) -> F32;
    fn f32_sqrt(a: F32) -> F32;
    fn f32_mulAdd(a: F32, b: F32, c: F32) -> F32;
    fn f32_eq(a: F32, b: F32) -> bool;
    fn f32_lt(a: F32, b: F32) -> bool;
    fn f32_le(a: F32, b: F32) -> bool;
    fn f32_lt_quiet(a: F32, b: F32) -> bool;
    fn f32_isSignalingNaN(a: F32) -> bool;
    fn f32_to_i32(a: F32, rm: u8, exact: bool) -> i64;
    fn f32_to_ui32(a: F32, rm: u8, exact: bool) -> u64;
    fn f32_to_i64(a: F32, rm: u8, exact: bool) -> i64;
    fn f32_to_ui64(a: F32, rm: u8, exact: bool) -> u64;
    fn f32_to_f64(a: F32) -> F64;
    fn f64_add(a: F64, b: F64) -> F64;
    fn f64_sub(a: F64, b: F64) -> F64;
    fn f64_mul(a: F64, b: F64) -> F64;
    fn f64_div(a: F64, b: F64) -> F64;
    fn f64_sqrt(a: F64) -> F64;
    fn f64_mulAdd(a: F64, b: F64, c: F64) -> F64;
    fn f64_eq(a: F64, b: F64) -> bool;
    fn f64_lt(a: F64, b: F64) -> bool;
    fn f64_le(a: F64, b: F64) -> bool;
    fn f64_lt_quiet(a: F64, b: F64) -> bool;
    fn f64_isSignalingNaN(a: F64) -> bool;
    fn f64_to_i32(a: F64, rm: u8, exact: bool) -> i64;
    fn f64_to_ui32(a: F64, rm: u8, exact: bool) -> u64;
    fn f64_to_i64(a: F64, rm: u8, exact: bool) -> i64;
    fn f64_to_ui64(a: F64, rm: u8, exact: bool) -> u64;
    fn f64_to_f32(a: F64) -> F32;
    fn i32_to_f32(a: i32) -> F32;
    fn ui32_to_f32(a: u32) -> F32;
    fn i64_to_f32(a: i64) -> F32;
    fn ui64_to_f32(a: u64) -> F32;
    fn i32_to_f64(a: i32) -> F64;
    fn ui32_to_f64(a: u32) -> F64;
    fn i64_to_f64(a: i64) -> F64;
    fn ui64_to_f64(a: u64) -> F64;
    fn bv_sf_set_rounding_mode(rm: u8);
    fn bv_sf_take_flags() -> u8;
}

const NV: u8 = 16;
const S_SIGN: u32 = 1 << 31;
const D_SIGN: u64 = 1 << 63;
const CANONICAL_NAN_S: u32 = 0x7fc0_0000;
const CANONICAL_NAN_D: u64 = 0x7ff8_0000_0000_0000;

/// Read a single-precision value: a properly NaN-boxed register yields its low 32 bits,
/// anything else reads as the canonical NaN.
#[inline]
pub fn unbox(v: u64) -> u32 {
    if v >> 32 == 0xffff_ffff {
        v as u32
    } else {
        CANONICAL_NAN_S
    }
}

/// NaN-box a single-precision value for storage in a 64-bit f register.
#[inline]
pub fn nanbox(v: u32) -> u64 {
    0xffff_ffff_0000_0000 | v as u64
}

fn is_nan_s(v: u32) -> bool {
    v & 0x7f80_0000 == 0x7f80_0000 && v & 0x007f_ffff != 0
}
fn is_nan_d(v: u64) -> bool {
    v & 0x7ff0_0000_0000_0000 == 0x7ff0_0000_0000_0000 && v & 0x000f_ffff_ffff_ffff != 0
}

/// FCLASS result bits: -inf, -normal, -subnormal, -0, +0, +subnormal, +normal, +inf, sNaN, qNaN.
fn classify(sign: bool, exp_all_ones: bool, exp_zero: bool, frac_zero: bool, quiet: bool) -> u64 {
    let bit = if exp_all_ones {
        if frac_zero {
            if sign { 0 } else { 7 }
        } else if quiet {
            9
        } else {
            8
        }
    } else if exp_zero {
        if frac_zero {
            if sign { 3 } else { 4 }
        } else if sign {
            2
        } else {
            5
        }
    } else if sign {
        1
    } else {
        6
    };
    1 << bit
}

fn fclass_s(v: u32) -> u64 {
    let (e, f) = ((v >> 23) & 0xff, v & 0x7f_ffff);
    classify(v >> 31 != 0, e == 0xff, e == 0, f == 0, f & (1 << 22) != 0)
}
fn fclass_d(v: u64) -> u64 {
    let (e, f) = ((v >> 52) & 0x7ff, v & 0xf_ffff_ffff_ffff);
    classify(v >> 63 != 0, e == 0x7ff, e == 0, f == 0, f & (1 << 51) != 0)
}

impl CpuState {
    /// Resolve an instruction's rounding-mode field (7 = dynamic, from `frm`); reserved
    /// values (5, 6, or an invalid `frm`) make the instruction illegal.
    fn rounding_mode(&self, rm: u8, raw: u32) -> Result<u8, Exception> {
        let r = if rm == 7 { self.frm } else { rm };
        if r > 4 {
            Err(Exception::illegal(raw))
        } else {
            // SAFETY: plain setter of SoftFloat's thread-local rounding mode.
            unsafe { bv_sf_set_rounding_mode(r) };
            Ok(r)
        }
    }

    /// Accrue the exception flags raised by the SoftFloat operation just performed.
    fn accrue_flags(&mut self) {
        // SAFETY: plain accessor of SoftFloat's thread-local flags.
        self.fflags |= unsafe { bv_sf_take_flags() };
    }

    fn freg_s(&self, r: u8) -> u32 {
        unbox(self.f[r as usize])
    }
    fn freg_d(&self, r: u8) -> u64 {
        self.f[r as usize]
    }
    fn set_freg_s(&mut self, r: u8, v: u32) {
        self.f[r as usize] = nanbox(v);
    }
}

/// FMIN/FMAX: a NaN operand is ignored unless both are NaN (then canonical NaN); signaling
/// NaNs raise NV; -0 is less than +0.
fn min_max_s(cpu: &mut CpuState, a: u32, b: u32, max: bool) -> u32 {
    // SAFETY: pure SoftFloat predicates.
    let snan = unsafe { f32_isSignalingNaN(F32 { v: a }) || f32_isSignalingNaN(F32 { v: b }) };
    if snan {
        cpu.fflags |= NV;
    }
    match (is_nan_s(a), is_nan_s(b)) {
        (true, true) => CANONICAL_NAN_S,
        (true, false) => b,
        (false, true) => a,
        _ => {
            // SAFETY: pure SoftFloat predicate; neither operand is a NaN here.
            let a_lt_b =
                unsafe { f32_lt_quiet(F32 { v: a }, F32 { v: b }) } || (a == S_SIGN && b == 0);
            if a_lt_b != max { a } else { b }
        }
    }
}

fn min_max_d(cpu: &mut CpuState, a: u64, b: u64, max: bool) -> u64 {
    // SAFETY: pure SoftFloat predicates.
    let snan = unsafe { f64_isSignalingNaN(F64 { v: a }) || f64_isSignalingNaN(F64 { v: b }) };
    if snan {
        cpu.fflags |= NV;
    }
    match (is_nan_d(a), is_nan_d(b)) {
        (true, true) => CANONICAL_NAN_D,
        (true, false) => b,
        (false, true) => a,
        _ => {
            // SAFETY: pure SoftFloat predicate; neither operand is a NaN here.
            let a_lt_b =
                unsafe { f64_lt_quiet(F64 { v: a }, F64 { v: b }) } || (a == D_SIGN && b == 0);
            if a_lt_b != max { a } else { b }
        }
    }
}

/// Execute an F/D instruction (FLW/FSW/FLD/FSD, FMA, OP-FP).
pub fn exec(
    cpu: &mut CpuState,
    mem: &mut DirectMem,
    inst: &Inst,
    raw: u32,
) -> Result<(), Exception> {
    if !cpu.fp_enabled() {
        return Err(Exception::illegal(raw));
    }
    match *inst {
        Inst::FLoad { fmt, rd, rs1, imm } => {
            let addr = cpu.x[rs1 as usize].wrapping_add(imm as u64);
            cpu.f[rd as usize] = match fmt {
                FpFmt::S => nanbox(mem.load(addr, 4)? as u32),
                FpFmt::D => mem.load(addr, 8)?,
            };
        }
        Inst::FStore { fmt, rs1, rs2, imm } => {
            let addr = cpu.x[rs1 as usize].wrapping_add(imm as u64);
            // FSW stores bits [31:0] as they are, without NaN-unboxing.
            let size = if fmt == FpFmt::S { 4 } else { 8 };
            mem.store(addr, size, cpu.f[rs2 as usize])?;
            return Ok(()); // a store modifies no FP state
        }
        Inst::Fma {
            op,
            fmt,
            rd,
            rs1,
            rs2,
            rs3,
            rm,
        } => {
            cpu.rounding_mode(rm, raw)?;
            // Negation is exact, so the negated forms are mulAdd with flipped signs.
            let (neg_prod, neg_add) = match op {
                FmaOp::Madd => (false, false),
                FmaOp::Msub => (false, true),
                FmaOp::Nmsub => (true, false),
                FmaOp::Nmadd => (true, true),
            };
            match fmt {
                FpFmt::S => {
                    let a = cpu.freg_s(rs1) ^ if neg_prod { S_SIGN } else { 0 };
                    let c = cpu.freg_s(rs3) ^ if neg_add { S_SIGN } else { 0 };
                    // SAFETY: pure SoftFloat arithmetic.
                    let r = unsafe {
                        f32_mulAdd(F32 { v: a }, F32 { v: cpu.freg_s(rs2) }, F32 { v: c })
                    };
                    cpu.set_freg_s(rd, r.v);
                }
                FpFmt::D => {
                    let a = cpu.freg_d(rs1) ^ if neg_prod { D_SIGN } else { 0 };
                    let c = cpu.freg_d(rs3) ^ if neg_add { D_SIGN } else { 0 };
                    // SAFETY: pure SoftFloat arithmetic.
                    let r = unsafe {
                        f64_mulAdd(F64 { v: a }, F64 { v: cpu.freg_d(rs2) }, F64 { v: c })
                    };
                    cpu.f[rd as usize] = r.v;
                }
            }
            cpu.accrue_flags();
        }
        Inst::Fp {
            op,
            fmt,
            rd,
            rs1,
            rs2,
            rm,
        } => exec_op_fp(cpu, op, fmt, rd, rs1, rs2, rm, raw)?,
        _ => return Err(Exception::illegal(raw)),
    }
    cpu.set_fs_dirty();
    Ok(())
}

#[allow(clippy::too_many_arguments)] // mirrors the instruction fields one-to-one
fn exec_op_fp(
    cpu: &mut CpuState,
    op: FpOp,
    fmt: FpFmt,
    rd: u8,
    rs1: u8,
    rs2: u8,
    rm: u8,
    raw: u32,
) -> Result<(), Exception> {
    let rd_u = rd as usize;
    match (op, fmt) {
        (FpOp::Add | FpOp::Sub | FpOp::Mul | FpOp::Div, FpFmt::S) => {
            cpu.rounding_mode(rm, raw)?;
            let (a, b) = (F32 { v: cpu.freg_s(rs1) }, F32 { v: cpu.freg_s(rs2) });
            // SAFETY: pure SoftFloat arithmetic.
            let r = unsafe {
                match op {
                    FpOp::Add => f32_add(a, b),
                    FpOp::Sub => f32_sub(a, b),
                    FpOp::Mul => f32_mul(a, b),
                    _ => f32_div(a, b),
                }
            };
            cpu.set_freg_s(rd, r.v);
        }
        (FpOp::Add | FpOp::Sub | FpOp::Mul | FpOp::Div, FpFmt::D) => {
            cpu.rounding_mode(rm, raw)?;
            let (a, b) = (F64 { v: cpu.freg_d(rs1) }, F64 { v: cpu.freg_d(rs2) });
            // SAFETY: pure SoftFloat arithmetic.
            let r = unsafe {
                match op {
                    FpOp::Add => f64_add(a, b),
                    FpOp::Sub => f64_sub(a, b),
                    FpOp::Mul => f64_mul(a, b),
                    _ => f64_div(a, b),
                }
            };
            cpu.f[rd_u] = r.v;
        }
        (FpOp::Sqrt, FpFmt::S) => {
            cpu.rounding_mode(rm, raw)?;
            // SAFETY: pure SoftFloat arithmetic.
            let r = unsafe { f32_sqrt(F32 { v: cpu.freg_s(rs1) }) };
            cpu.set_freg_s(rd, r.v);
        }
        (FpOp::Sqrt, FpFmt::D) => {
            cpu.rounding_mode(rm, raw)?;
            // SAFETY: pure SoftFloat arithmetic.
            cpu.f[rd_u] = unsafe { f64_sqrt(F64 { v: cpu.freg_d(rs1) }) }.v;
        }
        (FpOp::SgnJ | FpOp::SgnJn | FpOp::SgnJx, FpFmt::S) => {
            let (a, b) = (cpu.freg_s(rs1), cpu.freg_s(rs2));
            let sign = match op {
                FpOp::SgnJ => b & S_SIGN,
                FpOp::SgnJn => !b & S_SIGN,
                _ => (a ^ b) & S_SIGN,
            };
            cpu.set_freg_s(rd, (a & !S_SIGN) | sign);
        }
        (FpOp::SgnJ | FpOp::SgnJn | FpOp::SgnJx, FpFmt::D) => {
            let (a, b) = (cpu.freg_d(rs1), cpu.freg_d(rs2));
            let sign = match op {
                FpOp::SgnJ => b & D_SIGN,
                FpOp::SgnJn => !b & D_SIGN,
                _ => (a ^ b) & D_SIGN,
            };
            cpu.f[rd_u] = (a & !D_SIGN) | sign;
        }
        (FpOp::Min | FpOp::Max, FpFmt::S) => {
            let (a, b) = (cpu.freg_s(rs1), cpu.freg_s(rs2));
            let r = min_max_s(cpu, a, b, op == FpOp::Max);
            cpu.set_freg_s(rd, r);
        }
        (FpOp::Min | FpOp::Max, FpFmt::D) => {
            let (a, b) = (cpu.freg_d(rs1), cpu.freg_d(rs2));
            let r = min_max_d(cpu, a, b, op == FpOp::Max);
            cpu.f[rd_u] = r;
        }
        (FpOp::Eq | FpOp::Lt | FpOp::Le, FpFmt::S) => {
            let (a, b) = (F32 { v: cpu.freg_s(rs1) }, F32 { v: cpu.freg_s(rs2) });
            // SAFETY: pure SoftFloat predicates (FEQ is quiet, FLT/FLE signal on any NaN).
            let r = unsafe {
                match op {
                    FpOp::Eq => f32_eq(a, b),
                    FpOp::Lt => f32_lt(a, b),
                    _ => f32_le(a, b),
                }
            };
            cpu.set_x(rd, r as u64);
        }
        (FpOp::Eq | FpOp::Lt | FpOp::Le, FpFmt::D) => {
            let (a, b) = (F64 { v: cpu.freg_d(rs1) }, F64 { v: cpu.freg_d(rs2) });
            // SAFETY: pure SoftFloat predicates.
            let r = unsafe {
                match op {
                    FpOp::Eq => f64_eq(a, b),
                    FpOp::Lt => f64_lt(a, b),
                    _ => f64_le(a, b),
                }
            };
            cpu.set_x(rd, r as u64);
        }
        (FpOp::Class, FpFmt::S) => cpu.set_x(rd, fclass_s(cpu.freg_s(rs1))),
        (FpOp::Class, FpFmt::D) => cpu.set_x(rd, fclass_d(cpu.freg_d(rs1))),
        // FMV.X.W moves bits [31:0] as they are (no unboxing), sign-extended.
        (FpOp::MvToInt, FpFmt::S) => cpu.set_x(rd, cpu.f[rs1 as usize] as u32 as i32 as u64),
        (FpOp::MvToInt, FpFmt::D) => cpu.set_x(rd, cpu.f[rs1 as usize]),
        (FpOp::MvFromInt, FpFmt::S) => cpu.set_freg_s(rd, cpu.x[rs1 as usize] as u32),
        (FpOp::MvFromInt, FpFmt::D) => cpu.f[rd_u] = cpu.x[rs1 as usize],
        (FpOp::CvtToInt(t), _) => {
            let r = cpu.rounding_mode(rm, raw)?;
            // exact = true: an inexact conversion raises NX. W/WU results are sign-extended.
            // SAFETY: pure SoftFloat conversions.
            let v = unsafe {
                match fmt {
                    FpFmt::S => {
                        let a = F32 { v: cpu.freg_s(rs1) };
                        match t {
                            IntTy::W => f32_to_i32(a, r, true) as i32 as u64,
                            IntTy::Wu => f32_to_ui32(a, r, true) as u32 as i32 as u64,
                            IntTy::L => f32_to_i64(a, r, true) as u64,
                            IntTy::Lu => f32_to_ui64(a, r, true),
                        }
                    }
                    FpFmt::D => {
                        let a = F64 { v: cpu.freg_d(rs1) };
                        match t {
                            IntTy::W => f64_to_i32(a, r, true) as i32 as u64,
                            IntTy::Wu => f64_to_ui32(a, r, true) as u32 as i32 as u64,
                            IntTy::L => f64_to_i64(a, r, true) as u64,
                            IntTy::Lu => f64_to_ui64(a, r, true),
                        }
                    }
                }
            };
            cpu.set_x(rd, v);
        }
        (FpOp::CvtFromInt(t), _) => {
            cpu.rounding_mode(rm, raw)?;
            let s = cpu.x[rs1 as usize];
            // SAFETY: pure SoftFloat conversions.
            unsafe {
                match fmt {
                    FpFmt::S => {
                        let r = match t {
                            IntTy::W => i32_to_f32(s as i32),
                            IntTy::Wu => ui32_to_f32(s as u32),
                            IntTy::L => i64_to_f32(s as i64),
                            IntTy::Lu => ui64_to_f32(s),
                        };
                        cpu.set_freg_s(rd, r.v);
                    }
                    FpFmt::D => {
                        cpu.f[rd_u] = match t {
                            IntTy::W => i32_to_f64(s as i32),
                            IntTy::Wu => ui32_to_f64(s as u32),
                            IntTy::L => i64_to_f64(s as i64),
                            IntTy::Lu => ui64_to_f64(s),
                        }
                        .v;
                    }
                }
            }
        }
        (FpOp::CvtFmt, FpFmt::S) => {
            cpu.rounding_mode(rm, raw)?;
            // SAFETY: pure SoftFloat conversion.
            let r = unsafe { f64_to_f32(F64 { v: cpu.freg_d(rs1) }) };
            cpu.set_freg_s(rd, r.v);
        }
        (FpOp::CvtFmt, FpFmt::D) => {
            cpu.rounding_mode(rm, raw)?;
            // SAFETY: pure SoftFloat conversion.
            cpu.f[rd_u] = unsafe { f32_to_f64(F32 { v: cpu.freg_s(rs1) }) }.v;
        }
    }
    cpu.accrue_flags();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nan_boxing() {
        assert_eq!(unbox(nanbox(0x3f80_0000)), 0x3f80_0000);
        assert_eq!(
            unbox(0x0000_0000_3f80_0000),
            CANONICAL_NAN_S,
            "not boxed → canonical NaN"
        );
    }

    #[test]
    fn fclass_bits() {
        assert_eq!(fclass_s(0xff80_0000), 1 << 0); // -inf
        assert_eq!(fclass_s(0x8000_0000), 1 << 3); // -0
        assert_eq!(fclass_s(0x0000_0001), 1 << 5); // +subnormal
        assert_eq!(fclass_s(0x7f80_0001), 1 << 8); // sNaN
        assert_eq!(fclass_s(CANONICAL_NAN_S), 1 << 9); // qNaN
        assert_eq!(fclass_d(0x3ff0_0000_0000_0000), 1 << 6); // +1.0
    }

    #[test]
    fn softfloat_is_linked_and_rounds() {
        let mut cpu = CpuState::new_user(0);
        cpu.rounding_mode(0, 0).unwrap();
        // 1/3 in single precision, RNE.
        // SAFETY: pure SoftFloat arithmetic.
        let r = unsafe { f32_div(F32 { v: 0x3f80_0000 }, F32 { v: 0x4040_0000 }) };
        assert_eq!(r.v, 0x3eaa_aaab);
        cpu.accrue_flags();
        assert_eq!(cpu.fflags, 1, "inexact");
        assert!(cpu.rounding_mode(5, 0).is_err(), "rm 5 is reserved");
    }
}
