//! Liveness and live intervals (CLAUDE.md §9 pass 5, P4.4).
//!
//! A block is straight-line SSA code, so one sweep collects, for every value, its definition
//! position and the sorted positions of its uses. The live interval of `v` is
//! `[def, last_use]`; `next_use(v, p)` (the first use after position `p`) drives the
//! allocator's furthest-next-use spill choice. Fixed-register constraints (RAX/RDX for
//! multiply-high and division, RCX for shifts without BMI2, the whole pool at helper calls)
//! are properties of the ops at those positions and are handled by the allocator when it
//! reaches them (`constraint`).

use super::ops::{BinOp, Block, Op, V};

/// Registers an op needs for itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Constraint {
    None,
    /// RDX:RAX (MUL/IMUL one-operand, DIV/IDIV, CQO).
    RaxRdx,
    /// CL holds the shift count (no BMI2).
    Rcx,
    /// A helper call clobbers every caller-saved register.
    Call,
}

pub struct Liveness {
    def: Vec<u32>,
    uses: Vec<Vec<u32>>,
}

impl Liveness {
    pub fn compute(b: &Block) -> Liveness {
        let n = b.nvals as usize;
        let mut def = vec![u32::MAX; n];
        let mut uses = vec![Vec::new(); n];
        for (i, op) in b.ops.iter().enumerate() {
            for u in op.uses() {
                uses[u.0 as usize].push(i as u32);
            }
            if let Some(d) = op.def() {
                def[d.0 as usize] = i as u32;
            }
        }
        Liveness { def, uses }
    }

    /// Position of the first use of `v` strictly after `pos`.
    pub fn next_use(&self, v: V, pos: u32) -> Option<u32> {
        let u = &self.uses[v.0 as usize];
        let i = u.partition_point(|&p| p <= pos);
        u.get(i).copied()
    }

    /// All use positions of every value (sorted), consuming the analysis.
    pub fn into_uses(self) -> Vec<Vec<u32>> {
        self.uses
    }

    pub fn last_use(&self, v: V) -> Option<u32> {
        self.uses[v.0 as usize].last().copied()
    }

    /// Live interval `[def, last_use]` of every defined value (unused values: `[def, def]`).
    pub fn intervals(&self) -> Vec<(V, u32, u32)> {
        (0..self.def.len())
            .filter(|&v| self.def[v] != u32::MAX)
            .map(|v| {
                let d = self.def[v];
                (V(v as u32), d, self.last_use(V(v as u32)).unwrap_or(d))
            })
            .collect()
    }
}

/// The fixed-register constraint of `op` (`bmi2`: whether BMI2 shifts are available).
pub fn constraint(op: &Op, bmi2: bool) -> Constraint {
    use BinOp::*;
    let bin = match *op {
        Op::Bin { op, .. } => op,
        Op::Interp { .. } => return Constraint::Call,
        _ => return Constraint::None,
    };
    match bin {
        Mulh | Mulhu | Mulhsu | Div | Divu | Rem | Remu | Divw | Divuw | Remw | Remuw => {
            Constraint::RaxRdx
        }
        Sll | Srl | Sra | Sllw | Srlw | Sraw if !bmi2 => Constraint::Rcx,
        _ => Constraint::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intervals_and_next_use() {
        let v = V;
        let b = Block {
            pc: 0,
            ops: vec![
                Op::ReadReg { dst: v(0), g: 1 }, // 0
                Op::Const { dst: v(1), imm: 4 }, // 1
                Op::Bin {
                    op: BinOp::Add,
                    dst: v(2),
                    a: v(0),
                    b: v(1),
                }, // 2
                Op::Bin {
                    op: BinOp::Div,
                    dst: v(3),
                    a: v(2),
                    b: v(0),
                }, // 3
                Op::WriteReg { g: 3, src: v(3) }, // 4
                Op::Jump { pc: 0 },              // 5
            ],
            n_insns: 1,
            nvals: 4,
            fp_guard: false,
            fp_dyn: false,
        };
        let l = Liveness::compute(&b);
        assert_eq!(
            l.intervals(),
            vec![(v(0), 0, 3), (v(1), 1, 2), (v(2), 2, 3), (v(3), 3, 4)]
        );
        assert_eq!(l.next_use(v(0), 0), Some(2));
        assert_eq!(l.next_use(v(0), 2), Some(3));
        assert_eq!(l.next_use(v(0), 3), None);
        assert_eq!(constraint(&b.ops[3], true), Constraint::RaxRdx);
        let sll = Op::Bin {
            op: BinOp::Sll,
            dst: v(0),
            a: v(0),
            b: v(0),
        };
        assert_eq!(constraint(&sll, false), Constraint::Rcx);
        assert_eq!(constraint(&sll, true), Constraint::None);
    }
}
