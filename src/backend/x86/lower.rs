//! Naive lowering, 1:1 from decoded `Inst` to x86-64 (P2.6). Phase 4 replaces this with IR
//! lowering over allocated registers.
//!
//! Every guest register lives in `CpuState` (`[rbp + 8*i - 128]`); each instruction loads its
//! operands into RAX/RCX/RDX, computes, and stores the result back, so guest state in memory
//! is precise at every instruction boundary. Guest memory is accessed directly as
//! `[rbx + addr]` (direct backend, §14.1): a bad access raises a host SIGSEGV that the
//! dispatcher maps back to the faulting guest instruction through the TB's `pcmap`.
//!
//! Instructions without a native lowering (CSR, AMO, FP, privileged, illegal) call
//! `helper_interp_one` (D14). `cpu.icount` is only updated at exits and before helper calls:
//! `pending` counts retired instructions not yet added.
//!
//! Exits (§13.3) go through stubs at the TB tail: `jmp`/`jcc rel32` → stub, which stores
//! `pc`, adds the pending count to `icount`, sets `exit_reason`, loads the exit code
//! `(tb_id << 2) | slot` into EAX and jumps to `exit_jit`.

use std::mem::offset_of;

use super::emit::{Alu, Asm, Cond, Label, Mem, Scale, Shift, Size, Unary};
use super::regs::{CPU, CPU_BIAS, MEM_BASE, Reg};
use crate::cpu::state::{CpuState, exit};
use crate::cpu::trap::Exception;
use crate::isa::inst::*;
use crate::jit::cache::PcEntry;
use crate::jit::trampoline::{SLOT_SPECIAL, Trampolines, cpu_field, helper};

/// Lowering options.
#[derive(Clone, Copy, Debug, Default)]
pub struct LowerOptions {
    /// Test-only: miscompile `addi` (adds `imm + 1`) so lockstep can prove it catches bugs.
    pub inject_bug: bool,
}

/// Output of `translate`.
pub struct Lowered {
    pub code: Vec<u8>,
    pub pcmap: Vec<PcEntry>,
}

use Reg::{R11, Rax, Rcx, Rdi, Rdx, Rsi};

fn xreg(g: u8) -> Mem {
    Mem::base(CPU, 8 * g as i32 - CPU_BIAS)
}

fn field(off: usize) -> Mem {
    cpu_field(off)
}

struct Stub {
    label: Label,
    /// Guest pc to store (None: already stored by the TB body, e.g. JALR).
    pc: Option<u64>,
    icount: u32,
    reason: u32,
    exc: Option<Exception>,
    slot: u64,
}

struct Ctx<'a> {
    a: Asm,
    tr: &'a Trampolines,
    tb_id: u32,
    pending: u32,
    stubs: Vec<Stub>,
    helper_exit: Option<Label>,
    opts: LowerOptions,
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

    /// Add the pending instruction count to `cpu.icount` (before a helper observes it).
    fn flush_icount(&mut self) {
        if self.pending > 0 {
            let n = self.pending as i32;
            self.a
                .alu_ri(Size::B64, Alu::Add, field(offset_of!(CpuState, icount)), n);
            self.pending = 0;
        }
    }

    fn stub(&mut self, pc: Option<u64>, icount: u32, reason: u32, slot: u64) -> Label {
        let label = self.a.new_label();
        self.stubs.push(Stub {
            label,
            pc,
            icount,
            reason,
            exc: None,
            slot,
        });
        label
    }

    /// Unconditional exit to `pc`, counting `icount` more retired instructions.
    fn exit_to(&mut self, pc: Option<u64>, icount: u32, reason: u32, slot: u64) {
        let l = self.stub(pc, icount, reason, slot);
        self.a.jmp(l);
    }

    fn emit_stubs(&mut self) {
        for s in std::mem::take(&mut self.stubs) {
            self.a.bind(s.label);
            if s.icount > 0 {
                let n = s.icount as i32;
                self.a
                    .alu_ri(Size::B64, Alu::Add, field(offset_of!(CpuState, icount)), n);
            }
            if let Some(pc) = s.pc {
                self.store_const(field(offset_of!(CpuState, pc)), pc);
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
            // The helper already set pc, exit_reason and icount.
            self.a.bind(l);
            self.a
                .mov_r32_imm(Rax, ((self.tb_id as u64) << 2 | SLOT_SPECIAL) as u32);
            self.a.jmp_abs(self.tr.exit);
        }
    }

    /// Execute `d` at `pc` through `helper_interp_one` (full state sync is implicit: all
    /// guest state is already in `CpuState`).
    fn call_interp(&mut self, d: &Decoded, pc: u64) {
        self.flush_icount();
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
        // Returned 0: the instruction retired without leaving the block; `insn` counts it.
    }

    /// Lower one instruction. Returns true if it ended the TB (an exit was emitted).
    fn insn(&mut self, d: &Decoded, pc: u64) -> bool {
        let next = pc.wrapping_add(d.len as u64);
        match d.inst {
            Inst::Lui { rd, imm } => self.set_x_const(rd, imm as u64),
            Inst::Auipc { rd, imm } => self.set_x_const(rd, pc.wrapping_add(imm as u64)),
            Inst::Jal { rd, imm } => {
                self.set_x_const(rd, next);
                let n = self.pending + 1;
                self.exit_to(Some(pc.wrapping_add(imm as u64)), n, exit::NONE, 0);
                return true;
            }
            Inst::Jalr { rd, rs1, imm } => {
                // Read rs1 before writing rd (rd == rs1 is legal).
                self.load_x(Rax, rs1);
                self.a.alu_ri(Size::B64, Alu::Add, Rax, imm as i32);
                self.a.alu_ri(Size::B64, Alu::And, Rax, -2);
                self.a
                    .store(Size::B64, field(offset_of!(CpuState, pc)), Rax);
                self.set_x_const(rd, next);
                let n = self.pending + 1;
                self.exit_to(None, n, exit::NONE, SLOT_SPECIAL);
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
                let n = self.pending + 1;
                let taken = self.stub(Some(pc.wrapping_add(imm as u64)), n, exit::NONE, 1);
                self.a.jcc(cond, taken);
                self.exit_to(Some(next), n, exit::NONE, 0);
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
                let n = self.pending + 1;
                self.exit_to(Some(next), n, exit::FLUSH, SLOT_SPECIAL);
                return true;
            }
            Inst::Ecall => {
                let n = self.pending;
                self.exit_to(Some(pc), n, exit::ECALL, SLOT_SPECIAL);
                return true;
            }
            _ => self.call_interp(d, pc),
        }
        self.pending += 1;
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
    let mut c = Ctx {
        a: Asm::new(origin),
        tr,
        tb_id,
        pending: 0,
        stubs: Vec::new(),
        helper_exit: None,
        opts,
    };
    let mut pcmap = Vec::with_capacity(insns.len());
    let mut pc = pc;
    let mut ended = false;
    for (idx, d) in insns.iter().enumerate() {
        pcmap.push(PcEntry {
            host_off: c.a.pos() as u32,
            idx: idx as u32,
            guest_pc: pc,
            pending: c.pending,
        });
        ended = c.insn(d, pc);
        pc = pc.wrapping_add(d.len as u64);
        if ended {
            break;
        }
    }
    if !ended {
        // Fell off the end: page boundary, block limit, a block-ending instruction executed by
        // the helper (CSR, MRET, …) that returned "continue", or a fetch fault.
        let n = c.pending;
        match fetch_fault {
            Some(e) => {
                let label = c.a.new_label();
                c.stubs.push(Stub {
                    label,
                    pc: Some(pc),
                    icount: n,
                    reason: exit::EXCEPTION,
                    exc: Some(e),
                    slot: SLOT_SPECIAL,
                });
                c.a.jmp(label);
            }
            None => c.exit_to(Some(pc), n, exit::NONE, 0),
        }
    }
    c.emit_stubs();
    Lowered {
        code: c.a.finish(),
        pcmap,
    }
}
