//! IR → x86-64 lowering with register allocation (CLAUDE.md §9–11, §15, P4.7).
//!
//! Walks an optimized IR block in order, asking `regalloc::linear_scan::Alloc` for registers.
//! Instruction selection: `lea` for adds, three-operand `imul`, BMI2 `shlx/shrx/sarx` when
//! available, `setcc` for comparisons, immediate forms for constants. Fixed-register ops
//! (multiply-high, division, shifts without BMI2) copy their operands to the R10/R11 scratch
//! registers, vacate RAX/RDX (or RCX), and bind the result there.
//!
//! Block structure is Phase 3's: budget prologue, chainable `jmp`/`jcc rel32` exits with
//! aligned rel32 fields, stubs at the tail, jump cache for JALR (D30, D31). Before every exit
//! the dirty guest registers are written back; pinned registers stay in R12–R15 (§8.3).
//!
//! Every load and store is a potential fault site: it records its host offset, its guest
//! instruction, the address register and the **state map** (where each dirty guest register
//! is). The SIGSEGV path uses it to rebuild precise guest state (§15). Helper calls
//! (`Interp`) do a full sync: write back, store pinned registers home, reload them after.

use std::mem::offset_of;

use super::emit::{Alu, Asm, Cond, Label, Mem, Scale, Shift, ShiftX, Size, Unary};
use super::regs::{CPU, CPU_BIAS, MEM_BASE, Reg};
use crate::cpu::state::{CpuState, JC_SIZE, exit};
use crate::cpu::trap::Exception;
use crate::ir::liveness::{Constraint, constraint};
use crate::ir::ops::{BinOp, Block, Cond as IrCond, ExitKind, Op, V};
use crate::jit::cache::PcEntry;
use crate::jit::trampoline::{SLOT_SPECIAL, Trampolines, cpu_field, helper};
use crate::regalloc::linear_scan::{Alloc, AllocStats, DLoc, OutOfSlots, Result, store_const};

use super::lower::ExitInfo;
use Reg::{R10, R11, Rax, Rcx, Rdi, Rdx, Rsi};

/// Options of the IR back end.
#[derive(Clone, Copy, Debug)]
pub struct IrOptions {
    /// Lazy write-back of non-pinned guest registers (`--regalloc=linear`).
    pub lazy: bool,
    pub bmi2: bool,
    /// `--profile-jit`
    pub profile: bool,
    /// Guest register → pinned host register.
    pub pinned: [Option<Reg>; 32],
    /// Test-only miscompilation of `addi` (as in the naive back end).
    pub inject_bug: bool,
}

/// A potentially faulting memory access and the state needed to make its fault precise.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FaultSite {
    /// Offset of the faulting host instruction from the TB start.
    pub rip_off: u32,
    /// Guest instruction index and pc.
    pub idx: u32,
    pub pc: u64,
    /// Guest address = `addr` register + `off`.
    pub addr: Reg,
    pub off: i32,
    pub size: u8,
    pub store: bool,
    /// Where each dirty (not yet stored) guest register is.
    pub dirty: Vec<(u8, DLoc)>,
}

pub struct LoweredIr {
    pub code: Vec<u8>,
    pub pcmap: Vec<PcEntry>,
    pub exits: [Option<ExitInfo>; 2],
    pub fault_sites: Vec<FaultSite>,
    pub stats: AllocStats,
}

#[derive(Clone, Copy)]
enum PcSrc {
    Const(u64),
    Rax,
}

struct Stub {
    label: Label,
    pc: PcSrc,
    refund: u32,
    reason: u32,
    exc: Option<Exception>,
    slot: u64,
}

fn budget() -> Mem {
    cpu_field(offset_of!(CpuState, budget))
}

fn field(off: usize) -> Mem {
    cpu_field(off)
}

struct Ctx<'a> {
    a: Asm,
    ra: Alloc,
    tr: &'a Trampolines,
    tb_id: u32,
    n: u32,
    opts: IrOptions,
    stubs: Vec<Stub>,
    helper_exit: Option<Label>,
    exits: [Option<(usize, Label, u64)>; 2],
    pcmap: Vec<PcEntry>,
    sites: Vec<FaultSite>,
    pc: u64,
    idx: u32,
}

fn is_pool(r: Reg) -> bool {
    super::regs::POOL.contains(&r)
}

impl Ctx<'_> {
    // ------------------------------------------------------------ exits ----

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

    fn exit_special(&mut self, pc: u64, refund: u32, reason: u32, exc: Option<Exception>) {
        let label = self.a.new_label();
        self.stubs.push(Stub {
            label,
            pc: PcSrc::Const(pc),
            refund,
            reason,
            exc,
            slot: SLOT_SPECIAL,
        });
        self.a.jmp(label);
    }

    fn exit_direct(&mut self, target: u64) {
        let l = self.stub(PcSrc::Const(target), 0, exit::NONE, 0);
        self.a.align(4, 1);
        let at = self.a.jmp(l);
        self.exits[0] = Some((at, l, target));
    }

    fn exit_cond(&mut self, cond: Cond, target: u64) {
        let l = self.stub(PcSrc::Const(target), 0, exit::NONE, 1);
        self.a.align(4, 2);
        let at = self.a.jcc(cond, l);
        self.exits[1] = Some((at, l, target));
    }

    fn reload_pinned(&mut self) {
        for g in 1..32u8 {
            if let Some(p) = self.opts.pinned[g as usize] {
                self.a
                    .load(Size::B64, p, Mem::base(CPU, 8 * g as i32 - CPU_BIAS));
            }
        }
    }

    fn emit_stubs(&mut self) {
        for s in std::mem::take(&mut self.stubs) {
            self.a.bind(s.label);
            if s.refund > 0 {
                self.a
                    .alu_ri(Size::B64, Alu::Add, budget(), s.refund as i32);
            }
            match s.pc {
                PcSrc::Const(pc) => store_const(&mut self.a, field(offset_of!(CpuState, pc)), pc),
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
                store_const(&mut self.a, field(offset_of!(CpuState, exc_cause)), e.cause);
                store_const(&mut self.a, field(offset_of!(CpuState, exc_tval)), e.tval);
            }
            self.a
                .mov_r32_imm(Rax, ((self.tb_id as u64) << 2 | s.slot) as u32);
            self.a.jmp_abs(self.tr.exit);
        }
        if let Some(l) = self.helper_exit.take() {
            // The helper stored pc/exit_reason and may have changed pinned guest registers in
            // CpuState: reload them so exit_jit stores the right values back.
            self.a.bind(l);
            self.reload_pinned();
            self.a
                .mov_r32_imm(Rax, ((self.tb_id as u64) << 2 | SLOT_SPECIAL) as u32);
            self.a.jmp_abs(self.tr.exit);
        }
    }

    fn jump_cache(&mut self) {
        let jc = offset_of!(CpuState, jmp_cache) as i32 - CPU_BIAS;
        if self.opts.profile {
            let prof = field(offset_of!(CpuState, prof_jalr));
            self.a.alu_ri(Size::B64, Alu::Add, prof, 1);
        }
        self.a.mov_rr(Size::B64, R10, Rax);
        self.a.shift_ri(Size::B64, Shift::Shl, R10, 3);
        self.a
            .alu_ri(Size::B32, Alu::And, R10, ((JC_SIZE - 1) << 4) as i32);
        self.a
            .alu_rm(Size::B64, Alu::Cmp, Rax, Mem::bi(CPU, R10, Scale::S1, jc));
        let miss = self.stub(PcSrc::Rax, 0, exit::LOOKUP, SLOT_SPECIAL);
        self.a.jcc(Cond::Ne, miss);
        self.a.jmp_rm(Mem::bi(CPU, R10, Scale::S1, jc + 8));
    }

    // ------------------------------------------------------------ ops ----

    fn record_site(&mut self, addr: Reg, off: i32, size: u8, store: bool) {
        self.sites.push(FaultSite {
            rip_off: self.a.pos() as u32,
            idx: self.idx,
            pc: self.pc,
            addr,
            off,
            size,
            store,
            dirty: self.ra.dirty_state(),
        });
    }

    fn bin(&mut self, op: BinOp, dst: V, x: V, y: V) -> Result<()> {
        match constraint(
            &Op::Bin {
                op,
                dst,
                a: x,
                b: y,
            },
            self.opts.bmi2,
        ) {
            Constraint::RaxRdx => return self.muldiv(op, dst, x, y),
            Constraint::Rcx => return self.shift_cl(op, dst, x, y),
            _ => {}
        }
        if matches!(op, BinOp::Sub | BinOp::Subw) && self.ra.const_of(x) == Some(0) {
            // neg / negw (`sub rd, x0, rs`).
            let rb = self.ra.get(&mut self.a, y, &[])?;
            self.ra.release(&[y]);
            let prefer = if is_pool(rb) { Some(rb) } else { None };
            let d = self.ra.def(&mut self.a, dst, prefer, &[rb])?;
            if d != rb {
                self.a.mov_rr(Size::B64, d, rb);
            }
            if op == BinOp::Sub {
                self.a.unary(Size::B64, Unary::Neg, d);
            } else {
                self.a.unary(Size::B32, Unary::Neg, d);
                self.a.movsxd(d, d);
            }
            self.ra.release(&[dst]);
            return Ok(());
        }
        let ra = self.ra.get(&mut self.a, x, &[])?;
        let rb = self.ra.get(&mut self.a, y, &[ra])?;
        self.ra.release(&[x, y]);
        let prefer = if is_pool(ra) { Some(ra) } else { None };
        let d = self.ra.def(&mut self.a, dst, prefer, &[ra, rb])?;
        self.emit_bin(op, d, ra, rb);
        self.ra.release(&[dst]);
        Ok(())
    }

    /// `d = a op b` for ops without fixed registers; `d` may alias `a` or `b`.
    fn emit_bin(&mut self, op: BinOp, d: Reg, ra: Reg, rb: Reg) {
        use BinOp::*;
        let a = &mut self.a;
        let s64 = Size::B64;
        match op {
            Add => {
                if d == ra {
                    a.alu_rr(s64, Alu::Add, d, rb);
                } else if d == rb {
                    a.alu_rr(s64, Alu::Add, d, ra);
                } else {
                    a.lea(d, Mem::bi(ra, rb, Scale::S1, 0));
                }
            }
            Sub => {
                if d == ra {
                    a.alu_rr(s64, Alu::Sub, d, rb);
                } else if d == rb {
                    a.unary(s64, Unary::Neg, d);
                    a.alu_rr(s64, Alu::Add, d, ra);
                } else {
                    a.mov_rr(s64, d, ra);
                    a.alu_rr(s64, Alu::Sub, d, rb);
                }
            }
            And | Or | Xor | Mul => {
                let (x, y) = if d == rb { (rb, ra) } else { (ra, rb) };
                if d != x {
                    a.mov_rr(s64, d, x);
                }
                match op {
                    And => a.alu_rr(s64, Alu::And, d, y),
                    Or => a.alu_rr(s64, Alu::Or, d, y),
                    Xor => a.alu_rr(s64, Alu::Xor, d, y),
                    _ => a.imul_rr(s64, d, y),
                }
            }
            Slt | Sltu => {
                a.alu_rr(s64, Alu::Cmp, ra, rb);
                a.setcc(if op == Slt { Cond::L } else { Cond::B }, d);
                a.movzx(Size::B8, d, d);
            }
            Sll | Srl | Sra | Sllw | Srlw | Sraw => {
                let (size, x) = match op {
                    Sll => (s64, ShiftX::Shlx),
                    Srl => (s64, ShiftX::Shrx),
                    Sra => (s64, ShiftX::Sarx),
                    Sllw => (Size::B32, ShiftX::Shlx),
                    Srlw => (Size::B32, ShiftX::Shrx),
                    _ => (Size::B32, ShiftX::Sarx),
                };
                a.shiftx(size, x, d, ra, rb);
                if size == Size::B32 {
                    a.movsxd(d, d);
                }
            }
            Addw | Mulw | Subw => {
                let (x, mut y) = if d == rb && op != Subw {
                    (rb, ra)
                } else {
                    (ra, rb)
                };
                if d == y {
                    a.mov_rr(s64, R11, y);
                    y = R11;
                }
                if d != x {
                    a.mov_rr(s64, d, x);
                }
                match op {
                    Addw => a.alu_rr(Size::B32, Alu::Add, d, y),
                    Subw => a.alu_rr(Size::B32, Alu::Sub, d, y),
                    _ => a.imul_rr(Size::B32, d, y),
                }
                a.movsxd(d, d);
            }
            _ => unreachable!("{op:?} needs fixed registers"),
        }
    }

    /// Multiply-high and division: operands to R10 (dividend) / R11 (divisor), RDX:RAX vacated.
    fn muldiv(&mut self, op: BinOp, dst: V, x: V, y: V) -> Result<()> {
        use BinOp::*;
        // Free RDX:RAX first, then load the operands elsewhere: the allocator uses R11 for its
        // own copies (write-back protection), so nothing may be parked in R10/R11 across an
        // allocator call.
        self.ra.vacate(&mut self.a, Rax, &[Rax, Rdx])?;
        self.ra.vacate(&mut self.a, Rdx, &[Rax, Rdx])?;
        let ra = self.ra.get(&mut self.a, x, &[Rax, Rdx])?;
        let rb = self.ra.get(&mut self.a, y, &[Rax, Rdx, ra])?;
        self.a.mov_rr(Size::B64, R10, ra);
        self.a.mov_rr(Size::B64, R11, rb);
        self.ra.release(&[x, y]);
        let a = &mut self.a;
        a.mov_rr(Size::B64, Rax, R10);
        let res = match op {
            Mulh => {
                a.unary(Size::B64, Unary::Imul, R11);
                Rdx
            }
            Mulhu => {
                a.unary(Size::B64, Unary::Mul, R11);
                Rdx
            }
            Mulhsu => {
                // mulhsu(a, b) = mulhu(a, b) - (a < 0 ? b : 0)  (§7.5)
                a.unary(Size::B64, Unary::Mul, R11);
                a.shift_ri(Size::B64, Shift::Sar, R10, 63);
                a.alu_rr(Size::B64, Alu::And, R10, R11);
                a.alu_rr(Size::B64, Alu::Sub, Rdx, R10);
                Rdx
            }
            Div | Rem => div_signed(a, Size::B64, op == Rem),
            Divu | Remu => div_unsigned(a, Size::B64, op == Remu),
            Divw | Remw => div_signed(a, Size::B32, op == Remw),
            Divuw | Remuw => div_unsigned(a, Size::B32, op == Remuw),
            _ => unreachable!(),
        };
        if matches!(op, Divw | Remw | Divuw | Remuw) {
            a.movsxd(res, res);
        }
        self.ra.def_fixed(dst, res);
        self.ra.release(&[dst]);
        Ok(())
    }

    /// Variable shifts without BMI2: the count must be in CL.
    fn shift_cl(&mut self, op: BinOp, dst: V, x: V, y: V) -> Result<()> {
        use BinOp::*;
        // As in `muldiv`: free RCX before loading the operands, never hold R10/R11 across an
        // allocator call.
        self.ra.vacate(&mut self.a, Rcx, &[Rcx])?;
        let ra = self.ra.get(&mut self.a, x, &[Rcx])?;
        let rb = self.ra.get(&mut self.a, y, &[Rcx, ra])?;
        self.a.mov_rr(Size::B64, R10, ra);
        self.a.mov_rr(Size::B64, Rcx, rb);
        self.ra.release(&[x, y]);
        let (size, sh) = match op {
            Sll => (Size::B64, Shift::Shl),
            Srl => (Size::B64, Shift::Shr),
            Sra => (Size::B64, Shift::Sar),
            Sllw => (Size::B32, Shift::Shl),
            Srlw => (Size::B32, Shift::Shr),
            _ => (Size::B32, Shift::Sar),
        };
        self.a.shift_cl(size, sh, R10);
        if size == Size::B32 {
            self.a.movsxd(R10, R10);
        }
        let d = self.ra.def(&mut self.a, dst, Some(Rcx), &[])?;
        self.a.mov_rr(Size::B64, d, R10);
        self.ra.release(&[dst]);
        Ok(())
    }

    fn bin_imm(&mut self, op: BinOp, dst: V, x: V, imm: i64) -> Result<()> {
        use BinOp::*;
        let ra = self.ra.get(&mut self.a, x, &[])?;
        self.ra.release(&[x]);
        let prefer = if is_pool(ra) { Some(ra) } else { None };
        let d = self.ra.def(&mut self.a, dst, prefer, &[ra])?;
        let i = imm as i32;
        debug_assert_eq!(i as i64, imm, "BinImm immediate must fit i32");
        let s64 = Size::B64;
        let mv = |a: &mut Asm| {
            if d != ra {
                a.mov_rr(s64, d, ra);
            }
        };
        let i = if op == Add && self.opts.inject_bug && i != 0 {
            i.wrapping_add(1)
        } else {
            i
        };
        let a = &mut self.a;
        match op {
            Add => {
                if d == ra {
                    a.alu_ri(s64, Alu::Add, d, i);
                } else {
                    a.lea(d, Mem::base(ra, i));
                }
            }
            And | Or | Xor => {
                mv(a);
                let o = match op {
                    And => Alu::And,
                    Or => Alu::Or,
                    _ => Alu::Xor,
                };
                a.alu_ri(s64, o, d, i);
            }
            Slt | Sltu => {
                // SLTIU compares against the sign-extended immediate as unsigned (§7.5).
                a.alu_ri(s64, Alu::Cmp, ra, i);
                a.setcc(if op == Slt { Cond::L } else { Cond::B }, d);
                a.movzx(Size::B8, d, d);
            }
            Sll | Srl | Sra => {
                mv(a);
                let sh = match op {
                    Sll => Shift::Shl,
                    Srl => Shift::Shr,
                    _ => Shift::Sar,
                };
                a.shift_ri(s64, sh, d, i as u8 & 63);
            }
            Mul => a.imul_rri(s64, d, ra, i),
            Addw => {
                mv(a);
                a.alu_ri(Size::B32, Alu::Add, d, i);
                a.movsxd(d, d);
            }
            Sllw | Srlw | Sraw => {
                mv(a);
                let sh = match op {
                    Sllw => Shift::Shl,
                    Srlw => Shift::Shr,
                    _ => Shift::Sar,
                };
                a.shift_ri(Size::B32, sh, d, i as u8 & 31);
                a.movsxd(d, d);
            }
            Mulw => {
                a.imul_rri(Size::B32, d, ra, i);
                a.movsxd(d, d);
            }
            _ => {
                a.mov_imm(R11, imm as u64);
                self.emit_bin(op, d, ra, R11);
            }
        }
        self.ra.release(&[dst]);
        Ok(())
    }

    fn load(&mut self, dst: V, addr: V, off: i32, size: u8, signed: bool) -> Result<()> {
        let ra = self.ra.get(&mut self.a, addr, &[])?;
        self.ra.release(&[addr]);
        let prefer = if is_pool(ra) { Some(ra) } else { None };
        let d = self.ra.def(&mut self.a, dst, prefer, &[ra])?;
        self.record_site(ra, off, size, false);
        let m = Mem::bi(MEM_BASE, ra, Scale::S1, off);
        let a = &mut self.a;
        match (size, signed) {
            (8, _) => a.load(Size::B64, d, m),
            (4, true) => a.movsxd(d, m),
            (4, false) => a.load(Size::B32, d, m),
            (2, true) => a.movsx(Size::B64, Size::B16, d, m),
            (2, false) => a.movzx(Size::B16, d, m),
            (1, true) => a.movsx(Size::B64, Size::B8, d, m),
            _ => a.movzx(Size::B8, d, m),
        }
        self.ra.release(&[dst]);
        Ok(())
    }

    fn store(&mut self, addr: V, off: i32, val: V, size: u8) -> Result<()> {
        let ra = self.ra.get(&mut self.a, addr, &[])?;
        let m = Mem::bi(MEM_BASE, ra, Scale::S1, off);
        let sz = match size {
            1 => Size::B8,
            2 => Size::B16,
            4 => Size::B32,
            _ => Size::B64,
        };
        match self.ra.const_of(val) {
            Some(c) if size < 8 || c as i64 == c as i32 as i64 => {
                let c = match size {
                    1 => c as u8 as i32,
                    2 => c as u16 as i32,
                    _ => c as i32,
                };
                self.record_site(ra, off, size, true);
                self.a.store_imm(sz, m, c);
            }
            _ => {
                let rv = self.ra.get(&mut self.a, val, &[ra])?;
                self.record_site(ra, off, size, true);
                self.a.store(sz, m, rv);
            }
        }
        self.ra.release(&[addr, val]);
        Ok(())
    }

    fn interp(&mut self, raw: u32, pc: u64, idx: u32) -> Result<()> {
        self.ra.sync_for_call(&mut self.a)?;
        let rest = (self.n - idx) as i32;
        let a = &mut self.a;
        a.alu_ri(Size::B64, Alu::Add, budget(), rest);
        a.lea(Rdi, Mem::base(CPU, -CPU_BIAS));
        a.mov_r32_imm(Rsi, raw);
        a.mov_imm(Rdx, pc);
        a.call_indirect_abs(self.tr.helper_slot(helper::INTERP_ONE));
        a.test_rr(Size::B64, Rax, Rax);
        let l = match self.helper_exit {
            Some(l) => l,
            None => {
                let l = self.a.new_label();
                self.helper_exit = Some(l);
                l
            }
        };
        self.a.jcc(Cond::Ne, l);
        self.a.alu_ri(Size::B64, Alu::Sub, budget(), rest);
        self.ra.after_call(&mut self.a);
        Ok(())
    }

    fn terminator(&mut self, op: &Op) -> Result<()> {
        match *op {
            Op::Branch {
                cond,
                a: x,
                b: y,
                taken,
                fall,
            } => {
                // A constant operand becomes an immediate (`test r, r` for 0, e.g. BNEZ):
                // put it second, mirroring the condition if it was first.
                let imm = |v: V| {
                    self.ra
                        .const_of(v)
                        .filter(|&c| c as i64 == c as i32 as i64)
                        .map(|c| c as i32)
                };
                let swap = imm(x).is_some() && imm(y).is_none();
                let (x, y) = if swap { (y, x) } else { (x, y) };
                let cc = match (cond, swap) {
                    (IrCond::Eq, _) => Cond::E,
                    (IrCond::Ne, _) => Cond::Ne,
                    (IrCond::Lt, false) => Cond::L,
                    (IrCond::Lt, true) => Cond::G,
                    (IrCond::Ge, false) => Cond::Ge,
                    (IrCond::Ge, true) => Cond::Le,
                    (IrCond::Ltu, false) => Cond::B,
                    (IrCond::Ltu, true) => Cond::A,
                    (IrCond::Geu, false) => Cond::Ae,
                    (IrCond::Geu, true) => Cond::Be,
                };
                let c = imm(y);
                let ra = self.ra.get(&mut self.a, x, &[])?;
                let rb = match c {
                    Some(_) => None,
                    None => Some(self.ra.get(&mut self.a, y, &[ra])?),
                };
                self.ra.write_back_all(&mut self.a)?;
                match (rb, c) {
                    (Some(rb), _) => self.a.alu_rr(Size::B64, Alu::Cmp, ra, rb),
                    // x - 0 sets the same ZF/SF/CF/OF as TEST (CF = OF = 0).
                    (None, Some(0)) => self.a.test_rr(Size::B64, ra, ra),
                    (None, Some(c)) => self.a.alu_ri(Size::B64, Alu::Cmp, ra, c),
                    (None, None) => unreachable!(),
                }
                self.exit_cond(cc, taken);
                self.exit_direct(fall);
            }
            Op::Jump { pc } => {
                self.ra.write_back_all(&mut self.a)?;
                self.exit_direct(pc);
            }
            Op::JumpInd { target } => {
                let rt = self.ra.get(&mut self.a, target, &[])?;
                self.ra.write_back_all(&mut self.a)?;
                if rt != Rax {
                    self.a.mov_rr(Size::B64, Rax, rt);
                }
                self.jump_cache();
            }
            Op::Exit { kind, pc } => {
                self.ra.write_back_all(&mut self.a)?;
                match kind {
                    // Not retired yet: refund its charge.
                    ExitKind::Ecall => self.exit_special(pc, 1, exit::ECALL, None),
                    ExitKind::FenceI => self.exit_special(pc, 0, exit::FLUSH, None),
                    ExitKind::Fault(e) => self.exit_special(pc, 0, exit::EXCEPTION, Some(e)),
                }
            }
            _ => unreachable!("not a terminator"),
        }
        Ok(())
    }
}

/// Signed RAX / R11 (or remainder); RISC-V results for x/0 and MIN/-1 (§7.5). Returns the
/// result register.
fn div_signed(a: &mut Asm, size: Size, rem: bool) -> Reg {
    let (zero, normal, done) = (a.new_label(), a.new_label(), a.new_label());
    a.test_rr(size, R11, R11);
    a.jcc_short(Cond::E, zero);
    a.alu_ri(size, Alu::Cmp, R11, -1);
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
    a.unary(size, Unary::Idiv, R11);
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
    Rax
}

/// Unsigned RAX / R11 (or remainder); x/0 = all ones, x%0 = x.
fn div_unsigned(a: &mut Asm, size: Size, rem: bool) -> Reg {
    let (zero, done) = (a.new_label(), a.new_label());
    a.test_rr(size, R11, R11);
    a.jcc_short(Cond::E, zero);
    a.mov_r32_imm(Rdx, 0);
    a.unary(size, Unary::Div, R11);
    if rem {
        a.mov_rr(Size::B64, Rax, Rdx);
    }
    a.jmp_short(done);
    a.bind(zero);
    if !rem {
        a.mov_imm(Rax, u64::MAX);
    }
    a.bind(done);
    Rax
}

/// Lower `b` (already optimized if wanted) for placement at RX address `origin`.
pub fn translate(
    b: &Block,
    origin: u64,
    tb_id: u32,
    tr: &Trampolines,
    opts: IrOptions,
) -> std::result::Result<LoweredIr, OutOfSlots> {
    let mut c = Ctx {
        a: Asm::new(origin),
        ra: Alloc::new(b, opts.pinned, opts.lazy),
        tr,
        tb_id,
        n: b.n_insns,
        opts,
        stubs: Vec::new(),
        helper_exit: None,
        exits: [None, None],
        pcmap: Vec::new(),
        sites: Vec::new(),
        pc: b.pc,
        idx: 0,
    };
    if c.n > 0 {
        c.a.alu_ri(Size::B64, Alu::Sub, budget(), c.n as i32);
        let l = c.stub(PcSrc::Const(b.pc), c.n, exit::BUDGET, SLOT_SPECIAL);
        c.a.jcc(Cond::L, l);
    }
    for (i, op) in b.ops.iter().enumerate() {
        c.ra.pos = i as u32;
        match *op {
            Op::Insn { pc, idx } => {
                c.pc = pc;
                c.idx = idx;
                c.pcmap.push(PcEntry {
                    host_off: c.a.pos() as u32,
                    idx,
                    guest_pc: pc,
                });
            }
            Op::Const { dst, imm } => c.ra.def_const(dst, imm),
            Op::ReadReg { dst, g } => c.ra.read_reg(dst, g),
            Op::WriteReg { g, src } => {
                c.ra.write_reg(&mut c.a, g, src)?;
                c.ra.release(&[src]);
            }
            Op::Bin { op, dst, a, b } => c.bin(op, dst, a, b)?,
            Op::BinImm { op, dst, a, imm } => c.bin_imm(op, dst, a, imm)?,
            Op::Load {
                dst,
                addr,
                off,
                size,
                signed,
            } => c.load(dst, addr, off, size, signed)?,
            Op::Store {
                addr,
                off,
                val,
                size,
            } => c.store(addr, off, val, size)?,
            Op::Interp { raw, pc, idx } => c.interp(raw, pc, idx)?,
            _ => {
                c.terminator(op)?;
                break;
            }
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
    let stats = c.ra.stats;
    Ok(LoweredIr {
        code: c.a.finish(),
        pcmap: c.pcmap,
        exits,
        fault_sites: c.sites,
        stats,
    })
}
