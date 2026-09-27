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
use crate::mem::direct::DirectMem;
use crate::mem::{mmu, tlb};
use crate::stats::RegStats;

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
    /// System mode with the built-in SBI (D15): an S-mode ECALL stops the engine
    /// (`Stop::Ecall`) so the machine can serve it.
    pub sbi: bool,
}

/// An execution engine: the interpreter, the JIT, or the lockstep checker (CLAUDE.md §5).
pub trait Engine {
    /// Execute until a `Stop` condition, or until `max_insns` more instructions have retired
    /// (checked at block boundaries).
    fn run(&mut self, cpu: &mut CpuState, mem: &mut DirectMem, env: &Env, max_insns: u64) -> Stop;
    /// Guest code may have changed (`mmap`/`munmap` of executable memory): drop all decoded or
    /// translated code.
    fn flush(&mut self);
    /// FENCE.I semantics (`riscv_flush_icache`): with eager SMC invalidation (D49) nothing
    /// stale can remain, so engines may do less than `flush`.
    fn fence_i(&mut self) {
        self.flush();
    }
    /// Engine statistics for `--stats` (empty if none).
    fn stats(&self) -> String {
        String::new()
    }
    /// Collect the `--stats=regs` register-use histogram (reported by `stats`). Returns false
    /// if the engine cannot.
    fn enable_reg_stats(&mut self) -> bool {
        false
    }
}

/// Deliver the outcome of a block the way the environment requires: in user mode ECALLs and
/// exceptions stop execution (`Err`), otherwise they trap into guest privileged code.
/// `Flush` must be handled by the caller (it owns the code cache).
pub fn deliver(exit: BlockExit, env: &Env, cpu: &mut CpuState) -> Result<(), Stop> {
    match exit {
        BlockExit::Continue | BlockExit::Flush => Ok(()),
        BlockExit::Ecall => {
            if env.user_mode || (env.sbi && cpu.prv == prv::S) {
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

/// Take a pending, enabled interrupt (system/bare mode), at a block boundary (§15).
#[inline]
pub fn deliver_interrupt(cpu: &mut CpuState, env: &Env) -> bool {
    if env.user_mode || cpu.csr.mip & cpu.csr.mie == 0 {
        return false;
    }
    match cpu.pending_interrupt() {
        Some(c) => {
            cpu.take_trap(Exception { cause: c, tval: 0 }, true);
            true
        }
        None => false,
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
    /// Decoded blocks by (virtual pc, fetch MMU index). With softmmu the cache is dropped
    /// whenever translation may have changed (`CpuState::mmu_gen`).
    cache: FxHashMap<(u64, u8), Rc<Block>>,
    mmu_gen: u64,
    /// Blocks decoded so far (statistics).
    pub blocks_built: u64,
    /// `--stats=regs` histogram, when enabled.
    pub reg_stats: Option<Box<RegStats>>,
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
                // A store hit a page holding translated or decoded code: stop after it, so
                // the rest of the block is decoded again (D49).
                if !mem.smc_pages.is_empty() {
                    cpu.pc = pc;
                    return BlockExit::Continue;
                }
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

/// Mark the page(s) a decoded block came from as code pages (D49): writes to them are then
/// detected (and, for softmmu, take the store slow path). Physical pages with softmmu.
pub fn mark_block_code(cpu: &mut CpuState, mem: &mut DirectMem, pc: u64, insns: &[Decoded]) {
    let len: u64 = insns.iter().map(|d| d.len as u64).sum();
    let mut pages = [pc, pc.wrapping_add(len.max(1) - 1)];
    if cpu.softmmu != 0 {
        for p in pages.iter_mut() {
            // Already translated when the block was decoded: a TLB hit.
            *p = mmu::fetch_page(cpu, mem, *p).unwrap_or(u64::MAX);
        }
    }
    for p in pages {
        if p != u64::MAX && mem.mark_code(p) && cpu.softmmu != 0 {
            tlb::set_code_flag(cpu, mem.base() as u64, p & !0xfff, true);
        }
    }
}

/// Drop `mem.smc_pages` after the caller invalidated what it translated from them, clearing
/// their TLB code flags.
pub fn forget_smc_pages(cpu: &mut CpuState, mem: &mut DirectMem) {
    for p in std::mem::take(&mut mem.smc_pages) {
        if cpu.softmmu != 0 {
            tlb::set_code_flag(cpu, mem.base() as u64, p, false);
        }
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

    fn block(&mut self, cpu: &mut CpuState, mem: &mut DirectMem) -> Rc<Block> {
        let pc = cpu.pc;
        let key = if cpu.softmmu != 0 {
            if cpu.mmu_gen != self.mmu_gen {
                self.cache.clear();
                self.mmu_gen = cpu.mmu_gen;
            }
            (pc, tlb::fetch_idx(cpu))
        } else {
            (pc, 0)
        };
        if let Some(b) = self.cache.get(&key) {
            return b.clone();
        }
        let b = Rc::new(if cpu.softmmu != 0 {
            build_block_soft(pc, cpu, mem, MAX_BLOCK)
        } else {
            build_block(pc, mem)
        });
        self.blocks_built += 1;
        if let Some(r) = &mut self.reg_stats {
            r.block(&b.insns);
        }
        // A block whose first fetch faulted depends on the TLB state: don't keep it.
        if !(b.insns.is_empty() && cpu.softmmu != 0) {
            self.cache.insert(key, b.clone());
            mark_block_code(cpu, mem, pc, &b.insns);
        }
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
            deliver_interrupt(cpu, env);
            let block = self.block(cpu, mem);
            let before = cpu.icount;
            let exit = exec_block(cpu, mem, &block.insns, block.fetch_fault, env.trace);
            if let Some(r) = &mut self.reg_stats {
                // An ECALL retires when the environment services it: count it here.
                let ecall = usize::from(exit == BlockExit::Ecall);
                r.retired(&block.insns, (cpu.icount - before) as usize + ecall);
            }
            if exit == BlockExit::Flush {
                self.flush();
            }
            if !mem.smc_pages.is_empty() {
                // Code this cache decoded was written (D49): decode everything again.
                self.flush();
                forget_smc_pages(cpu, mem);
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
        let mut s = format!("interp: {} blocks decoded", self.blocks_built);
        if let Some(r) = &self.reg_stats {
            s += "\nguest register uses (--stats=regs):\n";
            s += &r.report();
        }
        s
    }

    fn enable_reg_stats(&mut self) -> bool {
        self.reg_stats = Some(Box::default());
        true
    }
}

/// Decode a block starting at `pc`, stopping at a block-ending instruction, a page boundary,
/// `MAX_BLOCK` instructions, or a fetch fault (recorded in `fetch_fault`).
pub fn build_block(pc: u64, mem: &DirectMem) -> Block {
    build_block_max(pc, mem, MAX_BLOCK)
}

/// `build_block` with an instruction limit of `max` (≥ 1), direct memory.
pub fn build_block_max(pc: u64, mem: &DirectMem, max: usize) -> Block {
    build_block_by(pc, max, false, |a| mem.fetch16(a).map_err(Exception::from))
}

/// `build_block` through the current fetch translation (softmmu, D48). A 32-bit instruction
/// that straddles a page boundary is only included as the first instruction of a block.
pub fn build_block_soft(pc: u64, cpu: &mut CpuState, mem: &mut DirectMem, max: usize) -> Block {
    build_block_by(pc, max, true, |a| mmu::fetch16(cpu, mem, a))
}

fn build_block_by(
    pc: u64,
    max: usize,
    no_straddle: bool,
    mut fetch: impl FnMut(u64) -> Result<u16, Exception>,
) -> Block {
    let mut insns = Vec::new();
    let mut a = pc;
    loop {
        let lo = match fetch(a) {
            Ok(v) => v,
            Err(e) => {
                return Block {
                    insns,
                    fetch_fault: Some(e),
                };
            }
        };
        if no_straddle && !insns.is_empty() && a & 0xfff == 0xffe && lo & 3 == 3 {
            return Block {
                insns,
                fetch_fault: None,
            };
        }
        let d = match decode_parts(lo, || fetch(a.wrapping_add(2))) {
            Ok(d) => d,
            // The second halfword of a 32-bit instruction faulted (tval: its address).
            Err(e) => {
                return Block {
                    insns,
                    fetch_fault: Some(e),
                };
            }
        };
        a = a.wrapping_add(d.len as u64);
        let end = d.inst.ends_block();
        insns.push(d);
        // A block never continues onto another page (§13.2).
        if end || insns.len() >= max || a & !0xfff != pc & !0xfff {
            return Block {
                insns,
                fetch_fault: None,
            };
        }
    }
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
                LoadOp::Lb => mmu::load(cpu, mem, addr, 1).map(|v| v as u8 as i8 as u64),
                LoadOp::Lh => mmu::load(cpu, mem, addr, 2).map(|v| v as u16 as i16 as u64),
                LoadOp::Lw => mmu::load(cpu, mem, addr, 4).map(|v| v as u32 as i32 as u64),
                LoadOp::Ld => mmu::load(cpu, mem, addr, 8),
                LoadOp::Lbu => mmu::load(cpu, mem, addr, 1),
                LoadOp::Lhu => mmu::load(cpu, mem, addr, 2),
                LoadOp::Lwu => mmu::load(cpu, mem, addr, 4),
            };
            match r {
                Ok(v) => cpu.set_x(rd, v),
                Err(e) => return Flow::Trap(e),
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
            let v = cpu.x[rs2 as usize];
            if let Err(e) = mmu::store(cpu, mem, addr, size, v) {
                return Flow::Trap(e);
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
        Inst::SfenceVma { rs1, .. } => {
            let tvm = cpu.csr.mstatus & mstatus::TVM != 0;
            if cpu.prv == prv::U || (cpu.prv == prv::S && tvm) {
                return illegal;
            }
            // One page when rs1 names an address, else everything; ASIDs are ignored (D51).
            if rs1 != 0 {
                tlb::flush_page(cpu, cpu.x[rs1 as usize]);
            } else {
                tlb::flush_all(cpu);
            }
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
            let v = mmu::load(cpu, mem, addr, size)?;
            cpu.res_addr = addr;
            cpu.res_val = v;
            cpu.res_valid = 1;
            cpu.set_x(rd, sext(v));
        }
        AmoOp::Sc => {
            let ok = cpu.res_valid != 0 && cpu.res_addr == addr;
            cpu.res_valid = 0; // any SC clears the reservation
            if ok {
                let v = cpu.x[rs2 as usize];
                mmu::store(cpu, mem, addr, size, v)?;
            }
            cpu.set_x(rd, (!ok) as u64);
        }
        _ => {
            let old = mmu::load_for_amo(cpu, mem, addr, size)?;
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
            mmu::store(cpu, mem, addr, size, new)?;
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
