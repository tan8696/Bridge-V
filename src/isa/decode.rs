//! 32-bit instruction decoder for RV64IMAFD + Zicsr + Zifencei (CLAUDE.md §7.2–7.3, §7.5).
//!
//! Every reserved or unsupported encoding decodes to `Inst::Illegal(raw)`; the decoder never
//! panics. `decode_bytes`/`insn_len` handle the 16/32-bit length split (§7.1).

use super::inst::*;
use super::rvc;

/// Extract `x[hi:lo]`.
#[inline]
fn bits(x: u32, hi: u32, lo: u32) -> u32 {
    ((x as u64 >> lo) & ((1u64 << (hi - lo + 1)) - 1)) as u32
}

/// Sign-extend the low `n` bits of `x`.
#[inline]
pub(crate) fn sext(x: u64, n: u32) -> i64 {
    ((x << (64 - n)) as i64) >> (64 - n)
}

#[inline]
fn rd(x: u32) -> u8 {
    bits(x, 11, 7) as u8
}
#[inline]
fn rs1(x: u32) -> u8 {
    bits(x, 19, 15) as u8
}
#[inline]
fn rs2(x: u32) -> u8 {
    bits(x, 24, 20) as u8
}
#[inline]
fn funct3(x: u32) -> u32 {
    bits(x, 14, 12)
}
#[inline]
fn funct7(x: u32) -> u32 {
    bits(x, 31, 25)
}

/// I-type immediate: inst[31:20].
#[inline]
pub fn imm_i(x: u32) -> i64 {
    ((x as i32) >> 20) as i64
}
/// S-type immediate: inst[31:25] ++ inst[11:7].
#[inline]
pub fn imm_s(x: u32) -> i64 {
    ((((x as i32) >> 25) << 5) | bits(x, 11, 7) as i32) as i64
}
/// B-type immediate: imm[12|10:5] = inst[31:25], imm[4:1|11] = inst[11:7].
#[inline]
pub fn imm_b(x: u32) -> i64 {
    let v = (bits(x, 31, 31) << 12)
        | (bits(x, 7, 7) << 11)
        | (bits(x, 30, 25) << 5)
        | (bits(x, 11, 8) << 1);
    sext(v as u64, 13)
}
/// U-type immediate: inst[31:12] << 12, sign-extended to 64 bits.
#[inline]
pub fn imm_u(x: u32) -> i64 {
    (x & 0xffff_f000) as i32 as i64
}
/// J-type immediate: imm[20|10:1|11|19:12] = inst[31:12].
#[inline]
pub fn imm_j(x: u32) -> i64 {
    let v = (bits(x, 31, 31) << 20)
        | (bits(x, 19, 12) << 12)
        | (bits(x, 20, 20) << 11)
        | (bits(x, 30, 21) << 1);
    sext(v as u64, 21)
}

/// Length in bytes of the instruction whose low 16 bits are `lo`: 2, 4, or 0 for the
/// unsupported ≥48-bit encodings (§7.1).
#[inline]
pub fn insn_len(lo: u16) -> u8 {
    if lo & 0b11 != 0b11 {
        2
    } else if lo & 0b11100 != 0b11100 {
        4
    } else {
        0
    }
}

/// Decode the instruction starting with halfword `lo`; `hi` supplies the next halfword and is
/// only called for 32-bit instructions (so a fetch fault on it happens only when needed).
pub fn decode_parts<E>(lo: u16, hi: impl FnOnce() -> Result<u16, E>) -> Result<Decoded, E> {
    Ok(match insn_len(lo) {
        2 => Decoded {
            inst: rvc::expand(lo),
            len: 2,
            raw: lo as u32,
        },
        4 => {
            let raw = lo as u32 | (hi()? as u32) << 16;
            Decoded {
                inst: decode(raw),
                len: 4,
                raw,
            }
        }
        _ => Decoded {
            inst: Inst::Illegal(lo as u32),
            len: 2,
            raw: lo as u32,
        },
    })
}

/// Decode a 32-bit instruction.
pub fn decode(x: u32) -> Inst {
    let ill = Inst::Illegal(x);
    match bits(x, 6, 0) {
        0x37 => Inst::Lui {
            rd: rd(x),
            imm: imm_u(x),
        },
        0x17 => Inst::Auipc {
            rd: rd(x),
            imm: imm_u(x),
        },
        0x6f => Inst::Jal {
            rd: rd(x),
            imm: imm_j(x),
        },
        0x67 if funct3(x) == 0 => Inst::Jalr {
            rd: rd(x),
            rs1: rs1(x),
            imm: imm_i(x),
        },
        0x63 => {
            let op = match funct3(x) {
                0 => BranchOp::Beq,
                1 => BranchOp::Bne,
                4 => BranchOp::Blt,
                5 => BranchOp::Bge,
                6 => BranchOp::Bltu,
                7 => BranchOp::Bgeu,
                _ => return ill,
            };
            Inst::Branch {
                op,
                rs1: rs1(x),
                rs2: rs2(x),
                imm: imm_b(x),
            }
        }
        0x03 => {
            let op = match funct3(x) {
                0 => LoadOp::Lb,
                1 => LoadOp::Lh,
                2 => LoadOp::Lw,
                3 => LoadOp::Ld,
                4 => LoadOp::Lbu,
                5 => LoadOp::Lhu,
                6 => LoadOp::Lwu,
                _ => return ill,
            };
            Inst::Load {
                op,
                rd: rd(x),
                rs1: rs1(x),
                imm: imm_i(x),
            }
        }
        0x23 => {
            let op = match funct3(x) {
                0 => StoreOp::Sb,
                1 => StoreOp::Sh,
                2 => StoreOp::Sw,
                3 => StoreOp::Sd,
                _ => return ill,
            };
            Inst::Store {
                op,
                rs1: rs1(x),
                rs2: rs2(x),
                imm: imm_s(x),
            }
        }
        0x13 => decode_op_imm(x),
        0x1b => decode_op_imm_w(x),
        0x33 => decode_op(x),
        0x3b => decode_op_w(x),
        0x0f => match funct3(x) {
            // Reserved fields (rd, rs1, unused fm values) are ignored, as the spec recommends.
            0 => Inst::Fence {
                pred: bits(x, 27, 24) as u8,
                succ: bits(x, 23, 20) as u8,
                fm: bits(x, 31, 28) as u8,
            },
            1 => Inst::FenceI,
            _ => ill,
        },
        0x73 => decode_system(x),
        0x2f => decode_amo(x),
        0x07 => match funct3(x) {
            2 => Inst::FLoad {
                fmt: FpFmt::S,
                rd: rd(x),
                rs1: rs1(x),
                imm: imm_i(x),
            },
            3 => Inst::FLoad {
                fmt: FpFmt::D,
                rd: rd(x),
                rs1: rs1(x),
                imm: imm_i(x),
            },
            _ => ill,
        },
        0x27 => match funct3(x) {
            2 => Inst::FStore {
                fmt: FpFmt::S,
                rs1: rs1(x),
                rs2: rs2(x),
                imm: imm_s(x),
            },
            3 => Inst::FStore {
                fmt: FpFmt::D,
                rs1: rs1(x),
                rs2: rs2(x),
                imm: imm_s(x),
            },
            _ => ill,
        },
        opc @ (0x43 | 0x47 | 0x4b | 0x4f) => {
            let fmt = match bits(x, 26, 25) {
                0 => FpFmt::S,
                1 => FpFmt::D,
                _ => return ill,
            };
            let op = match opc {
                0x43 => FmaOp::Madd,
                0x47 => FmaOp::Msub,
                0x4b => FmaOp::Nmsub,
                _ => FmaOp::Nmadd,
            };
            Inst::Fma {
                op,
                fmt,
                rd: rd(x),
                rs1: rs1(x),
                rs2: rs2(x),
                rs3: bits(x, 31, 27) as u8,
                rm: funct3(x) as u8,
            }
        }
        0x53 => decode_op_fp(x),
        _ => ill,
    }
}

fn decode_op_imm(x: u32) -> Inst {
    let (rd, rs1, imm) = (rd(x), rs1(x), imm_i(x));
    let op = match funct3(x) {
        0 => AluOp::Add,
        2 => AluOp::Slt,
        3 => AluOp::Sltu,
        4 => AluOp::Xor,
        6 => AluOp::Or,
        7 => AluOp::And,
        // RV64: 6-bit shamt in inst[25:20], funct6 in inst[31:26].
        1 if bits(x, 31, 26) == 0 => AluOp::Sll,
        5 if bits(x, 31, 26) == 0 => AluOp::Srl,
        5 if bits(x, 31, 26) == 0x10 => AluOp::Sra,
        _ => return Inst::Illegal(x),
    };
    let imm = match op {
        AluOp::Sll | AluOp::Srl | AluOp::Sra => bits(x, 25, 20) as i64,
        _ => imm,
    };
    Inst::OpImm { op, rd, rs1, imm }
}

fn decode_op_imm_w(x: u32) -> Inst {
    let (rd, rs1) = (rd(x), rs1(x));
    let (op, imm) = match (funct3(x), funct7(x)) {
        (0, _) => (AluWOp::Addw, imm_i(x)),
        // 5-bit shamt; inst[25] must be 0 (it is part of funct7 here).
        (1, 0) => (AluWOp::Sllw, bits(x, 24, 20) as i64),
        (5, 0) => (AluWOp::Srlw, bits(x, 24, 20) as i64),
        (5, 0x20) => (AluWOp::Sraw, bits(x, 24, 20) as i64),
        _ => return Inst::Illegal(x),
    };
    Inst::OpImmW { op, rd, rs1, imm }
}

fn decode_op(x: u32) -> Inst {
    use AluOp::*;
    let op = match (funct7(x), funct3(x)) {
        (0, 0) => Add,
        (0x20, 0) => Sub,
        (0, 1) => Sll,
        (0, 2) => Slt,
        (0, 3) => Sltu,
        (0, 4) => Xor,
        (0, 5) => Srl,
        (0x20, 5) => Sra,
        (0, 6) => Or,
        (0, 7) => And,
        (1, 0) => Mul,
        (1, 1) => Mulh,
        (1, 2) => Mulhsu,
        (1, 3) => Mulhu,
        (1, 4) => Div,
        (1, 5) => Divu,
        (1, 6) => Rem,
        (1, 7) => Remu,
        _ => return Inst::Illegal(x),
    };
    Inst::Op {
        op,
        rd: rd(x),
        rs1: rs1(x),
        rs2: rs2(x),
    }
}

fn decode_op_w(x: u32) -> Inst {
    use AluWOp::*;
    let op = match (funct7(x), funct3(x)) {
        (0, 0) => Addw,
        (0x20, 0) => Subw,
        (0, 1) => Sllw,
        (0, 5) => Srlw,
        (0x20, 5) => Sraw,
        (1, 0) => Mulw,
        (1, 4) => Divw,
        (1, 5) => Divuw,
        (1, 6) => Remw,
        (1, 7) => Remuw,
        _ => return Inst::Illegal(x),
    };
    Inst::OpW {
        op,
        rd: rd(x),
        rs1: rs1(x),
        rs2: rs2(x),
    }
}

fn decode_system(x: u32) -> Inst {
    let f3 = funct3(x);
    if f3 == 0 {
        if rd(x) != 0 {
            return Inst::Illegal(x);
        }
        if funct7(x) == 0x09 {
            return Inst::SfenceVma {
                rs1: rs1(x),
                rs2: rs2(x),
            };
        }
        if rs1(x) != 0 {
            return Inst::Illegal(x);
        }
        return match bits(x, 31, 20) {
            0x000 => Inst::Ecall,
            0x001 => Inst::Ebreak,
            0x102 => Inst::Sret,
            0x302 => Inst::Mret,
            0x105 => Inst::Wfi,
            _ => Inst::Illegal(x),
        };
    }
    let op = match f3 & 3 {
        1 => CsrOp::Rw,
        2 => CsrOp::Rs,
        3 => CsrOp::Rc,
        _ => return Inst::Illegal(x), // funct3 = 4: hypervisor loads/stores (not supported)
    };
    Inst::Csr {
        op,
        rd: rd(x),
        rs1: rs1(x),
        uimm: f3 & 4 != 0,
        csr: bits(x, 31, 20) as u16,
    }
}

fn decode_amo(x: u32) -> Inst {
    let width = match funct3(x) {
        2 => AmoWidth::W,
        3 => AmoWidth::D,
        _ => return Inst::Illegal(x),
    };
    let op = match bits(x, 31, 27) {
        0b00010 if rs2(x) == 0 => AmoOp::Lr,
        0b00011 => AmoOp::Sc,
        0b00001 => AmoOp::Swap,
        0b00000 => AmoOp::Add,
        0b00100 => AmoOp::Xor,
        0b01100 => AmoOp::And,
        0b01000 => AmoOp::Or,
        0b10000 => AmoOp::Min,
        0b10100 => AmoOp::Max,
        0b11000 => AmoOp::Minu,
        0b11100 => AmoOp::Maxu,
        _ => return Inst::Illegal(x),
    };
    Inst::Amo {
        op,
        width,
        aq: bits(x, 26, 26) != 0,
        rl: bits(x, 25, 25) != 0,
        rd: rd(x),
        rs1: rs1(x),
        rs2: rs2(x),
    }
}

fn decode_op_fp(x: u32) -> Inst {
    let fmt = match bits(x, 26, 25) {
        0 => FpFmt::S,
        1 => FpFmt::D,
        _ => return Inst::Illegal(x),
    };
    let (f3, r2) = (funct3(x), rs2(x));
    let int = |r: u8| match r {
        0 => Some(IntTy::W),
        1 => Some(IntTy::Wu),
        2 => Some(IntTy::L),
        3 => Some(IntTy::Lu),
        _ => None,
    };
    let op = match bits(x, 31, 27) {
        0b00000 => FpOp::Add,
        0b00001 => FpOp::Sub,
        0b00010 => FpOp::Mul,
        0b00011 => FpOp::Div,
        0b01011 if r2 == 0 => FpOp::Sqrt,
        0b00100 => match f3 {
            0 => FpOp::SgnJ,
            1 => FpOp::SgnJn,
            2 => FpOp::SgnJx,
            _ => return Inst::Illegal(x),
        },
        0b00101 => match f3 {
            0 => FpOp::Min,
            1 => FpOp::Max,
            _ => return Inst::Illegal(x),
        },
        // FCVT.S.D has rs2 = 1 (source D); FCVT.D.S has rs2 = 0 (source S).
        0b01000 => match (fmt, r2) {
            (FpFmt::S, 1) | (FpFmt::D, 0) => FpOp::CvtFmt,
            _ => return Inst::Illegal(x),
        },
        0b10100 => match f3 {
            0 => FpOp::Le,
            1 => FpOp::Lt,
            2 => FpOp::Eq,
            _ => return Inst::Illegal(x),
        },
        0b11000 => match int(r2) {
            Some(t) => FpOp::CvtToInt(t),
            None => return Inst::Illegal(x),
        },
        0b11010 => match int(r2) {
            Some(t) => FpOp::CvtFromInt(t),
            None => return Inst::Illegal(x),
        },
        0b11100 if r2 == 0 && f3 == 0 => FpOp::MvToInt,
        0b11100 if r2 == 0 && f3 == 1 => FpOp::Class,
        0b11110 if r2 == 0 && f3 == 0 => FpOp::MvFromInt,
        _ => return Inst::Illegal(x),
    };
    Inst::Fp {
        op,
        fmt,
        rd: rd(x),
        rs1: rs1(x),
        rs2: r2,
        rm: f3 as u8,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn immediates_extremes() {
        // addi x0, x0, -2048 / 2047
        assert_eq!(imm_i(0x8000_0013), -2048);
        assert_eq!(imm_i(0x7ff0_0013), 2047);
        // sw with imm -1: inst[31:25]=0x7f, inst[11:7]=0x1f
        assert_eq!(imm_s(0xfe00_0fa3), -1);
        // beq with imm -4096 (only bit 31 set)
        assert_eq!(imm_b(0x8000_0063), -4096);
        // jal with imm -1048576 (only bit 31 set)
        assert_eq!(imm_j(0x8000_006f), -1_048_576);
        assert_eq!(imm_u(0xfffff037), -4096);
    }

    #[test]
    fn lengths() {
        assert_eq!(insn_len(0x0001), 2);
        assert_eq!(insn_len(0x0013), 4);
        assert_eq!(insn_len(0x001f), 0); // 48-bit prefix
        assert_eq!(insn_len(0x007f), 0); // >= 64-bit prefix
    }

    #[test]
    fn illegal_encodings() {
        for raw in [
            0x0000_000b, // custom-0
            0xffff_ffff, // opcode 0x7f
            0x0400_1013, // slli funct6 = 000001 (bit 25 alone is shamt[5], which is legal)
            0x4000_1013, // slli funct6 = 010000 (srai pattern on funct3=1)
            0x0200_101b, // slliw with inst[25]=1
            0x1000_0033, // OP funct7 = 0x08
            0x0000_2063, // branch funct3 = 2
            0x0000_7003, // load funct3 = 7
            0x0000_4023, // store funct3 = 4
            0x0000_1067, // jalr funct3 = 1
            0x0010_00f3, // ecall-space with rd != 0
            0x0020_0073, // funct12 = 2 (uret is not supported)
            0x0000_4073, // SYSTEM funct3 = 4
            0x0000_102f, // AMO funct3 = 1
            0x1010_202f, // LR with rs2 != 0
            0x0600_0053, // OP-FP fmt = 3 (Q)
            0x5810_0053, // fsqrt with rs2 != 0
        ] {
            assert_eq!(decode(raw), Inst::Illegal(raw), "{raw:#010x}");
        }
        // RV64 slli has a 6-bit shamt: inst[25] = 1 (shamt = 32) is legal.
        assert!(matches!(
            decode(0x0200_1013),
            Inst::OpImm {
                op: AluOp::Sll,
                imm: 32,
                ..
            }
        ));
    }
}
