//! Compressed (RVC) instruction expansion to the equivalent 32-bit `Inst` (CLAUDE.md §7.4).
//!
//! Follows the RV64C tables of the unprivileged spec (chapter "C" Standard Extension). HINT
//! encodings (e.g. `c.addi x0, imm`, `c.slli x0`, `c.mv x0, rs2`) expand to ordinary
//! instructions writing `x0`, which makes them architectural no-ops. Reserved encodings expand
//! to `Inst::Illegal`.

use super::decode::sext;
use super::inst::*;

#[inline]
fn b(x: u16, hi: u32, lo: u32) -> u32 {
    ((x as u32) >> lo) & ((1 << (hi - lo + 1)) - 1)
}

/// 3-bit register field `rd'`/`rs1'`/`rs2'` → x8..x15 (or f8..f15).
#[inline]
fn creg(v: u32) -> u8 {
    (v + 8) as u8
}

/// Expand a 16-bit compressed instruction.
pub fn expand(x: u16) -> Inst {
    let ill = Inst::Illegal(x as u32);
    let rd_full = b(x, 11, 7) as u8; // rd / rs1 in CR/CI formats
    let rs2_full = b(x, 6, 2) as u8;
    let rdp = creg(b(x, 4, 2)); // rd' / rs2' (CIW, CL, CS, CA)
    let rs1p = creg(b(x, 9, 7)); // rs1' / rd' (CL, CS, CB, CA)
    // CI-format 6-bit signed immediate: imm[5] = inst[12], imm[4:0] = inst[6:2].
    let ci_imm = sext(((b(x, 12, 12) << 5) | b(x, 6, 2)) as u64, 6);
    // CI-format 6-bit shift amount (unsigned).
    let shamt = ((b(x, 12, 12) << 5) | b(x, 6, 2)) as i64;

    match (b(x, 1, 0), b(x, 15, 13)) {
        // ---------------- Quadrant 0 ----------------
        (0, 0b000) => {
            // C.ADDI4SPN: nzuimm[5:4|9:6|2|3] = inst[12:11|10:7|6|5]
            let imm =
                (b(x, 12, 11) << 4) | (b(x, 10, 7) << 6) | (b(x, 6, 6) << 2) | (b(x, 5, 5) << 3);
            if imm == 0 {
                return ill; // includes the all-zero instruction
            }
            Inst::OpImm {
                op: AluOp::Add,
                rd: rdp,
                rs1: 2,
                imm: imm as i64,
            }
        }
        (0, 0b001) => Inst::FLoad {
            fmt: FpFmt::D,
            rd: rdp,
            rs1: rs1p,
            imm: cl_d_imm(x),
        },
        (0, 0b010) => Inst::Load {
            op: LoadOp::Lw,
            rd: rdp,
            rs1: rs1p,
            imm: cl_w_imm(x),
        },
        (0, 0b011) => Inst::Load {
            op: LoadOp::Ld,
            rd: rdp,
            rs1: rs1p,
            imm: cl_d_imm(x),
        },
        (0, 0b100) => ill, // reserved
        (0, 0b101) => Inst::FStore {
            fmt: FpFmt::D,
            rs1: rs1p,
            rs2: rdp,
            imm: cl_d_imm(x),
        },
        (0, 0b110) => Inst::Store {
            op: StoreOp::Sw,
            rs1: rs1p,
            rs2: rdp,
            imm: cl_w_imm(x),
        },
        (0, 0b111) => Inst::Store {
            op: StoreOp::Sd,
            rs1: rs1p,
            rs2: rdp,
            imm: cl_d_imm(x),
        },

        // ---------------- Quadrant 1 ----------------
        // C.NOP / C.ADDI (rd = 0 or imm = 0 are HINTs)
        (1, 0b000) => Inst::OpImm {
            op: AluOp::Add,
            rd: rd_full,
            rs1: rd_full,
            imm: ci_imm,
        },
        // C.ADDIW (RV64; rd = 0 reserved)
        (1, 0b001) => {
            if rd_full == 0 {
                return ill;
            }
            Inst::OpImmW {
                op: AluWOp::Addw,
                rd: rd_full,
                rs1: rd_full,
                imm: ci_imm,
            }
        }
        // C.LI (rd = 0 is a HINT)
        (1, 0b010) => Inst::OpImm {
            op: AluOp::Add,
            rd: rd_full,
            rs1: 0,
            imm: ci_imm,
        },
        (1, 0b011) => {
            if rd_full == 2 {
                // C.ADDI16SP: nzimm[9|4|6|8:7|5] = inst[12|6|5|4:3|2]
                let v = (b(x, 12, 12) << 9)
                    | (b(x, 6, 6) << 4)
                    | (b(x, 5, 5) << 6)
                    | (b(x, 4, 3) << 7)
                    | (b(x, 2, 2) << 5);
                if v == 0 {
                    return ill;
                }
                Inst::OpImm {
                    op: AluOp::Add,
                    rd: 2,
                    rs1: 2,
                    imm: sext(v as u64, 10),
                }
            } else {
                // C.LUI: nzimm[17|16:12] = inst[12|6:2]; nzimm = 0 reserved; rd = 0 HINT
                let v = (b(x, 12, 12) << 17) | (b(x, 6, 2) << 12);
                if v == 0 {
                    return ill;
                }
                Inst::Lui {
                    rd: rd_full,
                    imm: sext(v as u64, 18),
                }
            }
        }
        (1, 0b100) => match b(x, 11, 10) {
            0b00 => Inst::OpImm {
                op: AluOp::Srl,
                rd: rs1p,
                rs1: rs1p,
                imm: shamt,
            },
            0b01 => Inst::OpImm {
                op: AluOp::Sra,
                rd: rs1p,
                rs1: rs1p,
                imm: shamt,
            },
            0b10 => Inst::OpImm {
                op: AluOp::And,
                rd: rs1p,
                rs1: rs1p,
                imm: ci_imm,
            },
            _ => {
                let (rd, rs1, rs2) = (rs1p, rs1p, rdp);
                match (b(x, 12, 12), b(x, 6, 5)) {
                    (0, 0b00) => Inst::Op {
                        op: AluOp::Sub,
                        rd,
                        rs1,
                        rs2,
                    },
                    (0, 0b01) => Inst::Op {
                        op: AluOp::Xor,
                        rd,
                        rs1,
                        rs2,
                    },
                    (0, 0b10) => Inst::Op {
                        op: AluOp::Or,
                        rd,
                        rs1,
                        rs2,
                    },
                    (0, 0b11) => Inst::Op {
                        op: AluOp::And,
                        rd,
                        rs1,
                        rs2,
                    },
                    (1, 0b00) => Inst::OpW {
                        op: AluWOp::Subw,
                        rd,
                        rs1,
                        rs2,
                    },
                    (1, 0b01) => Inst::OpW {
                        op: AluWOp::Addw,
                        rd,
                        rs1,
                        rs2,
                    },
                    _ => ill,
                }
            }
        },
        // C.J: imm[11|4|9:8|10|6|7|3:1|5] = inst[12|11|10:9|8|7|6|5:3|2]
        (1, 0b101) => {
            let v = (b(x, 12, 12) << 11)
                | (b(x, 11, 11) << 4)
                | (b(x, 10, 9) << 8)
                | (b(x, 8, 8) << 10)
                | (b(x, 7, 7) << 6)
                | (b(x, 6, 6) << 7)
                | (b(x, 5, 3) << 1)
                | (b(x, 2, 2) << 5);
            Inst::Jal {
                rd: 0,
                imm: sext(v as u64, 12),
            }
        }
        // C.BEQZ / C.BNEZ: imm[8|4:3] = inst[12|11:10], imm[7:6|2:1|5] = inst[6:5|4:3|2]
        (1, f @ (0b110 | 0b111)) => {
            let v = (b(x, 12, 12) << 8)
                | (b(x, 11, 10) << 3)
                | (b(x, 6, 5) << 6)
                | (b(x, 4, 3) << 1)
                | (b(x, 2, 2) << 5);
            Inst::Branch {
                op: if f == 0b110 {
                    BranchOp::Beq
                } else {
                    BranchOp::Bne
                },
                rs1: rs1p,
                rs2: 0,
                imm: sext(v as u64, 9),
            }
        }

        // ---------------- Quadrant 2 ----------------
        // C.SLLI (rd = 0 is a HINT)
        (2, 0b000) => Inst::OpImm {
            op: AluOp::Sll,
            rd: rd_full,
            rs1: rd_full,
            imm: shamt,
        },
        // C.FLDSP: uimm[5|4:3|8:6] = inst[12|6:5|4:2]
        (2, 0b001) => Inst::FLoad {
            fmt: FpFmt::D,
            rd: rd_full,
            rs1: 2,
            imm: ci_sp_d_imm(x),
        },
        // C.LWSP: uimm[5|4:2|7:6] = inst[12|6:4|3:2]; rd = 0 reserved
        (2, 0b010) => {
            if rd_full == 0 {
                return ill;
            }
            let v = (b(x, 12, 12) << 5) | (b(x, 6, 4) << 2) | (b(x, 3, 2) << 6);
            Inst::Load {
                op: LoadOp::Lw,
                rd: rd_full,
                rs1: 2,
                imm: v as i64,
            }
        }
        // C.LDSP; rd = 0 reserved
        (2, 0b011) => {
            if rd_full == 0 {
                return ill;
            }
            Inst::Load {
                op: LoadOp::Ld,
                rd: rd_full,
                rs1: 2,
                imm: ci_sp_d_imm(x),
            }
        }
        (2, 0b100) => match (b(x, 12, 12), rd_full, rs2_full) {
            (0, 0, 0) => ill,                                 // C.JR with rs1 = 0: reserved
            (0, rs1, 0) => Inst::Jalr { rd: 0, rs1, imm: 0 }, // C.JR
            (0, rd, rs2) => Inst::Op {
                op: AluOp::Add,
                rd,
                rs1: 0,
                rs2,
            }, // C.MV
            (1, 0, 0) => Inst::Ebreak,                        // C.EBREAK
            (1, rs1, 0) => Inst::Jalr { rd: 1, rs1, imm: 0 }, // C.JALR
            (_, rd, rs2) => Inst::Op {
                op: AluOp::Add,
                rd,
                rs1: rd,
                rs2,
            }, // C.ADD
        },
        // C.FSDSP / C.SDSP: uimm[5:3|8:6] = inst[12:10|9:7]
        (2, 0b101) => Inst::FStore {
            fmt: FpFmt::D,
            rs1: 2,
            rs2: rs2_full,
            imm: css_d_imm(x),
        },
        // C.SWSP: uimm[5:2|7:6] = inst[12:9|8:7]
        (2, 0b110) => {
            let v = (b(x, 12, 9) << 2) | (b(x, 8, 7) << 6);
            Inst::Store {
                op: StoreOp::Sw,
                rs1: 2,
                rs2: rs2_full,
                imm: v as i64,
            }
        }
        (2, 0b111) => Inst::Store {
            op: StoreOp::Sd,
            rs1: 2,
            rs2: rs2_full,
            imm: css_d_imm(x),
        },
        _ => ill, // quadrant 3 is not a compressed instruction
    }
}

/// CL/CS word offset: uimm[5:3] = inst[12:10], uimm[2] = inst[6], uimm[6] = inst[5].
fn cl_w_imm(x: u16) -> i64 {
    ((b(x, 12, 10) << 3) | (b(x, 6, 6) << 2) | (b(x, 5, 5) << 6)) as i64
}

/// CL/CS doubleword offset: uimm[5:3] = inst[12:10], uimm[7:6] = inst[6:5].
fn cl_d_imm(x: u16) -> i64 {
    ((b(x, 12, 10) << 3) | (b(x, 6, 5) << 6)) as i64
}

/// CI sp-relative doubleword offset: uimm[5] = inst[12], uimm[4:3] = inst[6:5], uimm[8:6] = inst[4:2].
fn ci_sp_d_imm(x: u16) -> i64 {
    ((b(x, 12, 12) << 5) | (b(x, 6, 5) << 3) | (b(x, 4, 2) << 6)) as i64
}

/// CSS sp-relative doubleword offset: uimm[5:3] = inst[12:10], uimm[8:6] = inst[9:7].
fn css_d_imm(x: u16) -> i64 {
    ((b(x, 12, 10) << 3) | (b(x, 9, 7) << 6)) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserved_encodings_are_illegal() {
        for raw in [
            0x0000u16, // all zero (C.ADDI4SPN nzuimm = 0)
            0x0004,    // C.ADDI4SPN rd'=x9 nzuimm = 0
            0x8000,    // Q0 funct3 = 100
            0x2001,    // C.ADDIW rd = 0
            0x6081,    // C.LUI rd = x1, nzimm = 0
            0x6101,    // C.ADDI16SP nzimm = 0
            0x8002,    // C.JR rs1 = 0
            0x4002,    // C.LWSP rd = 0
            0x6002,    // C.LDSP rd = 0
            0x9c41,    // C.SUBW-space funct2 = 10 with inst[12] = 1 (reserved)
        ] {
            assert_eq!(expand(raw), Inst::Illegal(raw as u32), "{raw:#06x}");
        }
    }

    #[test]
    fn hints_are_nops() {
        // c.nop
        assert_eq!(
            expand(0x0001),
            Inst::OpImm {
                op: AluOp::Add,
                rd: 0,
                rs1: 0,
                imm: 0
            }
        );
        // c.li x0, 1 (HINT) writes x0
        assert!(matches!(expand(0x4005), Inst::OpImm { rd: 0, .. }));
    }
}
