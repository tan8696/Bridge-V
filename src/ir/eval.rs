//! IR evaluator (P4.2): executes a `Block` directly against `CpuState` and guest memory with
//! the same observable semantics as `interp::exec_block` over the block's instructions. It is
//! the oracle that separates lifter and optimizer bugs from backend bugs: lift → eval must
//! equal the interpreter, and every optimizer pass must preserve eval results.

use crate::cpu::state::CpuState;
use crate::cpu::trap::Exception;
use crate::interp::{BlockExit, Flow, step};
use crate::isa::decode_parts;
use crate::mem::direct::DirectMem;

use super::ops::{Block, ExitKind, Op};

fn load(mem: &DirectMem, addr: u64, size: u8, signed: bool) -> Result<u64, Exception> {
    let v = mem.load(addr, size as u64).map_err(Exception::from)?;
    Ok(match (size, signed) {
        (1, true) => v as u8 as i8 as u64,
        (2, true) => v as u16 as i16 as u64,
        (4, true) => v as u32 as i32 as u64,
        _ => v,
    })
}

/// Run `b` from `cpu.pc == b.pc`. Updates registers, memory, `pc` and `icount` exactly like the
/// interpreter; traps are returned, not delivered.
pub fn eval(b: &Block, cpu: &mut CpuState, mem: &mut DirectMem) -> BlockExit {
    let mut vals = vec![0u64; b.nvals as usize];
    let base = cpu.icount;
    let (mut pc, mut idx) = (b.pc, 0u32);
    for op in &b.ops {
        match *op {
            Op::Insn { pc: p, idx: i } => {
                pc = p;
                idx = i;
            }
            Op::Const { dst, imm } => vals[dst.0 as usize] = imm,
            Op::ReadReg { dst, g } => vals[dst.0 as usize] = cpu.x[g as usize],
            Op::WriteReg { g, src } => cpu.set_x(g, vals[src.0 as usize]),
            Op::Bin { op, dst, a, b } => {
                vals[dst.0 as usize] = op.eval(vals[a.0 as usize], vals[b.0 as usize]);
            }
            Op::BinImm { op, dst, a, imm } => {
                vals[dst.0 as usize] = op.eval(vals[a.0 as usize], imm as u64);
            }
            Op::Load {
                dst,
                addr,
                off,
                size,
                signed,
            } => {
                let a = vals[addr.0 as usize].wrapping_add(off as i64 as u64);
                match load(mem, a, size, signed) {
                    Ok(v) => vals[dst.0 as usize] = v,
                    Err(e) => {
                        cpu.pc = pc;
                        cpu.icount = base + idx as u64;
                        return BlockExit::Trap(e);
                    }
                }
            }
            Op::Store {
                addr,
                off,
                val,
                size,
            } => {
                let a = vals[addr.0 as usize].wrapping_add(off as i64 as u64);
                if let Err(f) = mem.store(a, size as u64, vals[val.0 as usize]) {
                    cpu.pc = pc;
                    cpu.icount = base + idx as u64;
                    return BlockExit::Trap(f.into());
                }
            }
            Op::Interp { raw, pc: p, idx: i } => {
                cpu.icount = base + i as u64;
                let d =
                    decode_parts::<()>(raw as u16, || Ok((raw >> 16) as u16)).expect("infallible");
                match step(cpu, mem, &d, p) {
                    Flow::Next => {}
                    Flow::Jump(t) => {
                        cpu.icount += 1;
                        cpu.pc = t;
                        return BlockExit::Continue;
                    }
                    Flow::Flush => {
                        cpu.icount += 1;
                        cpu.pc = p.wrapping_add(d.len as u64);
                        return BlockExit::Flush;
                    }
                    Flow::Trap(e) => {
                        cpu.pc = p;
                        return BlockExit::Trap(e);
                    }
                    Flow::Ecall => {
                        cpu.pc = p;
                        return BlockExit::Ecall;
                    }
                }
            }
            Op::Branch {
                cond,
                a,
                b: bb,
                taken,
                fall,
            } => {
                let t = cond.eval(vals[a.0 as usize], vals[bb.0 as usize]);
                cpu.pc = if t { taken } else { fall };
                cpu.icount = base + b.n_insns as u64;
                return BlockExit::Continue;
            }
            Op::Jump { pc: t } => {
                cpu.pc = t;
                cpu.icount = base + b.n_insns as u64;
                return BlockExit::Continue;
            }
            Op::JumpInd { target } => {
                cpu.pc = vals[target.0 as usize];
                cpu.icount = base + b.n_insns as u64;
                return BlockExit::Continue;
            }
            Op::Exit { kind, pc: p } => {
                cpu.pc = p;
                return match kind {
                    ExitKind::Ecall => {
                        cpu.icount = base + b.n_insns as u64 - 1;
                        BlockExit::Ecall
                    }
                    ExitKind::FenceI => {
                        cpu.icount = base + b.n_insns as u64;
                        BlockExit::Flush
                    }
                    ExitKind::Fault(e) => {
                        cpu.icount = base + b.n_insns as u64;
                        BlockExit::Trap(e)
                    }
                };
            }
        }
    }
    unreachable!("IR block without a terminator")
}
