//! `Inst`, the decoded form of every supported RV64GC instruction (CLAUDE.md §7).
//!
//! Register fields are plain `u8` indices, always in `0..32` because the decoder extracts them
//! from 5-bit fields. Immediates are stored fully sign-extended as `i64` (for `Lui`/`Auipc` the
//! value is `imm[31:12] << 12`, already shifted). Compressed instructions decode to the same
//! `Inst` as their 32-bit expansion; only `Decoded::len` differs.

/// Integer register index (`x0`..`x31`).
pub type Reg = u8;
/// Floating-point register index (`f0`..`f31`).
pub type FReg = u8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BranchOp {
    Beq,
    Bne,
    Blt,
    Bge,
    Bltu,
    Bgeu,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadOp {
    Lb,
    Lh,
    Lw,
    Ld,
    Lbu,
    Lhu,
    Lwu,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreOp {
    Sb,
    Sh,
    Sw,
    Sd,
}

/// 64-bit ALU operations, shared by register (`Op`) and immediate (`OpImm`) forms. The
/// immediate form only uses Add, Slt, Sltu, Xor, Or, And, Sll, Srl and Sra.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AluOp {
    Add,
    Sub,
    Sll,
    Slt,
    Sltu,
    Xor,
    Srl,
    Sra,
    Or,
    And,
    Mul,
    Mulh,
    Mulhsu,
    Mulhu,
    Div,
    Divu,
    Rem,
    Remu,
}

/// 32-bit ("W") ALU operations: compute on the low 32 bits, sign-extend the result.
/// The immediate form only uses Addw, Sllw, Srlw and Sraw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AluWOp {
    Addw,
    Subw,
    Sllw,
    Srlw,
    Sraw,
    Mulw,
    Divw,
    Divuw,
    Remw,
    Remuw,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CsrOp {
    /// CSRRW / CSRRWI
    Rw,
    /// CSRRS / CSRRSI
    Rs,
    /// CSRRC / CSRRCI
    Rc,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AmoOp {
    Lr,
    Sc,
    Swap,
    Add,
    Xor,
    And,
    Or,
    Min,
    Max,
    Minu,
    Maxu,
}

/// Access width of an atomic memory operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AmoWidth {
    W,
    D,
}

/// Floating-point format of an F/D instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FpFmt {
    S,
    D,
}

/// Integer type of an FCVT between integer and floating point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntTy {
    W,
    Wu,
    L,
    Lu,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FmaOp {
    /// rs1*rs2 + rs3
    Madd,
    /// rs1*rs2 - rs3
    Msub,
    /// -(rs1*rs2) + rs3
    Nmsub,
    /// -(rs1*rs2) - rs3
    Nmadd,
}

/// OP-FP operations. Register files of the operands depend on the op:
/// `Eq/Lt/Le/Class/MvToInt/CvtToInt` write an integer `rd`; `MvFromInt/CvtFromInt` read an
/// integer `rs1`; everything else is FP-to-FP.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FpOp {
    Add,
    Sub,
    Mul,
    Div,
    Sqrt,
    SgnJ,
    SgnJn,
    SgnJx,
    Min,
    Max,
    Eq,
    Lt,
    Le,
    Class,
    /// FMV.X.W / FMV.X.D
    MvToInt,
    /// FMV.W.X / FMV.D.X
    MvFromInt,
    /// FCVT.{W,WU,L,LU}.fmt
    CvtToInt(IntTy),
    /// FCVT.fmt.{W,WU,L,LU}
    CvtFromInt(IntTy),
    /// FCVT.S.D (fmt = S) or FCVT.D.S (fmt = D): convert *to* `fmt` from the other format.
    CvtFmt,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Inst {
    Lui {
        rd: Reg,
        imm: i64,
    },
    Auipc {
        rd: Reg,
        imm: i64,
    },
    Jal {
        rd: Reg,
        imm: i64,
    },
    Jalr {
        rd: Reg,
        rs1: Reg,
        imm: i64,
    },
    Branch {
        op: BranchOp,
        rs1: Reg,
        rs2: Reg,
        imm: i64,
    },
    Load {
        op: LoadOp,
        rd: Reg,
        rs1: Reg,
        imm: i64,
    },
    Store {
        op: StoreOp,
        rs1: Reg,
        rs2: Reg,
        imm: i64,
    },
    OpImm {
        op: AluOp,
        rd: Reg,
        rs1: Reg,
        imm: i64,
    },
    OpImmW {
        op: AluWOp,
        rd: Reg,
        rs1: Reg,
        imm: i64,
    },
    Op {
        op: AluOp,
        rd: Reg,
        rs1: Reg,
        rs2: Reg,
    },
    OpW {
        op: AluWOp,
        rd: Reg,
        rs1: Reg,
        rs2: Reg,
    },
    /// FENCE / FENCE.TSO. `pred`/`succ` bits: I=8, O=4, R=2, W=1; `fm` = inst[31:28].
    Fence {
        pred: u8,
        succ: u8,
        fm: u8,
    },
    FenceI,
    Ecall,
    Ebreak,
    Mret,
    Sret,
    Wfi,
    SfenceVma {
        rs1: Reg,
        rs2: Reg,
    },
    /// CSR access. With `uimm`, `rs1` holds the 5-bit zero-extended immediate instead of a register.
    Csr {
        op: CsrOp,
        rd: Reg,
        rs1: u8,
        uimm: bool,
        csr: u16,
    },
    Amo {
        op: AmoOp,
        width: AmoWidth,
        aq: bool,
        rl: bool,
        rd: Reg,
        rs1: Reg,
        rs2: Reg,
    },
    FLoad {
        fmt: FpFmt,
        rd: FReg,
        rs1: Reg,
        imm: i64,
    },
    FStore {
        fmt: FpFmt,
        rs1: Reg,
        rs2: FReg,
        imm: i64,
    },
    Fma {
        op: FmaOp,
        fmt: FpFmt,
        rd: FReg,
        rs1: FReg,
        rs2: FReg,
        rs3: FReg,
        rm: u8,
    },
    Fp {
        op: FpOp,
        fmt: FpFmt,
        rd: u8,
        rs1: u8,
        rs2: u8,
        rm: u8,
    },
    /// Any encoding that is reserved, unsupported or malformed; raises illegal-instruction.
    Illegal(u32),
}

/// A decoded instruction with its length in bytes (2 or 4) and its raw bits (for `mtval`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Decoded {
    pub inst: Inst,
    pub len: u8,
    pub raw: u32,
}

impl Inst {
    /// Does this instruction end a basic block (CLAUDE.md §13.2)?
    pub fn ends_block(&self) -> bool {
        matches!(
            self,
            Inst::Jal { .. }
                | Inst::Jalr { .. }
                | Inst::Branch { .. }
                | Inst::FenceI
                | Inst::Ecall
                | Inst::Ebreak
                | Inst::Mret
                | Inst::Sret
                | Inst::Wfi
                | Inst::SfenceVma { .. }
                | Inst::Csr { .. }
                | Inst::Illegal(_)
        )
    }
}
