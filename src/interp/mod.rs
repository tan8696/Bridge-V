//! Reference interpreter over pre-decoded basic blocks (CLAUDE.md §5, P1.9).
//!
//! It is the golden model for lockstep testing, the speedup baseline and the JIT's fallback
//! helper (D14). Blocks are decoded once (ending at the rules of §13.2, simplified: control
//! flow, system/CSR instructions, a page boundary or `MAX_BLOCK` instructions) and cached by
//! pc; FENCE.I and `riscv_flush_icache` flush the cache.

use std::rc::Rc;

use rustc_hash::FxHashMap;

use crate::cpu::csr::mstatus;
use crate::cpu::fp;
use crate::cpu::state::CpuState;
use crate::cpu::trap::{Exception, cause, prv};
use crate::isa::decode_parts;
use crate::isa::disasm::disasm;
use crate::isa::inst::*;
use crate::mem::MemFault;
use crate::mem::direct::DirectMem;

const MAX_BLOCK: usize = 64;

/// A pre-decoded basic block.
pub struct Block {
    pub insns: Vec<Decoded>,
    /// Raised when execution falls off the end of `insns` (the next fetch faulted while the
    /// block was built).
    pub fetch_fault: Option<Exception>,
}

/// Why `Engine::run` returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    /// User mode: ECALL at `cpu.pc`; the caller services the syscall.
    Ecall,
    /// User mode: an exception at `cpu.pc`, which is fatal for the process.
    Fault(Exception),
    /// The instruction budget passed to `run` was used up.
    Limit,
    /// Bare mode: the guest wrote this nonzero value to `tohost`.
    Tohost(u64),
    /// Lockstep: the JIT and the interpreter disagreed (details already printed).
    Diverged,
}

/// Execution environment of `Interp::run`.
#[derive(Clone, Copy, Debug, Default)]
pub struct Env {
    /// Linux user-mode emulation: ECALL and exceptions return to the caller instead of
    /// trapping into guest privileged code.
    pub user_mode: bool,
    /// Bare mode: address of the HTIF `tohost` word, polled at every block boundary.
    pub tohost: Option<u64>,
    /// Print every executed instruction to stderr.
    pub trace: bool,
}

/// An execution engine: the interpreter, the JIT, or the lockstep checker (CLAUDE.md §5).
pub trait Engine {
    /// Execute until a `Stop` condition, or until `max_insns` more instructions have retired
    /// (checked at block boundaries).
    fn run(&mut self, cpu: &mut CpuState, mem: &mut DirectMem, env: &Env, max_insns: u64) -> Stop;
    /// Guest code may have changed (FENCE.I, `mmap`/`munmap` of executable memory,
    /// `riscv_flush_icache`): drop all decoded or translated code.
    fn flush(&mut self);
    /// Engine statistics for `--stats` (empty if none).
    fn stats(&self) -> String {
        String::new()
    }
}

/// Deliver the outcome of a block the way the environment requires: in user mode ECALLs and
/// exceptions stop execution (`Err`), otherwise they trap into guest privileged code.
/// `Flush` must be handled by the caller (it owns the code cache).
pub fn deliver(exit: BlockExit, env: &Env, cpu: &mut CpuState) -> Result<(), Stop> {
    match exit {
        BlockExit::Continue | BlockExit::Flush => Ok(()),
        BlockExit::Ecall => {
            if env.user_mode {
                return Err(Stop::Ecall);
            }
            let c = ecall_cause(cpu);
            cpu.take_trap(Exception { cause: c, tval: 0 }, false);
            Ok(())
        }
        BlockExit::Trap(e) => {
            if env.user_mode {
                return Err(Stop::Fault(e));
            }
            cpu.take_trap(e, false);
            Ok(())
        }
    }
}

/// The `tohost` check done at every block boundary in bare mode.
#[inline]
pub fn tohost_written(mem: &DirectMem, env: &Env) -> Option<u64> {
    let th = env.tohost?;
    match mem.load(th, 8) {
        Ok(0) | Err(_) => None,
        Ok(v) => Some(v),
    }
}

#[derive(Default)]
pub struct Interp {
    cache: FxHashMap<u64, Rc<Block>>,
    /// Blocks decoded so far (statistics).
    pub blocks_built: u64,
}

/// Result of executing one instruction (`step`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flow {
    /// Fall through to the next instruction.
    Next,
    /// Control transfer to this pc (the instruction retired).
    Jump(u64),
    /// The instruction raised an exception (it did not retire).
    Trap(Exception),
    /// ECALL (not retired yet: the environment services it).
    Ecall,
    /// FENCE.I: flush the block cache, continue at the next instruction.
    Flush,
}

/// How `exec_block` ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockExit {
    /// Execution continues at `cpu.pc`.
    Continue,
    /// ECALL at `cpu.pc`.
    Ecall,
    /// Exception at `cpu.pc` (not yet delivered).
    Trap(Exception),
    /// FENCE.I retired; `cpu.pc` is the next instruction. Decoded code must be flushed.
    Flush,
}

/// Execute the pre-decoded instructions of one block starting at `cpu.pc`, updating `pc` and
/// `icount` exactly as the architecture requires. Traps are returned, not delivered. Shared by
/// the interpreter, the JIT's lockstep checker and (via `step`) the JIT fallback helper.
pub fn exec_block(
    cpu: &mut CpuState,
    mem: &mut DirectMem,
    insns: &[Decoded],
    fetch_fault: Option<Exception>,
    trace: bool,
) -> BlockExit {
    let mut pc = cpu.pc;
    for d in insns {
        if trace {
            eprintln!("{pc:016x}: {:08x}  {}", d.raw, disasm(&d.inst));
        }
        match step(cpu, mem, d, pc) {
            Flow::Next => {
                pc = pc.wrapping_add(d.len as u64);
                cpu.icount += 1;
            }
            Flow::Jump(target) => {
                cpu.icount += 1;
                cpu.pc = target;
                return BlockExit::Continue;
            }
            Flow::Flush => {
                cpu.icount += 1;
                cpu.pc = pc.wrapping_add(d.len as u64);
                return BlockExit::Flush;
            }
            Flow::Ecall => {
                cpu.pc = pc;
                return BlockExit::Ecall;
            }
            Flow::Trap(e) => {
                cpu.pc = pc;
                return BlockExit::Trap(e);
            }
        }
    }
    cpu.pc = pc;
    match fetch_fault {
        Some(e) => BlockExit::Trap(e),
        None => BlockExit::Continue,
    }
}

/// ECALL cause for the current privilege level.
pub fn ecall_cause(cpu: &CpuState) -> u64 {
    match cpu.prv {
        prv::U => cause::ECALL_U,
        prv::S => cause::ECALL_S,
        _ => cause::ECALL_M,
    }
}

impl Interp {
    pub fn new() -> Self {
        Self::default()
    }

    /// Drop all cached blocks (after code may have changed).
    pub fn flush(&mut self) {
        self.cache.clear();
    }

    fn block(&mut self, pc: u64, mem: &DirectMem) -> Rc<Block> {
        if let Some(b) = self.cache.get(&pc) {
            return b.clone();
        }
        let b = Rc::new(build_block(pc, mem));
        self.blocks_built += 1;
        self.cache.insert(pc, b.clone());
        b
    }

    /// Execute until a `Stop` condition, or until `max_insns` more instructions have retired
    /// (checked at block boundaries).
    pub fn run(
        &mut self,
        cpu: &mut CpuState,
        mem: &mut DirectMem,
        env: &Env,
        max_insns: u64,
    ) -> Stop {
        let limit = cpu.icount.saturating_add(max_insns);
        loop {
            if cpu.icount >= limit {
                return Stop::Limit;
            }
            if let Some(v) = tohost_written(mem, env) {
                return Stop::Tohost(v);
            }
            let block = self.block(cpu.pc, mem);
            let exit = exec_block(cpu, mem, &block.insns, block.fetch_fault, env.trace);
            if exit == BlockExit::Flush {
                self.flush();
            }
            if let Err(stop) = deliver(exit, env, cpu) {
                return stop;
            }
        }
    }
}

impl Engine for Interp {
    fn run(&mut self, cpu: &mut CpuState, mem: &mut DirectMem, env: &Env, max_insns: u64) -> Stop {
        Interp::run(self, cpu, mem, env, max_insns)
    }

    fn flush(&mut self) {
        Interp::flush(self);
    }

    fn stats(&self) -> String {
        format!("interp: {} blocks decoded", self.blocks_built)
    }
}

/// Decode a block starting at `pc`, stopping at a block-ending instruction, a page boundary,
/// `MAX_BLOCK` instructions, or a fetch fault (recorded in `fetch_fault`).
pub fn build_block(pc: u64, mem: &DirectMem) -> Block {
    build_block_max(pc, mem, MAX_BLOCK)
}

/// `build_block` with an instruction limit of `max` (≥ 1).
pub fn build_block_max(pc: u64, mem: &DirectMem, max: usize) -> Block {
    let mut insns = Vec::new();
    let mut a = pc;
    let fault = |tval| Exception {
        cause: cause::INSN_ACCESS,
        tval,
    };
    loop {
        let lo = match mem.fetch16(a) {
            Ok(v) => v,
            Err(_) => {
                return Block {
                    insns,
                    fetch_fault: Some(fault(a)),
                };
            }
        };
        let d = match decode_parts(lo, || mem.fetch16(a.wrapping_add(2))) {
            Ok(d) => d,
            // The second halfword of a 32-bit instruction faulted.
            Err(_) => {
                return Block {
                    insns,
                    fetch_fault: Some(fault(a.wrapping_add(2))),
                };
            }
        };
        a = a.wrapping_add(d.len as u64);
        let end = d.inst.ends_block();
        insns.push(d);
        if end || insns.len() >= max || a & 0xfff == 0 {
            return Block {
                insns,
                fetch_fault: None,
            };
        }
    }
}

fn mem_exc(f: MemFault) -> Exception {
    Exception::from(f)
}

/// 64-bit ALU semantics (register and immediate forms), CLAUDE.md §7.5.
#[inline(always)]
pub fn alu(op: AluOp, a: u64, b: u64) -> u64 {
    match op {
        AluOp::Add => a.wrapping_add(b),
        AluOp::Sub => a.wrapping_sub(b),
        AluOp::Sll => a << (b & 63),
        AluOp::Slt => ((a as i64) < (b as i64)) as u64,
        AluOp::Sltu => (a < b) as u64,
        AluOp::Xor => a ^ b,
        AluOp::Srl => a >> (b & 63),
        AluOp::Sra => ((a as i64) >> (b & 63)) as u64,
        AluOp::Or => a | b,
        AluOp::And => a & b,
        AluOp::Mul => a.wrapping_mul(b),
        AluOp::Mulh => (((a as i64 as i128) * (b as i64 as i128)) >> 64) as u64,
        AluOp::Mulhsu => (((a as i64 as i128) * (b as i128)) >> 64) as u64,
        AluOp::Mulhu => (((a as u128) * (b as u128)) >> 64) as u64,
        // Division never traps: x/0 = -1 (all ones), MIN/-1 = MIN (wrapping_div).
        AluOp::Div if b == 0 => u64::MAX,
        AluOp::Div => (a as i64).wrapping_div(b as i64) as u64,
        AluOp::Divu if b == 0 => u64::MAX,
        AluOp::Divu => a / b,
        // x%0 = x, MIN%-1 = 0 (wrapping_rem).
        AluOp::Rem if b == 0 => a,
        AluOp::Rem => (a as i64).wrapping_rem(b as i64) as u64,
        AluOp::Remu if b == 0 => a,
        AluOp::Remu => a % b,
    }
}

/// 32-bit ("W") ALU semantics: operate on the low 32 bits, sign-extend the result.
#[inline(always)]
pub fn aluw(op: AluWOp, a: u64, b: u64) -> u64 {
    let (a, b) = (a as u32, b as u32);
    let r: u32 = match op {
        AluWOp::Addw => a.wrapping_add(b),
        AluWOp::Subw => a.wrapping_sub(b),
        AluWOp::Sllw => a << (b & 31),
        AluWOp::Srlw => a >> (b & 31),
        AluWOp::Sraw => ((a as i32) >> (b & 31)) as u32,
        AluWOp::Mulw => a.wrapping_mul(b),
        AluWOp::Divw if b == 0 => u32::MAX,
        AluWOp::Divw => (a as i32).wrapping_div(b as i32) as u32,
        AluWOp::Divuw if b == 0 => u32::MAX,
        AluWOp::Divuw => a / b,
        AluWOp::Remw if b == 0 => a,
        AluWOp::Remw => (a as i32).wrapping_rem(b as i32) as u32,
        AluWOp::Remuw if b == 0 => a,
        AluWOp::Remuw => a % b,
    };
    r as i32 as i64 as u64
}

#[inline(always)]
/// Execute one decoded instruction at `pc` (the interpreter's semantics, used by the JIT's
/// fallback helper for everything it does not lower natively, D14).
pub fn step(cpu: &mut CpuState, mem: &mut DirectMem, d: &Decoded, pc: u64) -> Flow {
    let illegal = Flow::Trap(Exception::illegal(d.raw));
    let next_pc = pc.wrapping_add(d.len as u64);
    match d.inst {
        Inst::Lui { rd, imm } => cpu.set_x(rd, imm as u64),
        Inst::Auipc { rd, imm } => cpu.set_x(rd, pc.wrapping_add(imm as u64)),
        Inst::Jal { rd, imm } => {
            cpu.set_x(rd, next_pc);
            return Flow::Jump(pc.wrapping_add(imm as u64));
        }
        Inst::Jalr { rd, rs1, imm } => {
            // Read rs1 before writing rd: rd == rs1 is legal.
            let target = cpu.x[rs1 as usize].wrapping_add(imm as u64) & !1;
            cpu.set_x(rd, next_pc);
            return Flow::Jump(target);
        }
        Inst::Branch { op, rs1, rs2, imm } => {
            let (a, b) = (cpu.x[rs1 as usize], cpu.x[rs2 as usize]);
            let taken = match op {
                BranchOp::Beq => a == b,
                BranchOp::Bne => a != b,
                BranchOp::Blt => (a as i64) < (b as i64),
                BranchOp::Bge => (a as i64) >= (b as i64),
                BranchOp::Bltu => a < b,
                BranchOp::Bgeu => a >= b,
            };
            if taken {
                return Flow::Jump(pc.wrapping_add(imm as u64));
            }
        }
        Inst::Load { op, rd, rs1, imm } => {
            let addr = cpu.x[rs1 as usize].wrapping_add(imm as u64);
            // Loads into x0 still access memory (they may fault).
            let r = match op {
                LoadOp::Lb => mem.load(addr, 1).map(|v| v as u8 as i8 as u64),
                LoadOp::Lh => mem.load(addr, 2).map(|v| v as u16 as i16 as u64),
                LoadOp::Lw => mem.load(addr, 4).map(|v| v as u32 as i32 as u64),
                LoadOp::Ld => mem.load(addr, 8),
                LoadOp::Lbu => mem.load(addr, 1),
                LoadOp::Lhu => mem.load(addr, 2),
                LoadOp::Lwu => mem.load(addr, 4),
            };
            match r {
                Ok(v) => cpu.set_x(rd, v),
                Err(f) => return Flow::Trap(mem_exc(f)),
            }
        }
        Inst::Store { op, rs1, rs2, imm } => {
            let addr = cpu.x[rs1 as usize].wrapping_add(imm as u64);
            let size = match op {
                StoreOp::Sb => 1,
                StoreOp::Sh => 2,
                StoreOp::Sw => 4,
                StoreOp::Sd => 8,
            };
            if let Err(f) = mem.store(addr, size, cpu.x[rs2 as usize]) {
                return Flow::Trap(mem_exc(f));
            }
        }
        Inst::OpImm { op, rd, rs1, imm } => {
            let v = alu(op, cpu.x[rs1 as usize], imm as u64);
            cpu.set_x(rd, v)
        }
        Inst::OpImmW { op, rd, rs1, imm } => {
            let v = aluw(op, cpu.x[rs1 as usize], imm as u64);
            cpu.set_x(rd, v)
        }
        Inst::Op { op, rd, rs1, rs2 } => {
            let v = alu(op, cpu.x[rs1 as usize], cpu.x[rs2 as usize]);
            cpu.set_x(rd, v)
        }
        Inst::OpW { op, rd, rs1, rs2 } => {
            let v = aluw(op, cpu.x[rs1 as usize], cpu.x[rs2 as usize]);
            cpu.set_x(rd, v)
        }
        // Single hart: x86 TSO already orders everything a FENCE can require here.
        Inst::Fence { .. } => {}
        Inst::FenceI => return Flow::Flush,
        Inst::Ecall => return Flow::Ecall,
        Inst::Ebreak => {
            return Flow::Trap(Exception {
                cause: cause::BREAKPOINT,
                tval: pc,
            });
        }
        Inst::Mret => {
            if cpu.prv != prv::M {
                return illegal;
            }
            return Flow::Jump(cpu.mret());
        }
        Inst::Sret => {
            let tsr = cpu.csr.mstatus & mstatus::TSR != 0;
            if cpu.prv == prv::U || (cpu.prv == prv::S && tsr) {
                return illegal;
            }
            return Flow::Jump(cpu.sret());
        }
        Inst::Wfi => {
            // Phase 7 adds waiting for interrupts; until then WFI is a legal no-op.
            let tw = cpu.csr.mstatus & mstatus::TW != 0;
            if cpu.prv == prv::U || (cpu.prv == prv::S && tw) {
                return illegal;
            }
        }
        Inst::SfenceVma { .. } => {
            let tvm = cpu.csr.mstatus & mstatus::TVM != 0;
            if cpu.prv == prv::U || (cpu.prv == prv::S && tvm) {
                return illegal;
            }
            // No TLB until Phase 7: nothing to flush.
        }
        Inst::Csr {
            op,
            rd,
            rs1,
            uimm,
            csr,
        } => {
            let src = if uimm {
                rs1 as u64
            } else {
                cpu.x[rs1 as usize]
            };
            // CSRRS/CSRRC with rs1 = x0 (or uimm = 0) do not write; CSRRW with rd = x0 does
            // not read (CLAUDE.md §7.5).
            let writes = op == CsrOp::Rw || rs1 != 0;
            let reads = !(op == CsrOp::Rw && rd == 0);
            let old = if reads {
                match cpu.csr_read(csr) {
                    Some(v) => v,
                    None => return illegal,
                }
            } else {
                0
            };
            if writes {
                let new = match op {
                    CsrOp::Rw => src,
                    CsrOp::Rs => old | src,
                    CsrOp::Rc => old & !src,
                };
                if !cpu.csr_write(csr, new) {
                    return illegal;
                }
            }
            cpu.set_x(rd, old);
        }
        Inst::Amo {
            op,
            width,
            rd,
            rs1,
            rs2,
            ..
        } => {
            if let Err(e) = exec_amo(cpu, mem, op, width, rd, rs1, rs2) {
                return Flow::Trap(e);
            }
        }
        Inst::FLoad { .. } | Inst::FStore { .. } | Inst::Fma { .. } | Inst::Fp { .. } => {
            if let Err(e) = fp::exec(cpu, mem, &d.inst, d.raw) {
                return Flow::Trap(e);
            }
        }
        Inst::Illegal(_) => return illegal,
    }
    Flow::Next
}

fn exec_amo(
    cpu: &mut CpuState,
    mem: &mut DirectMem,
    op: AmoOp,
    width: AmoWidth,
    rd: u8,
    rs1: u8,
    rs2: u8,
) -> Result<(), Exception> {
    let addr = cpu.x[rs1 as usize];
    let w = width == AmoWidth::W;
    let size = if w { 4 } else { 8 };
    // LR/SC/AMOs require natural alignment.
    if !addr.is_multiple_of(size) {
        let c = if op == AmoOp::Lr {
            cause::LOAD_MISALIGNED
        } else {
            cause::STORE_MISALIGNED
        };
        return Err(Exception {
            cause: c,
            tval: addr,
        });
    }
    let sext = |v: u64| if w { v as u32 as i32 as u64 } else { v };
    match op {
        AmoOp::Lr => {
            let v = mem.load(addr, size).map_err(mem_exc)?;
            cpu.res_addr = addr;
            cpu.res_val = v;
            cpu.res_valid = 1;
            cpu.set_x(rd, sext(v));
        }
        AmoOp::Sc => {
            let ok = cpu.res_valid != 0 && cpu.res_addr == addr;
            cpu.res_valid = 0; // any SC clears the reservation
            if ok {
                mem.store(addr, size, cpu.x[rs2 as usize])
                    .map_err(mem_exc)?;
            }
            cpu.set_x(rd, (!ok) as u64);
        }
        _ => {
            let old = mem.load_for_amo(addr, size).map_err(mem_exc)?;
            let b = cpu.x[rs2 as usize];
            let (so, sb) = (sext(old) as i64, sext(b) as i64);
            let (uo, ub) = if w {
                (old as u32 as u64, b as u32 as u64)
            } else {
                (old, b)
            };
            let new = match op {
                AmoOp::Swap => b,
                AmoOp::Add => old.wrapping_add(b),
                AmoOp::Xor => old ^ b,
                AmoOp::And => old & b,
                AmoOp::Or => old | b,
                AmoOp::Min => so.min(sb) as u64,
                AmoOp::Max => so.max(sb) as u64,
                AmoOp::Minu => uo.min(ub),
                AmoOp::Maxu => uo.max(ub),
                AmoOp::Lr | AmoOp::Sc => unreachable!(),
            };
            mem.store(addr, size, new).map_err(mem_exc)?;
            cpu.set_x(rd, sext(old));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn division_edge_cases() {
        let min = i64::MIN as u64;
        let m1 = u64::MAX;
        assert_eq!(alu(AluOp::Div, 7, 0), u64::MAX);
        assert_eq!(alu(AluOp::Divu, 7, 0), u64::MAX);
        assert_eq!(alu(AluOp::Rem, 7, 0), 7);
        assert_eq!(alu(AluOp::Remu, 7, 0), 7);
        assert_eq!(alu(AluOp::Div, min, m1), min);
        assert_eq!(alu(AluOp::Rem, min, m1), 0);
        assert_eq!(aluw(AluWOp::Divw, 7, 0), u64::MAX);
        assert_eq!(aluw(AluWOp::Divuw, 7, 0), u64::MAX, "u32::MAX sign-extends");
        assert_eq!(
            aluw(AluWOp::Divw, 0x8000_0000, 0xffff_ffff),
            0xffff_ffff_8000_0000
        );
        assert_eq!(aluw(AluWOp::Remw, 0x8000_0000, 0xffff_ffff), 0);
        assert_eq!(
            aluw(AluWOp::Remuw, 0xffff_ffff, 0),
            u64::MAX,
            "remainder sign-extends"
        );
    }

    #[test]
    fn mul_high_variants() {
        let m1 = u64::MAX; // -1
        assert_eq!(alu(AluOp::Mulh, m1, m1), 0); // (-1)*(-1) = 1
        assert_eq!(alu(AluOp::Mulhu, m1, m1), u64::MAX - 1);
        assert_eq!(alu(AluOp::Mulhsu, m1, m1), u64::MAX); // -1 * (2^64-1) = -(2^64-1)
        assert_eq!(alu(AluOp::Mulhsu, 2, m1), 1);
    }

    #[test]
    fn w_ops_sign_extend() {
        assert_eq!(aluw(AluWOp::Addw, 0x7fff_ffff, 1), 0xffff_ffff_8000_0000);
        assert_eq!(aluw(AluWOp::Sllw, 1, 31), 0xffff_ffff_8000_0000);
        assert_eq!(aluw(AluWOp::Srlw, 0xffff_ffff_8000_0000, 31), 1);
        assert_eq!(aluw(AluWOp::Sraw, 0x8000_0000, 31), u64::MAX);
        assert_eq!(
            aluw(AluWOp::Sllw, 1, 32 + 3),
            8,
            "shift amount uses rs2[4:0]"
        );
        assert_eq!(alu(AluOp::Sll, 1, 64 + 3), 8, "shift amount uses rs2[5:0]");
    }

    #[test]
    fn set_less_than() {
        assert_eq!(alu(AluOp::Slt, u64::MAX, 0), 1);
        assert_eq!(alu(AluOp::Sltu, u64::MAX, 0), 0);
        // sltiu compares with the sign-extended immediate as unsigned: x < 0xfff..f is true.
        assert_eq!(alu(AluOp::Sltu, 5, -1i64 as u64), 1);
    }
}
