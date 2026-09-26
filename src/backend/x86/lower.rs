//! Naive lowering, 1:1 from decoded `Inst` to x86-64 (P2.6), with chainable exits, the
//! budget prologue and the inline jump cache (Phase 3). Phase 4 replaces the body lowering with
//! IR lowering over allocated registers.
//!
//! Every guest register lives in `CpuState` (`[rbp + 8*i - 128]`); each instruction loads its
//! operands into RAX/RCX/RDX, computes, and stores the result back, so guest state in memory
//! is precise at every instruction boundary. Guest memory is accessed directly as
//! `[rbx + addr]` (direct backend, §14.1): a bad access raises a host SIGSEGV that the
//! dispatcher maps back to the faulting guest instruction through the TB's `pcmap`.
//!
//! Instruction counting (D12, D30): the prologue `sub qword [budget], n; jl budget_stub`
//! charges all `n` instructions of the TB up front. Every path that leaves after retiring fewer
//! (ECALL, a trapping helper, a host fault) gives the rest back; the dispatcher derives
//! `icount` from `budget_ref - budget`. Instructions without a native lowering (CSR, AMO, FP,
//! privileged, illegal) call `helper_interp_one` (D14), which synchronizes `icount` first.
//!
//! Exits (§13.3): chainable direct exits are `jmp rel32` (slot 0) / `jcc rel32` (slot 1) with
//! 4-byte-aligned rel32 fields, initially targeting stubs at the TB tail that store `pc`, load
//! the exit code `(tb_id << 2) | slot` into EAX and jump to `exit_jit`. JALR looks its target
//! up in the jump cache (§13.4) and jumps straight to the TB on a hit.

use std::mem::offset_of;

use super::emit::{Alu, Asm, Cond, Label, Mem, Scale, Shift, Size, Unary};
use super::regs::{CPU, CPU_BIAS, MEM_BASE, Reg};
use crate::cpu::state::{CpuState, JC_SIZE, exit};
use crate::cpu::trap::Exception;
use crate::isa::inst::*;
use crate::jit::cache::PcEntry;
use crate::jit::trampoline::{SLOT_SPECIAL, Trampolines, cpu_field, helper};

/// Lowering options.
#[derive(Clone, Copy, Debug, Default)]
pub struct LowerOptions {
    /// Test-only: miscompile `addi` (adds `imm + 1`) so lockstep can prove it catches bugs.
    pub inject_bug: bool,
    /// `--profile-jit`: count JALR executions in `cpu.prof_jalr`.
    pub profile: bool,
    /// Softmmu (D48): memory accesses go through `helper_interp_one` (this back end has no
    /// inline TLB path; it exists for measurements).
    pub softmmu: bool,
}

/// A chainable exit of the lowered code (offsets from the TB start).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExitInfo {
    /// Offset of the 4-byte-aligned rel32 field.
    pub patch_off: u32,
    /// Offset of the exit stub the field initially targets.
    pub stub_off: u32,
    pub target_pc: u64,
}

/// Output of `translate`.
pub struct Lowered {
    pub code: Vec<u8>,
    pub pcmap: Vec<PcEntry>,
    /// Slot 0 (fall-through / jump) and slot 1 (branch taken).
    pub exits: [Option<ExitInfo>; 2],
}

use Reg::{R10, R11, Rax, Rcx, Rdi, Rdx, Rsi};

fn xreg(g: u8) -> Mem {
    Mem::base(CPU, 8 * g as i32 - CPU_BIAS)
}

fn field(off: usize) -> Mem {
    cpu_field(off)
}

/// Where an exit stub gets the guest pc it stores.
#[derive(Clone, Copy)]
enum PcSrc {
    Const(u64),
    /// RAX holds it (JALR jump-cache miss).
    Rax,
}

struct Stub {
    label: Label,
    pc: PcSrc,
    /// Instructions charged by the prologue but not retired: added back to the budget.
    refund: u32,
    reason: u32,
    exc: Option<Exception>,
    slot: u64,
}

struct Ctx<'a> {
    a: Asm,
    tr: &'a Trampolines,
    tb_id: u32,
    /// Instructions in the TB (charged by the prologue).
    n: u32,
    stubs: Vec<Stub>,
    helper_exit: Option<Label>,
    /// Chainable exits: (rel32 field offset, stub label, target pc).
    exits: [Option<(usize, Label, u64)>; 2],
    opts: LowerOptions,
}

fn budget() -> Mem {
    field(offset_of!(CpuState, budget))
}

impl Ctx<'_> {
    fn load_x(&mut self, r: Reg, g: u8) {
        if g == 0 {
            self.a.mov_r32_imm(r, 0);
        } else {
            self.a.load(Size::B64, r, xreg(g));
        }
    }

    fn load_x32(&mut self, r: Reg, g: u8) {
        if g == 0 {
            self.a.mov_r32_imm(r, 0);
        } else {
            self.a.load(Size::B32, r, xreg(g));
        }
    }

    fn store_x(&mut self, g: u8, r: Reg) {
        if g != 0 {
            self.a.store(Size::B64, xreg(g), r);
        }
    }

    /// `[mem] = v` (64-bit), using R11 when `v` does not fit a sign-extended imm32.
    fn store_const(&mut self, mem: Mem, v: u64) {
        if v as i64 == v as i32 as i64 {
            self.a.store_imm(Size::B64, mem, v as i32);
        } else {
            self.a.movabs(R11, v);
            self.a.store(Size::B64, mem, R11);
        }
    }

    fn set_x_const(&mut self, g: u8, v: u64) {
        if g != 0 {
            self.store_const(xreg(g), v);
        }
    }

    fn stub(&mut self, pc: PcSrc, refund: u32, reason: u32, slot: u64) -> Label {
        let label = self.a.new_label();
        self.stubs.push(Stub {
            label,
            pc,
            refund,
            reason,
            exc: None,
            slot,
        });
        label
    }

    /// Non-chainable exit through a stub (ECALL, FENCE.I).
    fn exit_special(&mut self, pc: u64, refund: u32, reason: u32) {
        let l = self.stub(PcSrc::Const(pc), refund, reason, SLOT_SPECIAL);
        self.a.jmp(l);
    }

    /// Chainable `jmp rel32` (slot 0) to guest `target`, rel32 field 4-byte aligned.
    fn exit_direct(&mut self, target: u64) {
        let l = self.stub(PcSrc::Const(target), 0, exit::NONE, 0);
        self.a.align(4, 1);
        let at = self.a.jmp(l);
        self.exits[0] = Some((at, l, target));
    }

    /// Chainable `jcc rel32` (slot 1) to guest `target`, rel32 field 4-byte aligned.
    fn exit_cond(&mut self, cond: Cond, target: u64) {
        let l = self.stub(PcSrc::Const(target), 0, exit::NONE, 1);
        self.a.align(4, 2);
        let at = self.a.jcc(cond, l);
        self.exits[1] = Some((at, l, target));
    }

    fn emit_stubs(&mut self) {
        for s in std::mem::take(&mut self.stubs) {
            self.a.bind(s.label);
            if s.refund > 0 {
                self.a
                    .alu_ri(Size::B64, Alu::Add, budget(), s.refund as i32);
            }
            match s.pc {
                PcSrc::Const(pc) => self.store_const(field(offset_of!(CpuState, pc)), pc),
                PcSrc::Rax => self
                    .a
                    .store(Size::B64, field(offset_of!(CpuState, pc)), Rax),
            }
            if s.reason != exit::NONE {
                let r = s.reason as i32;
                self.a
                    .store_imm(Size::B32, field(offset_of!(CpuState, exit_reason)), r);
            }
            if let Some(e) = s.exc {
                self.store_const(field(offset_of!(CpuState, exc_cause)), e.cause);
                self.store_const(field(offset_of!(CpuState, exc_tval)), e.tval);
            }
            self.a
                .mov_r32_imm(Rax, ((self.tb_id as u64) << 2 | s.slot) as u32);
            self.a.jmp_abs(self.tr.exit);
        }
        if let Some(l) = self.helper_exit.take() {
            // The helper already set pc and exit_reason and synchronized icount; the call
            // site already refunded the rest of the TB.
            self.a.bind(l);
            self.a
                .mov_r32_imm(Rax, ((self.tb_id as u64) << 2 | SLOT_SPECIAL) as u32);
            self.a.jmp_abs(self.tr.exit);
        }
    }

    /// Execute instruction `idx` (`d` at `pc`) through `helper_interp_one`. All guest state is
    /// already in `CpuState`; the prologue's charge for instructions `idx..n` is refunded
    /// around the call so the helper sees an exact instruction count (D30).
    fn call_interp(&mut self, d: &Decoded, pc: u64, idx: u32) {
        let rest = (self.n - idx) as i32;
        self.a.alu_ri(Size::B64, Alu::Add, budget(), rest);
        self.a.lea(Rdi, Mem::base(CPU, -CPU_BIAS));
        self.a.mov_r32_imm(Rsi, d.raw);
        self.a.mov_imm(Rdx, pc);
        self.a
            .call_indirect_abs(self.tr.helper_slot(helper::INTERP_ONE));
        self.a.test_rr(Size::B64, Rax, Rax);
        let l = match self.helper_exit {
            Some(l) => l,
            None => {
                let l = self.a.new_label();
                self.helper_exit = Some(l);
                l
            }
        };
        self.a.jcc(Cond::Ne, l);
        // Returned 0: the instruction retired without leaving the block; charge it and the
        // rest of the TB again.
        self.a.alu_ri(Size::B64, Alu::Sub, budget(), rest);
    }

    /// JALR tail (§13.4): RAX = target pc. Hit: jump straight to the TB. Miss: exit with
    /// `LOOKUP` and let the dispatcher translate and fill the entry.
    fn jump_cache(&mut self) {
        let jc = offset_of!(CpuState, jmp_cache) as i32 - CPU_BIAS;
        if self.opts.profile {
            let prof = field(offset_of!(CpuState, prof_jalr));
            self.a.alu_ri(Size::B64, Alu::Add, prof, 1);
        }
        // R10 = ((pc >> 1) & (JC_SIZE - 1)) * 16 = (pc << 3) & 0xFFF0
        self.a.mov_rr(Size::B64, R10, Rax);
        self.a.shift_ri(Size::B64, Shift::Shl, R10, 3);
        let mask = ((JC_SIZE - 1) << 4) as i32;
        self.a.alu_ri(Size::B32, Alu::And, R10, mask);
        self.a
            .alu_rm(Size::B64, Alu::Cmp, Rax, Mem::bi(CPU, R10, Scale::S1, jc));
        let miss = self.stub(PcSrc::Rax, 0, exit::LOOKUP, SLOT_SPECIAL);
        self.a.jcc(Cond::Ne, miss);
        self.a.jmp_rm(Mem::bi(CPU, R10, Scale::S1, jc + 8));
    }

    /// Lower instruction `idx`. Returns true if it ended the TB (an exit was emitted).
    fn insn(&mut self, d: &Decoded, pc: u64, idx: u32) -> bool {
        let next = pc.wrapping_add(d.len as u64);
        if self.opts.softmmu && matches!(d.inst, Inst::Load { .. } | Inst::Store { .. }) {
            self.call_interp(d, pc, idx);
            return false;
        }
        match d.inst {
            Inst::Lui { rd, imm } => self.set_x_const(rd, imm as u64),
            Inst::Auipc { rd, imm } => self.set_x_const(rd, pc.wrapping_add(imm as u64)),
            Inst::Jal { rd, imm } => {
                self.set_x_const(rd, next);
                self.exit_direct(pc.wrapping_add(imm as u64));
                return true;
            }
            Inst::Jalr { rd, rs1, imm } => {
                // Read rs1 before writing rd (rd == rs1 is legal).
                self.load_x(Rax, rs1);
                self.a.alu_ri(Size::B64, Alu::Add, Rax, imm as i32);
                self.a.alu_ri(Size::B64, Alu::And, Rax, -2);
                self.set_x_const(rd, next);
                self.jump_cache();
                return true;
            }
            Inst::Branch { op, rs1, rs2, imm } => {
                self.load_x(Rax, rs1);
                self.load_x(Rcx, rs2);
                self.a.alu_rr(Size::B64, Alu::Cmp, Rax, Rcx);
                let cond = match op {
                    BranchOp::Beq => Cond::E,
                    BranchOp::Bne => Cond::Ne,
                    BranchOp::Blt => Cond::L,
                    BranchOp::Bge => Cond::Ge,
                    BranchOp::Bltu => Cond::B,
                    BranchOp::Bgeu => Cond::Ae,
                };
                self.exit_cond(cond, pc.wrapping_add(imm as u64));
                self.exit_direct(next);
                return true;
            }
            Inst::Load { op, rd, rs1, imm } => {
                self.load_x(Rax, rs1);
                let m = Mem::bi(MEM_BASE, Rax, Scale::S1, imm as i32);
                // Loads into x0 still access memory (they may fault).
                match op {
                    LoadOp::Ld => self.a.load(Size::B64, Rcx, m),
                    LoadOp::Lw => self.a.movsxd(Rcx, m),
                    LoadOp::Lwu => self.a.load(Size::B32, Rcx, m),
                    LoadOp::Lh => self.a.movsx(Size::B64, Size::B16, Rcx, m),
                    LoadOp::Lhu => self.a.movzx(Size::B16, Rcx, m),
                    LoadOp::Lb => self.a.movsx(Size::B64, Size::B8, Rcx, m),
                    LoadOp::Lbu => self.a.movzx(Size::B8, Rcx, m),
                }
                self.store_x(rd, Rcx);
            }
            Inst::Store { op, rs1, rs2, imm } => {
                self.load_x(Rax, rs1);
                self.load_x(Rcx, rs2);
                let m = Mem::bi(MEM_BASE, Rax, Scale::S1, imm as i32);
                let size = match op {
                    StoreOp::Sb => Size::B8,
                    StoreOp::Sh => Size::B16,
                    StoreOp::Sw => Size::B32,
                    StoreOp::Sd => Size::B64,
                };
                self.a.store(size, m, Rcx);
            }
            Inst::OpImm { op, rd, rs1, imm } => {
                if rd != 0 {
                    self.op_imm(op, rs1, imm);
                    self.store_x(rd, Rax);
                }
            }
            Inst::OpImmW { op, rd, rs1, imm } => {
                if rd != 0 {
                    self.load_x32(Rax, rs1);
                    match op {
                        AluWOp::Addw => self.a.alu_ri(Size::B32, Alu::Add, Rax, imm as i32),
                        AluWOp::Sllw => self.a.shift_ri(Size::B32, Shift::Shl, Rax, imm as u8 & 31),
                        AluWOp::Srlw => self.a.shift_ri(Size::B32, Shift::Shr, Rax, imm as u8 & 31),
                        AluWOp::Sraw => self.a.shift_ri(Size::B32, Shift::Sar, Rax, imm as u8 & 31),
                        _ => unreachable!("no immediate form of {op:?}"),
                    }
                    self.a.movsxd(Rax, Rax);
                    self.store_x(rd, Rax);
                }
            }
            Inst::Op { op, rd, rs1, rs2 } => {
                // Every OP is side-effect free (division never traps), so rd = x0 is a no-op.
                if rd != 0 {
                    self.load_x(Rax, rs1);
                    self.load_x(Rcx, rs2);
                    self.op_rr(op);
                    self.store_x(rd, Rax);
                }
            }
            Inst::OpW { op, rd, rs1, rs2 } => {
                if rd != 0 {
                    self.load_x32(Rax, rs1);
                    self.load_x32(Rcx, rs2);
                    self.op_rr_w(op);
                    self.a.movsxd(Rax, Rax);
                    self.store_x(rd, Rax);
                }
            }
            // Single hart: x86 TSO already provides every ordering a FENCE can ask for (§18).
            Inst::Fence { .. } => {}
            Inst::FenceI => {
                // Last instruction of the TB, retired: nothing to refund.
                self.exit_special(next, 0, exit::FLUSH);
                return true;
            }
            Inst::Ecall => {
                // Not retired yet (the environment services it): refund its charge.
                self.exit_special(pc, 1, exit::ECALL);
                return true;
            }
            _ => self.call_interp(d, pc, idx),
        }
        false
    }

    /// RAX = x[rs1] `op` imm.
    fn op_imm(&mut self, op: AluOp, rs1: u8, imm: i64) {
        self.load_x(Rax, rs1);
        let i = imm as i32;
        match op {
            AluOp::Add => {
                let i = if self.opts.inject_bug {
                    i.wrapping_add(1)
                } else {
                    i
                };
                self.a.alu_ri(Size::B64, Alu::Add, Rax, i);
            }
            AluOp::Xor => self.a.alu_ri(Size::B64, Alu::Xor, Rax, i),
            AluOp::Or => self.a.alu_ri(Size::B64, Alu::Or, Rax, i),
            AluOp::And => self.a.alu_ri(Size::B64, Alu::And, Rax, i),
            AluOp::Slt | AluOp::Sltu => {
                // SLTIU compares against the sign-extended immediate as unsigned (§7.5).
                self.a.alu_ri(Size::B64, Alu::Cmp, Rax, i);
                let c = if op == AluOp::Slt { Cond::L } else { Cond::B };
                self.a.setcc(c, Rax);
                self.a.movzx(Size::B8, Rax, Rax);
            }
            AluOp::Sll => self.a.shift_ri(Size::B64, Shift::Shl, Rax, i as u8 & 63),
            AluOp::Srl => self.a.shift_ri(Size::B64, Shift::Shr, Rax, i as u8 & 63),
            AluOp::Sra => self.a.shift_ri(Size::B64, Shift::Sar, Rax, i as u8 & 63),
            _ => unreachable!("no immediate form of {op:?}"),
        }
    }

    /// RAX = RAX `op` RCX (64-bit). May clobber RDX and R11.
    fn op_rr(&mut self, op: AluOp) {
        let a = &mut self.a;
        match op {
            AluOp::Add => a.alu_rr(Size::B64, Alu::Add, Rax, Rcx),
            AluOp::Sub => a.alu_rr(Size::B64, Alu::Sub, Rax, Rcx),
            AluOp::Xor => a.alu_rr(Size::B64, Alu::Xor, Rax, Rcx),
            AluOp::Or => a.alu_rr(Size::B64, Alu::Or, Rax, Rcx),
            AluOp::And => a.alu_rr(Size::B64, Alu::And, Rax, Rcx),
            // x86 masks 64-bit shift counts to 6 bits, exactly like RV64 (§7.5).
            AluOp::Sll => a.shift_cl(Size::B64, Shift::Shl, Rax),
            AluOp::Srl => a.shift_cl(Size::B64, Shift::Shr, Rax),
            AluOp::Sra => a.shift_cl(Size::B64, Shift::Sar, Rax),
            AluOp::Slt | AluOp::Sltu => {
                a.alu_rr(Size::B64, Alu::Cmp, Rax, Rcx);
                a.setcc(if op == AluOp::Slt { Cond::L } else { Cond::B }, Rax);
                a.movzx(Size::B8, Rax, Rax);
            }
            AluOp::Mul => a.imul_rr(Size::B64, Rax, Rcx),
            AluOp::Mulh => {
                a.unary(Size::B64, Unary::Imul, Rcx);
                a.mov_rr(Size::B64, Rax, Rdx);
            }
            AluOp::Mulhu => {
                a.unary(Size::B64, Unary::Mul, Rcx);
                a.mov_rr(Size::B64, Rax, Rdx);
            }
            AluOp::Mulhsu => {
                // mulhsu(a, b) = mulhu(a, b) - (a < 0 ? b : 0)  (§7.5)
                a.mov_rr(Size::B64, R11, Rax);
                a.unary(Size::B64, Unary::Mul, Rcx);
                a.shift_ri(Size::B64, Shift::Sar, R11, 63);
                a.alu_rr(Size::B64, Alu::And, R11, Rcx);
                a.alu_rr(Size::B64, Alu::Sub, Rdx, R11);
                a.mov_rr(Size::B64, Rax, Rdx);
            }
            AluOp::Div | AluOp::Rem => self.div_signed(Size::B64, op == AluOp::Rem),
            AluOp::Divu | AluOp::Remu => self.div_unsigned(Size::B64, op == AluOp::Remu),
        }
    }

    /// EAX = EAX `op` ECX (32-bit; the caller sign-extends).
    fn op_rr_w(&mut self, op: AluWOp) {
        let a = &mut self.a;
        match op {
            AluWOp::Addw => a.alu_rr(Size::B32, Alu::Add, Rax, Rcx),
            AluWOp::Subw => a.alu_rr(Size::B32, Alu::Sub, Rax, Rcx),
            // 32-bit shifts mask the count to 5 bits, like the W forms.
            AluWOp::Sllw => a.shift_cl(Size::B32, Shift::Shl, Rax),
            AluWOp::Srlw => a.shift_cl(Size::B32, Shift::Shr, Rax),
            AluWOp::Sraw => a.shift_cl(Size::B32, Shift::Sar, Rax),
            AluWOp::Mulw => a.imul_rr(Size::B32, Rax, Rcx),
            AluWOp::Divw | AluWOp::Remw => self.div_signed(Size::B32, op == AluWOp::Remw),
            AluWOp::Divuw | AluWOp::Remuw => self.div_unsigned(Size::B32, op == AluWOp::Remuw),
        }
    }

    /// Signed RAX / RCX (or remainder) at `size`, with the RISC-V results for x/0 and
    /// MIN/-1 instead of x86's #DE (§7.5).
    fn div_signed(&mut self, size: Size, rem: bool) {
        let a = &mut self.a;
        let (zero, normal, done) = (a.new_label(), a.new_label(), a.new_label());
        a.test_rr(size, Rcx, Rcx);
        a.jcc_short(Cond::E, zero);
        a.alu_ri(size, Alu::Cmp, Rcx, -1);
        a.jcc_short(Cond::Ne, normal);
        // Divisor -1: quotient = -a (MIN stays MIN), remainder = 0.
        if rem {
            a.mov_r32_imm(Rax, 0);
        } else {
            a.unary(size, Unary::Neg, Rax);
        }
        a.jmp_short(done);
        a.bind(normal);
        if size == Size::B64 {
            a.cqo();
        } else {
            a.cdq();
        }
        a.unary(size, Unary::Idiv, Rcx);
        if rem {
            a.mov_rr(Size::B64, Rax, Rdx);
        }
        a.jmp_short(done);
        a.bind(zero);
        // x/0: quotient = -1, remainder = x (already in RAX).
        if !rem {
            a.mov_imm(Rax, u64::MAX);
        }
        a.bind(done);
    }

    /// Unsigned RAX / RCX (or remainder) at `size`; x/0 gives all ones, x%0 gives x.
    fn div_unsigned(&mut self, size: Size, rem: bool) {
        let a = &mut self.a;
        let (zero, done) = (a.new_label(), a.new_label());
        a.test_rr(size, Rcx, Rcx);
        a.jcc_short(Cond::E, zero);
        a.mov_r32_imm(Rdx, 0);
        a.unary(size, Unary::Div, Rcx);
        if rem {
            a.mov_rr(Size::B64, Rax, Rdx);
        }
        a.jmp_short(done);
        a.bind(zero);
        if !rem {
            a.mov_imm(Rax, u64::MAX);
        }
        a.bind(done);
    }
}

/// Translate one block. `origin` is the RX address the code will be placed at.
pub fn translate(
    insns: &[Decoded],
    fetch_fault: Option<Exception>,
    pc: u64,
    origin: u64,
    tb_id: u32,
    tr: &Trampolines,
    opts: LowerOptions,
) -> Lowered {
    let n = insns.len() as u32;
    let mut c = Ctx {
        a: Asm::new(origin),
        tr,
        tb_id,
        n,
        stubs: Vec::new(),
        helper_exit: None,
        exits: [None, None],
        opts,
    };
    // Prologue (D12): charge the whole TB; if the slice is used up, exit before executing
    // anything (the stub refunds the charge). An empty TB (immediate fetch fault) has none.
    if n > 0 {
        c.a.alu_ri(Size::B64, Alu::Sub, budget(), n as i32);
        let l = c.stub(PcSrc::Const(pc), n, exit::BUDGET, SLOT_SPECIAL);
        c.a.jcc(Cond::L, l);
    }
    let mut pcmap = Vec::with_capacity(insns.len());
    let mut pc = pc;
    let mut ended = false;
    for (idx, d) in insns.iter().enumerate() {
        pcmap.push(PcEntry {
            host_off: c.a.pos() as u32,
            idx: idx as u32,
            guest_pc: pc,
        });
        ended = c.insn(d, pc, idx as u32);
        pc = pc.wrapping_add(d.len as u64);
        if ended {
            break;
        }
    }
    if !ended {
        // Fell off the end: page boundary, block limit, a block-ending instruction executed by
        // the helper (CSR, MRET, …) that returned "continue", or a fetch fault. Everything in
        // the TB retired, so nothing is refunded.
        match fetch_fault {
            Some(e) => {
                let label = c.a.new_label();
                c.stubs.push(Stub {
                    label,
                    pc: PcSrc::Const(pc),
                    refund: 0,
                    reason: exit::EXCEPTION,
                    exc: Some(e),
                    slot: SLOT_SPECIAL,
                });
                c.a.jmp(label);
            }
            None => c.exit_direct(pc),
        }
    }
    c.emit_stubs();
    let exits = c.exits.map(|e| {
        e.map(|(at, label, target_pc)| ExitInfo {
            patch_off: at as u32,
            stub_off: c.a.label_offset(label).expect("stub bound") as u32,
            target_pc,
        })
    });
    Lowered {
        code: c.a.finish(),
        pcmap,
        exits,
    }
}
