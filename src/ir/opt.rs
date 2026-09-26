//! Optimizer passes (CLAUDE.md §9 passes 2–4, P4.3). All are single linear sweeps.
//!
//! 1. `forward`: guest-register forwarding and read CSE. A `ReadReg g` after a known value of
//!    `g` (an earlier read or write) is replaced by that value; a write of `g`'s own known
//!    value is dropped. `Interp` may change any guest register, so it forgets everything.
//! 2. `dead_writes`: a `WriteReg g` overwritten later is removed if no possible fault site or
//!    helper call (`Load`, `Store`, `Interp`) lies in between (at a fault site the older value
//!    is architecturally visible, §9 pass 4). Across fault sites the register allocator's lazy
//!    write-back does the rest: only the value live at an exit or fault is ever stored.
//! 3. `fold`: constant propagation and folding (LUI+ADDI, AUIPC, `x op 0` identities),
//!    register-register ops with a constant operand become immediate forms, branches on
//!    constants become jumps, and `JumpInd` to a constant becomes a chainable `Jump`.
//! 4. `dce`: removes pure ops whose result is unused (loads are kept: they may fault).

use super::ops::{BinOp, Block, Op, V};

/// Run every pass in order.
pub fn optimize(b: &mut Block) {
    forward(b);
    dead_writes(b);
    fold(b);
    dce(b);
}

/// Apply `rename` (value → representative) to every use; drop ops marked `None`.
fn rebuild(b: &mut Block, ops: Vec<Option<Op>>) {
    b.ops = ops.into_iter().flatten().collect();
}

pub fn forward(b: &mut Block) {
    let mut rename: Vec<V> = (0..b.nvals).map(V).collect();
    let mut cur: [Option<V>; 32] = [None; 32];
    let mut out = Vec::with_capacity(b.ops.len());
    for op in &b.ops {
        let mut op = *op;
        op.map_uses(|v| rename[v.0 as usize]);
        match op {
            Op::ReadReg { dst, g } => {
                if let Some(v) = cur[g as usize] {
                    rename[dst.0 as usize] = v;
                    continue;
                }
                cur[g as usize] = Some(dst);
            }
            Op::WriteReg { g, src } => {
                if cur[g as usize] == Some(src) {
                    continue; // g already holds this value
                }
                cur[g as usize] = Some(src);
            }
            Op::Interp { .. } => cur = [None; 32],
            _ => {}
        }
        out.push(Some(op));
    }
    rebuild(b, out);
}

pub fn dead_writes(b: &mut Block) {
    let mut ops: Vec<Option<Op>> = b.ops.iter().copied().map(Some).collect();
    // Index of the last write of each guest register since the last fault site / helper.
    let mut pending: [Option<usize>; 32] = [None; 32];
    for i in 0..ops.len() {
        match ops[i] {
            Some(Op::WriteReg { g, .. }) => {
                if let Some(j) = pending[g as usize] {
                    ops[j] = None;
                }
                pending[g as usize] = Some(i);
            }
            // A read makes the earlier write live (only possible without `forward`).
            Some(Op::ReadReg { g, .. }) => pending[g as usize] = None,
            Some(Op::Load { .. } | Op::Store { .. } | Op::Interp { .. }) => pending = [None; 32],
            _ => {}
        }
    }
    rebuild(b, ops);
}

fn fits_i32(v: u64) -> bool {
    v as i64 == v as i32 as i64
}

/// Can `op` have an immediate operand in the backend (`BinImm`)?
fn has_imm_form(op: BinOp) -> bool {
    use BinOp::*;
    matches!(
        op,
        Add | And
            | Or
            | Xor
            | Sll
            | Srl
            | Sra
            | Slt
            | Sltu
            | Mul
            | Addw
            | Sllw
            | Srlw
            | Sraw
            | Mulw
    )
}

pub fn fold(b: &mut Block) {
    use BinOp::*;
    let mut rename: Vec<V> = (0..b.nvals).map(V).collect();
    let mut k: Vec<Option<u64>> = vec![None; b.nvals as usize];
    let mut out = Vec::with_capacity(b.ops.len());
    for op in &b.ops {
        let mut op = *op;
        op.map_uses(|v| rename[v.0 as usize]);
        let kv = |k: &[Option<u64>], v: V| k[v.0 as usize];
        let new = match op {
            Op::Bin { op: o, dst, a, b } => match (kv(&k, a), kv(&k, b)) {
                (Some(x), Some(y)) => Op::Const {
                    dst,
                    imm: o.eval(x, y),
                },
                (_, Some(y)) if o == Sub && fits_i32(y.wrapping_neg()) => Op::BinImm {
                    op: Add,
                    dst,
                    a,
                    imm: y.wrapping_neg() as i64,
                },
                (_, Some(y)) if o == Subw && fits_i32(y.wrapping_neg()) => Op::BinImm {
                    op: Addw,
                    dst,
                    a,
                    imm: y.wrapping_neg() as i64,
                },
                (_, Some(y)) if has_imm_form(o) && fits_i32(y) => Op::BinImm {
                    op: o,
                    dst,
                    a,
                    imm: y as i64,
                },
                (Some(x), _) if o.is_commutative() && has_imm_form(o) && fits_i32(x) => {
                    Op::BinImm {
                        op: o,
                        dst,
                        a: b,
                        imm: x as i64,
                    }
                }
                _ => op,
            },
            _ => op,
        };
        // Second stage: immediate forms and identities.
        let new = match new {
            Op::BinImm { op: o, dst, a, imm } => {
                if let Some(x) = kv(&k, a) {
                    Op::Const {
                        dst,
                        imm: o.eval(x, imm as u64),
                    }
                } else {
                    let identity = match o {
                        Add | Or | Xor => imm == 0,
                        Sll | Srl | Sra => imm & 63 == 0,
                        And => imm == -1,
                        Mul => imm == 1,
                        _ => false,
                    };
                    let zero = matches!(o, And | Mul) && imm == 0;
                    if identity {
                        rename[dst.0 as usize] = a;
                        continue;
                    } else if zero {
                        Op::Const { dst, imm: 0 }
                    } else {
                        new
                    }
                }
            }
            Op::Branch {
                cond,
                a,
                b,
                taken,
                fall,
            } => match (kv(&k, a), kv(&k, b)) {
                (Some(x), Some(y)) => Op::Jump {
                    pc: if cond.eval(x, y) { taken } else { fall },
                },
                // Comparing a value with itself.
                _ if a == b => Op::Jump {
                    pc: if cond.eval(0, 0) { taken } else { fall },
                },
                _ => new,
            },
            Op::JumpInd { target } => match kv(&k, target) {
                Some(t) => Op::Jump { pc: t },
                None => new,
            },
            _ => new,
        };
        if let Op::Const { dst, imm } = new {
            k[dst.0 as usize] = Some(imm);
        }
        out.push(Some(new));
    }
    rebuild(b, out);
}

pub fn dce(b: &mut Block) {
    let mut used = vec![false; b.nvals as usize];
    let mut keep = vec![true; b.ops.len()];
    for (i, op) in b.ops.iter().enumerate().rev() {
        if op.is_pure()
            && let Some(d) = op.def()
            && !used[d.0 as usize]
        {
            keep[i] = false;
            continue;
        }
        for u in op.uses() {
            used[u.0 as usize] = true;
        }
    }
    let mut i = 0;
    b.ops.retain(|_| {
        i += 1;
        keep[i - 1]
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::ops::Cond;

    fn blk(ops: Vec<Op>, nvals: u32) -> Block {
        Block {
            pc: 0x1000,
            ops,
            n_insns: 1,
            nvals,
        }
    }

    const fn v(n: u32) -> V {
        V(n)
    }

    #[test]
    fn forward_replaces_reads_and_drops_self_writes() {
        let mut b = blk(
            vec![
                Op::ReadReg { dst: v(0), g: 5 },
                Op::ReadReg { dst: v(1), g: 5 }, // CSE → v0
                Op::BinImm {
                    op: BinOp::Add,
                    dst: v(2),
                    a: v(1),
                    imm: 1,
                },
                Op::WriteReg { g: 6, src: v(2) },
                Op::ReadReg { dst: v(3), g: 6 },  // forwarded → v2
                Op::WriteReg { g: 5, src: v(0) }, // x5 already holds v0 → dropped
                Op::Interp {
                    raw: 0,
                    pc: 0,
                    idx: 0,
                },
                Op::ReadReg { dst: v(4), g: 6 }, // after a helper: must stay
                Op::Branch {
                    cond: Cond::Eq,
                    a: v(3),
                    b: v(4),
                    taken: 1,
                    fall: 2,
                },
            ],
            5,
        );
        forward(&mut b);
        assert_eq!(
            b.ops,
            vec![
                Op::ReadReg { dst: v(0), g: 5 },
                Op::BinImm {
                    op: BinOp::Add,
                    dst: v(2),
                    a: v(0),
                    imm: 1,
                },
                Op::WriteReg { g: 6, src: v(2) },
                Op::Interp {
                    raw: 0,
                    pc: 0,
                    idx: 0,
                },
                Op::ReadReg { dst: v(4), g: 6 },
                Op::Branch {
                    cond: Cond::Eq,
                    a: v(2),
                    b: v(4),
                    taken: 1,
                    fall: 2,
                },
            ]
        );
    }

    #[test]
    fn dead_writes_respect_fault_sites() {
        let w = |g, s| Op::WriteReg { g, src: v(s) };
        let mut b = blk(
            vec![
                Op::Const { dst: v(0), imm: 1 },
                w(5, 0), // dead: overwritten below, no fault site between
                w(5, 0),
                Op::Load {
                    dst: v(1),
                    addr: v(0),
                    off: 0,
                    size: 8,
                    signed: false,
                },
                w(5, 1), // kept: the load in between may fault while x5 = v0
                Op::Jump { pc: 0 },
            ],
            2,
        );
        dead_writes(&mut b);
        assert_eq!(
            b.ops
                .iter()
                .filter(|o| matches!(o, Op::WriteReg { .. }))
                .count(),
            2
        );
        assert_eq!(b.ops[1], w(5, 0));
        assert!(matches!(b.ops[2], Op::Load { .. }));
    }

    #[test]
    fn fold_lui_addi_identities_and_branches() {
        let mut b = blk(
            vec![
                Op::Const {
                    dst: v(0),
                    imm: 0x12345000,
                }, // lui
                Op::BinImm {
                    op: BinOp::Addw,
                    dst: v(1),
                    a: v(0),
                    imm: 0x678,
                }, // addiw
                Op::ReadReg { dst: v(2), g: 7 },
                Op::BinImm {
                    op: BinOp::Add,
                    dst: v(3),
                    a: v(2),
                    imm: 0,
                }, // mv: identity
                Op::Bin {
                    op: BinOp::Sub,
                    dst: v(4),
                    a: v(3),
                    b: v(1),
                }, // sub x, const → add x, -const
                Op::WriteReg { g: 8, src: v(4) },
                Op::Branch {
                    cond: Cond::Ltu,
                    a: v(0),
                    b: v(1),
                    taken: 0x10,
                    fall: 0x20,
                },
            ],
            5,
        );
        fold(&mut b);
        dce(&mut b);
        assert_eq!(
            b.ops,
            vec![
                Op::ReadReg { dst: v(2), g: 7 },
                Op::BinImm {
                    op: BinOp::Add,
                    dst: v(4),
                    a: v(2),
                    imm: -0x12345678,
                },
                Op::WriteReg { g: 8, src: v(4) },
                Op::Jump { pc: 0x10 },
            ]
        );
    }

    #[test]
    fn jalr_to_constant_becomes_jump_and_loads_survive_dce() {
        let mut b = blk(
            vec![
                Op::Const {
                    dst: v(0),
                    imm: 0x2001,
                },
                Op::Load {
                    dst: v(1),
                    addr: v(0),
                    off: 0,
                    size: 1,
                    signed: false,
                }, // unused, but may fault
                Op::BinImm {
                    op: BinOp::And,
                    dst: v(2),
                    a: v(0),
                    imm: -2,
                },
                Op::JumpInd { target: v(2) },
            ],
            3,
        );
        optimize(&mut b);
        assert_eq!(b.ops.len(), 3);
        assert!(matches!(b.ops[1], Op::Load { .. }));
        assert_eq!(b.ops[2], Op::Jump { pc: 0x2000 });
    }
}
