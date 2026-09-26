//! Disassembler producing LLVM's canonical (`-M no-aliases`) syntax with ABI register names, so
//! decoder tests can compare against `llvm-mc` output verbatim. Compressed instructions print
//! as their 32-bit expansion.

use std::fmt::Write;

use super::inst::*;

pub const XNAMES: [&str; 32] = [
    "zero", "ra", "sp", "gp", "tp", "t0", "t1", "t2", "s0", "s1", "a0", "a1", "a2", "a3", "a4",
    "a5", "a6", "a7", "s2", "s3", "s4", "s5", "s6", "s7", "s8", "s9", "s10", "s11", "t3", "t4",
    "t5", "t6",
];

const FNAMES: [&str; 32] = [
    "ft0", "ft1", "ft2", "ft3", "ft4", "ft5", "ft6", "ft7", "fs0", "fs1", "fa0", "fa1", "fa2",
    "fa3", "fa4", "fa5", "fa6", "fa7", "fs2", "fs3", "fs4", "fs5", "fs6", "fs7", "fs8", "fs9",
    "fs10", "fs11", "ft8", "ft9", "ft10", "ft11",
];

/// ABI name of integer register `r`.
pub fn xname(r: u8) -> &'static str {
    XNAMES[r as usize & 31]
}

/// ABI name of FP register `r`.
pub fn fname(r: u8) -> &'static str {
    FNAMES[r as usize & 31]
}

/// Name of a CSR as LLVM prints it, if it has one.
pub fn csr_name(csr: u16) -> Option<&'static str> {
    Some(match csr {
        0x001 => "fflags",
        0x002 => "frm",
        0x003 => "fcsr",
        0xc00 => "cycle",
        0xc01 => "time",
        0xc02 => "instret",
        0x100 => "sstatus",
        0x104 => "sie",
        0x105 => "stvec",
        0x106 => "scounteren",
        0x10a => "senvcfg",
        0x140 => "sscratch",
        0x141 => "sepc",
        0x142 => "scause",
        0x143 => "stval",
        0x144 => "sip",
        0x180 => "satp",
        0x300 => "mstatus",
        0x301 => "misa",
        0x302 => "medeleg",
        0x303 => "mideleg",
        0x304 => "mie",
        0x305 => "mtvec",
        0x306 => "mcounteren",
        0x30a => "menvcfg",
        0x320 => "mcountinhibit",
        0x340 => "mscratch",
        0x341 => "mepc",
        0x342 => "mcause",
        0x343 => "mtval",
        0x344 => "mip",
        0x3a0 => "pmpcfg0",
        0x3a2 => "pmpcfg2",
        0x3b0 => "pmpaddr0",
        0xb00 => "mcycle",
        0xb02 => "minstret",
        0xf11 => "mvendorid",
        0xf12 => "marchid",
        0xf13 => "mimpid",
        0xf14 => "mhartid",
        _ => return None,
    })
}

fn rm_name(rm: u8) -> &'static str {
    match rm {
        0 => "rne",
        1 => "rtz",
        2 => "rdn",
        3 => "rup",
        4 => "rmm",
        7 => "dyn",
        _ => "frm?",
    }
}

fn fence_set(bits: u8) -> String {
    if bits == 0 {
        return "0".into();
    }
    let mut s = String::new();
    for (bit, c) in [(8, 'i'), (4, 'o'), (2, 'r'), (1, 'w')] {
        if bits & bit != 0 {
            s.push(c);
        }
    }
    s
}

fn sfx(fmt: FpFmt) -> &'static str {
    match fmt {
        FpFmt::S => "s",
        FpFmt::D => "d",
    }
}

fn int_sfx(t: IntTy) -> &'static str {
    match t {
        IntTy::W => "w",
        IntTy::Wu => "wu",
        IntTy::L => "l",
        IntTy::Lu => "lu",
    }
}

/// Disassemble one instruction.
pub fn disasm(inst: &Inst) -> String {
    let mut s = String::new();
    // Writing into a String cannot fail.
    let _ = write_inst(&mut s, inst);
    s
}

fn write_inst(s: &mut String, inst: &Inst) -> std::fmt::Result {
    let x = xname;
    let f = fname;
    match *inst {
        Inst::Lui { rd, imm } => write!(s, "lui {}, {}", x(rd), (imm >> 12) & 0xfffff),
        Inst::Auipc { rd, imm } => write!(s, "auipc {}, {}", x(rd), (imm >> 12) & 0xfffff),
        Inst::Jal { rd, imm } => write!(s, "jal {}, {imm}", x(rd)),
        Inst::Jalr { rd, rs1, imm } => write!(s, "jalr {}, {imm}({})", x(rd), x(rs1)),
        Inst::Branch { op, rs1, rs2, imm } => {
            let m = match op {
                BranchOp::Beq => "beq",
                BranchOp::Bne => "bne",
                BranchOp::Blt => "blt",
                BranchOp::Bge => "bge",
                BranchOp::Bltu => "bltu",
                BranchOp::Bgeu => "bgeu",
            };
            write!(s, "{m} {}, {}, {imm}", x(rs1), x(rs2))
        }
        Inst::Load { op, rd, rs1, imm } => {
            let m = match op {
                LoadOp::Lb => "lb",
                LoadOp::Lh => "lh",
                LoadOp::Lw => "lw",
                LoadOp::Ld => "ld",
                LoadOp::Lbu => "lbu",
                LoadOp::Lhu => "lhu",
                LoadOp::Lwu => "lwu",
            };
            write!(s, "{m} {}, {imm}({})", x(rd), x(rs1))
        }
        Inst::Store { op, rs1, rs2, imm } => {
            let m = match op {
                StoreOp::Sb => "sb",
                StoreOp::Sh => "sh",
                StoreOp::Sw => "sw",
                StoreOp::Sd => "sd",
            };
            write!(s, "{m} {}, {imm}({})", x(rs2), x(rs1))
        }
        Inst::OpImm { op, rd, rs1, imm } => {
            let m = match op {
                AluOp::Add => "addi",
                AluOp::Slt => "slti",
                AluOp::Sltu => "sltiu",
                AluOp::Xor => "xori",
                AluOp::Or => "ori",
                AluOp::And => "andi",
                AluOp::Sll => "slli",
                AluOp::Srl => "srli",
                AluOp::Sra => "srai",
                _ => "op-imm?",
            };
            write!(s, "{m} {}, {}, {imm}", x(rd), x(rs1))
        }
        Inst::OpImmW { op, rd, rs1, imm } => {
            let m = match op {
                AluWOp::Addw => "addiw",
                AluWOp::Sllw => "slliw",
                AluWOp::Srlw => "srliw",
                AluWOp::Sraw => "sraiw",
                _ => "op-imm-32?",
            };
            write!(s, "{m} {}, {}, {imm}", x(rd), x(rs1))
        }
        Inst::Op { op, rd, rs1, rs2 } => {
            let m = match op {
                AluOp::Add => "add",
                AluOp::Sub => "sub",
                AluOp::Sll => "sll",
                AluOp::Slt => "slt",
                AluOp::Sltu => "sltu",
                AluOp::Xor => "xor",
                AluOp::Srl => "srl",
                AluOp::Sra => "sra",
                AluOp::Or => "or",
                AluOp::And => "and",
                AluOp::Mul => "mul",
                AluOp::Mulh => "mulh",
                AluOp::Mulhsu => "mulhsu",
                AluOp::Mulhu => "mulhu",
                AluOp::Div => "div",
                AluOp::Divu => "divu",
                AluOp::Rem => "rem",
                AluOp::Remu => "remu",
            };
            write!(s, "{m} {}, {}, {}", x(rd), x(rs1), x(rs2))
        }
        Inst::OpW { op, rd, rs1, rs2 } => {
            let m = match op {
                AluWOp::Addw => "addw",
                AluWOp::Subw => "subw",
                AluWOp::Sllw => "sllw",
                AluWOp::Srlw => "srlw",
                AluWOp::Sraw => "sraw",
                AluWOp::Mulw => "mulw",
                AluWOp::Divw => "divw",
                AluWOp::Divuw => "divuw",
                AluWOp::Remw => "remw",
                AluWOp::Remuw => "remuw",
            };
            write!(s, "{m} {}, {}, {}", x(rd), x(rs1), x(rs2))
        }
        Inst::Fence { pred, succ, fm } => {
            if fm == 0b1000 && pred == 0b0011 && succ == 0b0011 {
                write!(s, "fence.tso")
            } else {
                write!(s, "fence {}, {}", fence_set(pred), fence_set(succ))
            }
        }
        Inst::FenceI => write!(s, "fence.i"),
        Inst::Ecall => write!(s, "ecall"),
        Inst::Ebreak => write!(s, "ebreak"),
        Inst::Mret => write!(s, "mret"),
        Inst::Sret => write!(s, "sret"),
        Inst::Wfi => write!(s, "wfi"),
        Inst::SfenceVma { rs1, rs2 } => write!(s, "sfence.vma {}, {}", x(rs1), x(rs2)),
        Inst::Csr {
            op,
            rd,
            rs1,
            uimm,
            csr,
        } => {
            let m = match (op, uimm) {
                (CsrOp::Rw, false) => "csrrw",
                (CsrOp::Rs, false) => "csrrs",
                (CsrOp::Rc, false) => "csrrc",
                (CsrOp::Rw, true) => "csrrwi",
                (CsrOp::Rs, true) => "csrrsi",
                (CsrOp::Rc, true) => "csrrci",
            };
            let name = csr_name(csr).map_or_else(|| csr.to_string(), str::to_string);
            if uimm {
                write!(s, "{m} {}, {name}, {rs1}", x(rd))
            } else {
                write!(s, "{m} {}, {name}, {}", x(rd), x(rs1))
            }
        }
        Inst::Amo {
            op,
            width,
            aq,
            rl,
            rd,
            rs1,
            rs2,
        } => {
            let m = match op {
                AmoOp::Lr => "lr",
                AmoOp::Sc => "sc",
                AmoOp::Swap => "amoswap",
                AmoOp::Add => "amoadd",
                AmoOp::Xor => "amoxor",
                AmoOp::And => "amoand",
                AmoOp::Or => "amoor",
                AmoOp::Min => "amomin",
                AmoOp::Max => "amomax",
                AmoOp::Minu => "amominu",
                AmoOp::Maxu => "amomaxu",
            };
            let w = if width == AmoWidth::W { "w" } else { "d" };
            let ord = match (aq, rl) {
                (false, false) => "",
                (true, false) => ".aq",
                (false, true) => ".rl",
                (true, true) => ".aqrl",
            };
            if op == AmoOp::Lr {
                write!(s, "{m}.{w}{ord} {}, ({})", x(rd), x(rs1))
            } else {
                write!(s, "{m}.{w}{ord} {}, {}, ({})", x(rd), x(rs2), x(rs1))
            }
        }
        Inst::FLoad { fmt, rd, rs1, imm } => {
            let m = if fmt == FpFmt::S { "flw" } else { "fld" };
            write!(s, "{m} {}, {imm}({})", f(rd), x(rs1))
        }
        Inst::FStore { fmt, rs1, rs2, imm } => {
            let m = if fmt == FpFmt::S { "fsw" } else { "fsd" };
            write!(s, "{m} {}, {imm}({})", f(rs2), x(rs1))
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
            let m = match op {
                FmaOp::Madd => "fmadd",
                FmaOp::Msub => "fmsub",
                FmaOp::Nmsub => "fnmsub",
                FmaOp::Nmadd => "fnmadd",
            };
            write!(
                s,
                "{m}.{} {}, {}, {}, {}, {}",
                sfx(fmt),
                f(rd),
                f(rs1),
                f(rs2),
                f(rs3),
                rm_name(rm)
            )
        }
        Inst::Fp {
            op,
            fmt,
            rd,
            rs1,
            rs2,
            rm,
        } => {
            let t = sfx(fmt);
            match op {
                FpOp::Add | FpOp::Sub | FpOp::Mul | FpOp::Div => {
                    let m = match op {
                        FpOp::Add => "fadd",
                        FpOp::Sub => "fsub",
                        FpOp::Mul => "fmul",
                        _ => "fdiv",
                    };
                    write!(
                        s,
                        "{m}.{t} {}, {}, {}, {}",
                        f(rd),
                        f(rs1),
                        f(rs2),
                        rm_name(rm)
                    )
                }
                FpOp::Sqrt => write!(s, "fsqrt.{t} {}, {}, {}", f(rd), f(rs1), rm_name(rm)),
                FpOp::SgnJ | FpOp::SgnJn | FpOp::SgnJx | FpOp::Min | FpOp::Max => {
                    let m = match op {
                        FpOp::SgnJ => "fsgnj",
                        FpOp::SgnJn => "fsgnjn",
                        FpOp::SgnJx => "fsgnjx",
                        FpOp::Min => "fmin",
                        _ => "fmax",
                    };
                    write!(s, "{m}.{t} {}, {}, {}", f(rd), f(rs1), f(rs2))
                }
                FpOp::Eq | FpOp::Lt | FpOp::Le => {
                    let m = match op {
                        FpOp::Eq => "feq",
                        FpOp::Lt => "flt",
                        _ => "fle",
                    };
                    write!(s, "{m}.{t} {}, {}, {}", x(rd), f(rs1), f(rs2))
                }
                FpOp::Class => write!(s, "fclass.{t} {}, {}", x(rd), f(rs1)),
                FpOp::MvToInt => {
                    let w = if fmt == FpFmt::S { "w" } else { "d" };
                    write!(s, "fmv.x.{w} {}, {}", x(rd), f(rs1))
                }
                FpOp::MvFromInt => {
                    let w = if fmt == FpFmt::S { "w" } else { "d" };
                    write!(s, "fmv.{w}.x {}, {}", f(rd), x(rs1))
                }
                FpOp::CvtToInt(it) => write!(
                    s,
                    "fcvt.{}.{t} {}, {}, {}",
                    int_sfx(it),
                    x(rd),
                    f(rs1),
                    rm_name(rm)
                ),
                FpOp::CvtFromInt(it) => {
                    // Conversions that are always exact (32-bit int to double) print no
                    // rounding mode when it is the default RNE encoding, like LLVM.
                    let exact = fmt == FpFmt::D && matches!(it, IntTy::W | IntTy::Wu);
                    write!(s, "fcvt.{t}.{} {}, {}", int_sfx(it), f(rd), x(rs1))?;
                    if !(exact && rm == 0) {
                        write!(s, ", {}", rm_name(rm))?;
                    }
                    Ok(())
                }
                FpOp::CvtFmt => {
                    // FCVT.D.S is exact: same printing rule as above.
                    let from = if fmt == FpFmt::S { "d" } else { "s" };
                    write!(s, "fcvt.{t}.{from} {}, {}", f(rd), f(rs1))?;
                    if !(fmt == FpFmt::D && rm == 0) {
                        write!(s, ", {}", rm_name(rm))?;
                    }
                    Ok(())
                }
            }
        }
        Inst::Illegal(raw) => write!(s, "unimp # {raw:#x}"),
    }
}
