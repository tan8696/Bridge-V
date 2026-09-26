//! Lifter from decoded `Inst` to IR (CLAUDE.md §9 pass 1, P4.2).
//!
//! Integer, memory and control-flow instructions become IR ops. Everything else (CSR, AMO,
//! FP, EBREAK, xRET, WFI, SFENCE.VMA, illegal) becomes `Interp`, executed by the interpreter
//! helper (D14). Reads of x0 become `Const 0`; writes to x0 are dropped (loads into x0 still
//! access memory).

use crate::cpu::trap::Exception;
use crate::isa::Decoded;
use crate::isa::inst::*;

use super::ops::{BinOp, Block, Cond, ExitKind, Op, V};

struct Lifter {
    b: Block,
}

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
/// end (the next fetch faulted while the block was built).
pub fn lift(insns: &[Decoded], fetch_fault: Option<Exception>, pc: u64) -> Block {
    let mut l = Lifter {
        b: Block {
            pc,
            ops: Vec::with_capacity(insns.len() * 4 + 2),
            n_insns: insns.len() as u32,
            nvals: 0,
        },
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
