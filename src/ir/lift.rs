//! Lifter from decoded `Inst` to IR (CLAUDE.md §9 pass 1, P4.2).
//!
//! Integer, memory and control-flow instructions become IR ops. Everything else (CSR, AMO,
//! FP, EBREAK, xRET, WFI, SFENCE.VMA, illegal) becomes `Interp`, executed by the interpreter
//! helper (D14). Reads of x0 become `Const 0`; writes to x0 are dropped (loads into x0 still
//! access memory).
//!
//! With `LiftOptions::inline_fp` (the fast TB variant, D47) most F/D instructions are lifted
//! too: loads/stores/moves/sign injection become integer IR on the raw register bits;
//! arithmetic, FMA (if the host has FMA3), conversions and compares become FP ops with RNE
//! (static rm = RNE, or dynamic rm, which the TB prologue guards with frm = RNE). FMIN/FMAX,
//! FCLASS, unsigned float→int, u64→float and other rounding modes stay `Interp`.

use crate::cpu::trap::Exception;
use crate::isa::Decoded;
use crate::isa::inst::*;

use super::ops::{BinOp, Block, Cond, ExitKind, FCmpOp, FOp, IntSrc, Op, V};

/// What the lifter may lift instead of calling the interpreter.
#[derive(Clone, Copy, Debug, Default)]
pub struct LiftOptions {
    /// Lift F/D instructions (fast TB variant: FS Dirty, frm RNE for dynamic-rm ops).
    pub inline_fp: bool,
    /// The host has FMA3 (`vfmadd231sd` …).
    pub fma: bool,
}

struct Lifter {
    b: Block,
    opts: LiftOptions,
}

const D_SIGN: u64 = 1 << 63;
const S_SIGN: u64 = 1 << 31;

impl Lifter {
    fn push(&mut self, op: Op) {
        self.b.ops.push(op);
    }

    fn konst(&mut self, imm: u64) -> V {
        let dst = self.b.new_value();
        self.push(Op::Const { dst, imm });
        dst
    }

    fn read(&mut self, g: u8) -> V {
        if g == 0 {
            return self.konst(0);
        }
        let dst = self.b.new_value();
        self.push(Op::ReadReg { dst, g });
        dst
    }

    fn write(&mut self, g: u8, src: V) {
        if g != 0 {
            self.push(Op::WriteReg { g, src });
        }
    }

    fn bin_imm(&mut self, op: BinOp, a: V, imm: i64) -> V {
        let dst = self.b.new_value();
        self.push(Op::BinImm { op, dst, a, imm });
        dst
    }

    fn bin(&mut self, op: BinOp, a: V, b: V) -> V {
        let dst = self.b.new_value();
        self.push(Op::Bin { op, dst, a, b });
        dst
    }

    fn read_f(&mut self, f: u8) -> V {
        let dst = self.b.new_value();
        self.push(Op::ReadF { dst, f });
        dst
    }

    /// Single-precision source: unboxed values read as the canonical NaN.
    fn read_s(&mut self, f: u8) -> V {
        let raw = self.read_f(f);
        let dst = self.b.new_value();
        self.push(Op::Unbox { dst, src: raw });
        dst
    }

    /// FSGNJ/FSGNJN/FSGNJX on the raw bits (singles keep their all-ones box).
    fn sign_inject(&mut self, op: FpOp, dbl: bool, rd: u8, rs1: u8, rs2: u8) {
        let (a, b) = if dbl {
            (self.read_f(rs1), self.read_f(rs2))
        } else {
            (self.read_s(rs1), self.read_s(rs2))
        };
        let sign = if dbl { D_SIGN } else { S_SIGN };
        let sign_c = self.konst(sign);
        let sb = self.bin(BinOp::And, b, sign_c);
        let r = match op {
            FpOp::SgnJx => self.bin(BinOp::Xor, a, sb),
            _ => {
                let keep = self.konst(!sign);
                let mag = self.bin(BinOp::And, a, keep);
                let s = if op == FpOp::SgnJn {
                    self.bin(BinOp::Xor, sb, sign_c)
                } else {
                    sb
                };
                self.bin(BinOp::Or, mag, s)
            }
        };
        self.push(Op::WriteF {
            f: rd,
            src: r,
            single: !dbl,
        });
    }

    /// Lift an F/D instruction inline; false = leave it to the interpreter.
    fn fp(&mut self, d: &Decoded) -> bool {
        // rm: 0 = RNE (static), 7 = dynamic (the prologue guarantees frm = RNE).
        let rne = |rm: u8| match rm {
            0 => Some(false),
            7 => Some(true),
            _ => None,
        };
        match d.inst {
            Inst::FLoad { fmt, rd, rs1, imm } => {
                let addr = self.read(rs1);
                let dst = self.b.new_value();
                let dbl = fmt == FpFmt::D;
                self.push(Op::Load {
                    dst,
                    addr,
                    off: imm as i32,
                    size: if dbl { 8 } else { 4 },
                    signed: false,
                });
                self.push(Op::WriteF {
                    f: rd,
                    src: dst,
                    single: !dbl,
                });
            }
            Inst::FStore { fmt, rs1, rs2, imm } => {
                let addr = self.read(rs1);
                let val = self.read_f(rs2);
                self.push(Op::Store {
                    addr,
                    off: imm as i32,
                    val,
                    size: if fmt == FpFmt::D { 8 } else { 4 },
                });
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
                let Some(dynrm) = rne(rm).filter(|_| self.opts.fma) else {
                    return false;
                };
                let op = match op {
                    FmaOp::Madd => FOp::Madd,
                    FmaOp::Msub => FOp::Msub,
                    FmaOp::Nmsub => FOp::Nmsub,
                    FmaOp::Nmadd => FOp::Nmadd,
                };
                self.push(Op::FArith {
                    op,
                    dbl: fmt == FpFmt::D,
                    rd,
                    rs1,
                    rs2,
                    rs3,
                    dynrm,
                    raw: d.raw,
                });
            }
            Inst::Fp {
                op,
                fmt,
                rd,
                rs1,
                rs2,
                rm,
            } => {
                let dbl = fmt == FpFmt::D;
                match op {
                    FpOp::Add | FpOp::Sub | FpOp::Mul | FpOp::Div | FpOp::Sqrt | FpOp::CvtFmt => {
                        let Some(dynrm) = rne(rm) else {
                            return false;
                        };
                        let op = match op {
                            FpOp::Add => FOp::Add,
                            FpOp::Sub => FOp::Sub,
                            FpOp::Mul => FOp::Mul,
                            FpOp::Div => FOp::Div,
                            FpOp::Sqrt => FOp::Sqrt,
                            _ if dbl => FOp::ToDouble,
                            _ => FOp::ToSingle,
                        };
                        self.push(Op::FArith {
                            op,
                            dbl,
                            rd,
                            rs1,
                            rs2,
                            rs3: 0,
                            dynrm,
                            raw: d.raw,
                        });
                    }
                    FpOp::SgnJ | FpOp::SgnJn | FpOp::SgnJx => {
                        self.sign_inject(op, dbl, rd, rs1, rs2)
                    }
                    FpOp::Eq | FpOp::Lt | FpOp::Le => {
                        let dst = self.b.new_value();
                        self.push(Op::FCmp {
                            op: match op {
                                FpOp::Eq => FCmpOp::Eq,
                                FpOp::Lt => FCmpOp::Lt,
                                _ => FCmpOp::Le,
                            },
                            dbl,
                            dst,
                            rs1,
                            rs2,
                            raw: d.raw,
                        });
                        self.write(rd, dst);
                    }
                    FpOp::MvToInt => {
                        let v = self.read_f(rs1);
                        // FMV.X.W: bits [31:0] sign-extended (no unboxing).
                        let v = if dbl {
                            v
                        } else {
                            self.bin_imm(BinOp::Addw, v, 0)
                        };
                        self.write(rd, v);
                    }
                    FpOp::MvFromInt => {
                        let v = self.read(rs1);
                        self.push(Op::WriteF {
                            f: rd,
                            src: v,
                            single: !dbl,
                        });
                    }
                    FpOp::CvtToInt(t @ (IntTy::W | IntTy::L)) => {
                        let (trunc, dynrm) = match rm {
                            1 => (true, false),
                            _ => match rne(rm) {
                                Some(dy) => (false, dy),
                                None => return false,
                            },
                        };
                        let dst = self.b.new_value();
                        self.push(Op::FToI {
                            dst,
                            dbl,
                            rs1,
                            long: t == IntTy::L,
                            trunc,
                            dynrm,
                            raw: d.raw,
                        });
                        self.write(rd, dst);
                    }
                    FpOp::CvtFromInt(t @ (IntTy::W | IntTy::Wu | IntTy::L)) => {
                        let Some(dynrm) = rne(rm) else {
                            return false;
                        };
                        let src = self.read(rs1);
                        self.push(Op::IToF {
                            rd,
                            dbl,
                            src,
                            from: match t {
                                IntTy::W => IntSrc::W,
                                IntTy::Wu => IntSrc::Wu,
                                _ => IntSrc::L,
                            },
                            dynrm,
                            raw: d.raw,
                        });
                    }
                    _ => return false,
                }
            }
            _ => return false,
        }
        true
    }

    /// Lift one instruction; returns true if it ended the block.
    fn insn(&mut self, d: &Decoded, pc: u64, idx: u32) -> bool {
        let next = pc.wrapping_add(d.len as u64);
        self.push(Op::Insn { pc, idx });
        match d.inst {
            Inst::Lui { rd, imm } => {
                if rd != 0 {
                    let v = self.konst(imm as u64);
                    self.write(rd, v);
                }
            }
            Inst::Auipc { rd, imm } => {
                if rd != 0 {
                    let v = self.konst(pc.wrapping_add(imm as u64));
                    self.write(rd, v);
                }
            }
            Inst::Jal { rd, imm } => {
                if rd != 0 {
                    let v = self.konst(next);
                    self.write(rd, v);
                }
                self.push(Op::Jump {
                    pc: pc.wrapping_add(imm as u64),
                });
                return true;
            }
            Inst::Jalr { rd, rs1, imm } => {
                // Read rs1 before writing rd (rd == rs1 is legal): SSA makes that automatic.
                let a = self.read(rs1);
                let t = self.bin_imm(BinOp::Add, a, imm);
                let target = self.bin_imm(BinOp::And, t, -2);
                if rd != 0 {
                    let v = self.konst(next);
                    self.write(rd, v);
                }
                self.push(Op::JumpInd { target });
                return true;
            }
            Inst::Branch { op, rs1, rs2, imm } => {
                let a = self.read(rs1);
                let b = self.read(rs2);
                let cond = match op {
                    BranchOp::Beq => Cond::Eq,
                    BranchOp::Bne => Cond::Ne,
                    BranchOp::Blt => Cond::Lt,
                    BranchOp::Bge => Cond::Ge,
                    BranchOp::Bltu => Cond::Ltu,
                    BranchOp::Bgeu => Cond::Geu,
                };
                self.push(Op::Branch {
                    cond,
                    a,
                    b,
                    taken: pc.wrapping_add(imm as u64),
                    fall: next,
                });
                return true;
            }
            Inst::Load { op, rd, rs1, imm } => {
                let addr = self.read(rs1);
                let (size, signed) = match op {
                    LoadOp::Lb => (1, true),
                    LoadOp::Lh => (2, true),
                    LoadOp::Lw => (4, true),
                    LoadOp::Ld => (8, false),
                    LoadOp::Lbu => (1, false),
                    LoadOp::Lhu => (2, false),
                    LoadOp::Lwu => (4, false),
                };
                let dst = self.b.new_value();
                self.push(Op::Load {
                    dst,
                    addr,
                    off: imm as i32,
                    size,
                    signed,
                });
                self.write(rd, dst);
            }
            Inst::Store { op, rs1, rs2, imm } => {
                let addr = self.read(rs1);
                let val = self.read(rs2);
                let size = match op {
                    StoreOp::Sb => 1,
                    StoreOp::Sh => 2,
                    StoreOp::Sw => 4,
                    StoreOp::Sd => 8,
                };
                self.push(Op::Store {
                    addr,
                    off: imm as i32,
                    val,
                    size,
                });
            }
            Inst::OpImm { op, rd, rs1, imm } => {
                if rd != 0 {
                    let a = self.read(rs1);
                    let v = self.bin_imm(BinOp::from_alu(op), a, imm);
                    self.write(rd, v);
                }
            }
            Inst::OpImmW { op, rd, rs1, imm } => {
                if rd != 0 {
                    let a = self.read(rs1);
                    let v = self.bin_imm(BinOp::from_aluw(op), a, imm);
                    self.write(rd, v);
                }
            }
            Inst::Op { op, rd, rs1, rs2 } => {
                if rd != 0 {
                    let (a, b) = (self.read(rs1), self.read(rs2));
                    let dst = self.b.new_value();
                    self.push(Op::Bin {
                        op: BinOp::from_alu(op),
                        dst,
                        a,
                        b,
                    });
                    self.write(rd, dst);
                }
            }
            Inst::OpW { op, rd, rs1, rs2 } => {
                if rd != 0 {
                    let (a, b) = (self.read(rs1), self.read(rs2));
                    let dst = self.b.new_value();
                    self.push(Op::Bin {
                        op: BinOp::from_aluw(op),
                        dst,
                        a,
                        b,
                    });
                    self.write(rd, dst);
                }
            }
            // Single hart: x86 TSO already provides every ordering a FENCE can ask for (§18).
            Inst::Fence { .. } => {}
            Inst::FenceI => {
                self.push(Op::Exit {
                    kind: ExitKind::FenceI,
                    pc: next,
                });
                return true;
            }
            Inst::Ecall => {
                self.push(Op::Exit {
                    kind: ExitKind::Ecall,
                    pc,
                });
                return true;
            }
            Inst::FLoad { .. } | Inst::FStore { .. } | Inst::Fma { .. } | Inst::Fp { .. }
                if self.opts.inline_fp && self.fp(d) =>
            {
                self.b.fp_guard = true;
                let n = self.b.ops.len();
                self.b.fp_dyn |= self.b.ops[n.saturating_sub(4)..].iter().any(Op::is_dynrm);
            }
            _ => {
                self.push(Op::Interp {
                    raw: d.raw,
                    pc,
                    idx,
                });
                if d.inst.ends_block() {
                    self.push(Op::Jump { pc: next });
                    return true;
                }
            }
        }
        false
    }
}

/// Lift a decoded block starting at `pc`. `fetch_fault` is raised when execution falls off the
/// end (the next fetch faulted while the block was built). FP instructions go to the
/// interpreter (`lift_with` can lift them).
pub fn lift(insns: &[Decoded], fetch_fault: Option<Exception>, pc: u64) -> Block {
    lift_with(insns, fetch_fault, pc, LiftOptions::default())
}

/// `lift` with options (D47).
pub fn lift_with(
    insns: &[Decoded],
    fetch_fault: Option<Exception>,
    pc: u64,
    opts: LiftOptions,
) -> Block {
    let mut l = Lifter {
        b: Block {
            pc,
            ops: Vec::with_capacity(insns.len() * 4 + 2),
            n_insns: insns.len() as u32,
            nvals: 0,
            fp_guard: false,
            fp_dyn: false,
        },
        opts,
    };
    let mut a = pc;
    for (idx, d) in insns.iter().enumerate() {
        if l.insn(d, a, idx as u32) {
            return l.b;
        }
        a = a.wrapping_add(d.len as u64);
    }
    // Fell off the end: page boundary, block limit, or a fetch fault.
    l.push(match fetch_fault {
        Some(e) => Op::Exit {
            kind: ExitKind::Fault(e),
            pc: a,
        },
        None => Op::Jump { pc: a },
    });
    l.b
}
