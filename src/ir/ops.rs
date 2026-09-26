//! IR definitions (CLAUDE.md §9, P4.1): one translation block of single-assignment values.
//!
//! A block is straight-line code, so every value `V` is defined exactly once and before its
//! uses (SSA without phis). Guest registers are read and written explicitly (`ReadReg`,
//! `WriteReg`); x0 never appears (reads lift to `Const 0`, writes are dropped). Every block
//! ends with exactly one terminator (`Branch`, `Jump`, `JumpInd`, `Exit`). `Insn` markers
//! delimit guest instructions: their index drives precise faults and budget refunds (D30).

use std::fmt;

use crate::cpu::trap::Exception;
use crate::interp::{alu, aluw};
use crate::isa::inst::{AluOp, AluWOp};

/// A single-assignment value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct V(pub u32);

/// Binary integer operations. `*w` forms compute on the low 32 bits and sign-extend.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BinOp {
    Add,
    Sub,
    And,
    Or,
    Xor,
    Sll,
    Srl,
    Sra,
    Slt,
    Sltu,
    Mul,
    Mulh,
    Mulhu,
    Mulhsu,
    Div,
    Divu,
    Rem,
    Remu,
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

impl BinOp {
    pub fn from_alu(op: AluOp) -> BinOp {
        match op {
            AluOp::Add => BinOp::Add,
            AluOp::Sub => BinOp::Sub,
            AluOp::Sll => BinOp::Sll,
            AluOp::Slt => BinOp::Slt,
            AluOp::Sltu => BinOp::Sltu,
            AluOp::Xor => BinOp::Xor,
            AluOp::Srl => BinOp::Srl,
            AluOp::Sra => BinOp::Sra,
            AluOp::Or => BinOp::Or,
            AluOp::And => BinOp::And,
            AluOp::Mul => BinOp::Mul,
            AluOp::Mulh => BinOp::Mulh,
            AluOp::Mulhsu => BinOp::Mulhsu,
            AluOp::Mulhu => BinOp::Mulhu,
            AluOp::Div => BinOp::Div,
            AluOp::Divu => BinOp::Divu,
            AluOp::Rem => BinOp::Rem,
            AluOp::Remu => BinOp::Remu,
        }
    }

    pub fn from_aluw(op: AluWOp) -> BinOp {
        match op {
            AluWOp::Addw => BinOp::Addw,
            AluWOp::Subw => BinOp::Subw,
            AluWOp::Sllw => BinOp::Sllw,
            AluWOp::Srlw => BinOp::Srlw,
            AluWOp::Sraw => BinOp::Sraw,
            AluWOp::Mulw => BinOp::Mulw,
            AluWOp::Divw => BinOp::Divw,
            AluWOp::Divuw => BinOp::Divuw,
            AluWOp::Remw => BinOp::Remw,
            AluWOp::Remuw => BinOp::Remuw,
        }
    }

    /// The architectural result (the interpreter's semantics are the reference).
    pub fn eval(self, a: u64, b: u64) -> u64 {
        use BinOp::*;
        match self {
            Add => alu(AluOp::Add, a, b),
            Sub => alu(AluOp::Sub, a, b),
            And => alu(AluOp::And, a, b),
            Or => alu(AluOp::Or, a, b),
            Xor => alu(AluOp::Xor, a, b),
            Sll => alu(AluOp::Sll, a, b),
            Srl => alu(AluOp::Srl, a, b),
            Sra => alu(AluOp::Sra, a, b),
            Slt => alu(AluOp::Slt, a, b),
            Sltu => alu(AluOp::Sltu, a, b),
            Mul => alu(AluOp::Mul, a, b),
            Mulh => alu(AluOp::Mulh, a, b),
            Mulhu => alu(AluOp::Mulhu, a, b),
            Mulhsu => alu(AluOp::Mulhsu, a, b),
            Div => alu(AluOp::Div, a, b),
            Divu => alu(AluOp::Divu, a, b),
            Rem => alu(AluOp::Rem, a, b),
            Remu => alu(AluOp::Remu, a, b),
            Addw => aluw(AluWOp::Addw, a, b),
            Subw => aluw(AluWOp::Subw, a, b),
            Sllw => aluw(AluWOp::Sllw, a, b),
            Srlw => aluw(AluWOp::Srlw, a, b),
            Sraw => aluw(AluWOp::Sraw, a, b),
            Mulw => aluw(AluWOp::Mulw, a, b),
            Divw => aluw(AluWOp::Divw, a, b),
            Divuw => aluw(AluWOp::Divuw, a, b),
            Remw => aluw(AluWOp::Remw, a, b),
            Remuw => aluw(AluWOp::Remuw, a, b),
        }
    }

    pub fn is_commutative(self) -> bool {
        use BinOp::*;
        matches!(
            self,
            Add | And | Or | Xor | Mul | Mulh | Mulhu | Addw | Mulw
        )
    }

    pub fn name(self) -> &'static str {
        use BinOp::*;
        match self {
            Add => "add",
            Sub => "sub",
            And => "and",
            Or => "or",
            Xor => "xor",
            Sll => "sll",
            Srl => "srl",
            Sra => "sra",
            Slt => "slt",
            Sltu => "sltu",
            Mul => "mul",
            Mulh => "mulh",
            Mulhu => "mulhu",
            Mulhsu => "mulhsu",
            Div => "div",
            Divu => "divu",
            Rem => "rem",
            Remu => "remu",
            Addw => "addw",
            Subw => "subw",
            Sllw => "sllw",
            Srlw => "srlw",
            Sraw => "sraw",
            Mulw => "mulw",
            Divw => "divw",
            Divuw => "divuw",
            Remw => "remw",
            Remuw => "remuw",
        }
    }
}

/// Branch conditions (RISC-V BEQ…BGEU).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cond {
    Eq,
    Ne,
    Lt,
    Ge,
    Ltu,
    Geu,
}

impl Cond {
    pub fn eval(self, a: u64, b: u64) -> bool {
        match self {
            Cond::Eq => a == b,
            Cond::Ne => a != b,
            Cond::Lt => (a as i64) < (b as i64),
            Cond::Ge => (a as i64) >= (b as i64),
            Cond::Ltu => a < b,
            Cond::Geu => a >= b,
        }
    }
}

/// Non-chainable block exits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitKind {
    /// ECALL at `pc` (not retired).
    Ecall,
    /// FENCE.I retired; continue at `pc` after flushing translated code.
    FenceI,
    /// The fetch at `pc` faults.
    Fault(Exception),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    /// Start of guest instruction `idx` of the block, at `pc`.
    Insn {
        pc: u64,
        idx: u32,
    },
    Const {
        dst: V,
        imm: u64,
    },
    ReadReg {
        dst: V,
        g: u8,
    },
    WriteReg {
        g: u8,
        src: V,
    },
    Bin {
        op: BinOp,
        dst: V,
        a: V,
        b: V,
    },
    /// `dst = op(a, imm as u64)`.
    BinImm {
        op: BinOp,
        dst: V,
        a: V,
        imm: i64,
    },
    /// `dst = mem[a + off]` (`size` bytes, sign- or zero-extended). May fault.
    Load {
        dst: V,
        addr: V,
        off: i32,
        size: u8,
        signed: bool,
    },
    /// `mem[a + off] = val` (low `size` bytes). May fault.
    Store {
        addr: V,
        off: i32,
        val: V,
        size: u8,
    },
    /// Execute guest instruction `idx` (`raw`, at `pc`) in the interpreter (D14). Reads and
    /// writes any guest state; may leave the block.
    Interp {
        raw: u32,
        pc: u64,
        idx: u32,
    },
    Branch {
        cond: Cond,
        a: V,
        b: V,
        taken: u64,
        fall: u64,
    },
    Jump {
        pc: u64,
    },
    /// Jump to the guest pc in `target` (already 2-byte aligned).
    JumpInd {
        target: V,
    },
    Exit {
        kind: ExitKind,
        pc: u64,
    },
}

impl Op {
    /// The value this op defines.
    pub fn def(&self) -> Option<V> {
        match *self {
            Op::Const { dst, .. }
            | Op::ReadReg { dst, .. }
            | Op::Bin { dst, .. }
            | Op::BinImm { dst, .. }
            | Op::Load { dst, .. } => Some(dst),
            _ => None,
        }
    }

    /// Values this op uses (at most 2).
    pub fn uses(&self) -> impl Iterator<Item = V> {
        let (a, b) = match *self {
            Op::WriteReg { src, .. } => (Some(src), None),
            Op::Bin { a, b, .. } => (Some(a), Some(b)),
            Op::BinImm { a, .. } => (Some(a), None),
            Op::Load { addr, .. } => (Some(addr), None),
            Op::Store { addr, val, .. } => (Some(addr), Some(val)),
            Op::Branch { a, b, .. } => (Some(a), Some(b)),
            Op::JumpInd { target } => (Some(target), None),
            _ => (None, None),
        };
        a.into_iter().chain(b)
    }

    /// Rewrite every use through `f`.
    pub fn map_uses(&mut self, mut f: impl FnMut(V) -> V) {
        match self {
            Op::WriteReg { src, .. } => *src = f(*src),
            Op::Bin { a, b, .. } | Op::Branch { a, b, .. } => {
                *a = f(*a);
                *b = f(*b);
            }
            Op::BinImm { a, .. } => *a = f(*a),
            Op::Load { addr, .. } => *addr = f(*addr),
            Op::Store { addr, val, .. } => {
                *addr = f(*addr);
                *val = f(*val);
            }
            Op::JumpInd { target } => *target = f(*target),
            _ => {}
        }
    }

    pub fn is_terminator(&self) -> bool {
        matches!(
            self,
            Op::Branch { .. } | Op::Jump { .. } | Op::JumpInd { .. } | Op::Exit { .. }
        )
    }

    /// Can this op be removed when its result is unused? (Loads may fault: never.)
    pub fn is_pure(&self) -> bool {
        matches!(
            self,
            Op::Const { .. } | Op::ReadReg { .. } | Op::Bin { .. } | Op::BinImm { .. }
        )
    }
}

/// One translation block in IR form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub pc: u64,
    pub ops: Vec<Op>,
    /// Guest instructions in the block (charged by the prologue, D30).
    pub n_insns: u32,
    /// Values are numbered `0..nvals`.
    pub nvals: u32,
}

impl Block {
    pub fn new_value(&mut self) -> V {
        self.nvals += 1;
        V(self.nvals - 1)
    }
}

impl fmt::Display for V {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "v{}", self.0)
    }
}

impl fmt::Display for Op {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Op::Insn { pc, idx } => write!(f, "--- #{idx} @{pc:#x}"),
            Op::Const { dst, imm } => write!(f, "{dst} = const {imm:#x}"),
            Op::ReadReg { dst, g } => write!(f, "{dst} = x{g}"),
            Op::WriteReg { g, src } => write!(f, "x{g} = {src}"),
            Op::Bin { op, dst, a, b } => write!(f, "{dst} = {} {a}, {b}", op.name()),
            Op::BinImm { op, dst, a, imm } => write!(f, "{dst} = {} {a}, {imm}", op.name()),
            Op::Load {
                dst,
                addr,
                off,
                size,
                signed,
            } => write!(
                f,
                "{dst} = load.{}{size} [{addr}{off:+}]",
                if signed { 's' } else { 'u' }
            ),
            Op::Store {
                addr,
                off,
                val,
                size,
            } => write!(f, "store.{size} [{addr}{off:+}], {val}"),
            Op::Interp { raw, pc, idx } => write!(f, "interp #{idx} {raw:#010x} @{pc:#x}"),
            Op::Branch {
                cond,
                a,
                b,
                taken,
                fall,
            } => write!(f, "br.{cond:?} {a}, {b} ? {taken:#x} : {fall:#x}"),
            Op::Jump { pc } => write!(f, "jump {pc:#x}"),
            Op::JumpInd { target } => write!(f, "jump [{target}]"),
            Op::Exit { kind, pc } => write!(f, "exit {kind:?} @{pc:#x}"),
        }
    }
}

impl fmt::Display for Block {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "block @{:#x}: {} insns, {} values",
            self.pc, self.n_insns, self.nvals
        )?;
        for op in &self.ops {
            if matches!(op, Op::Insn { .. }) {
                writeln!(f, "  {op}")?;
            } else {
                writeln!(f, "    {op}")?;
            }
        }
        Ok(())
    }
}
