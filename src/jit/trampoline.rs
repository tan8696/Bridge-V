//! `enter_jit` / `exit_jit` / `fault_exit` trampolines and the helper address table, generated
//! into the start of the code buffer (CLAUDE.md §8.4). Also the Rust helpers JIT code calls.
//!
//! Buffer prefix layout:
//! ```text
//!   +0   helper table: one 8-byte absolute address per helper (called via `call [rip+d32]`)
//!   ...  enter_jit(rdi = &CpuState, rsi = TB entry) -> rax
//!   ...  exit_jit (rax = exit code)
//!   ...  fault_exit (the SIGSEGV handler redirects RIP here)
//! ```

use std::mem::offset_of;
use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::backend::x86::emit::{Alu, Asm, Mem, Size};
use crate::backend::x86::regs::{CPU, CPU_BIAS, MEM_BASE, Reg};
use crate::cpu::state::{CpuState, exit};
use crate::interp::{Flow, step};
use crate::isa::decode_parts;
use crate::isa::inst::Inst;
use crate::mem::direct::DirectMem;
use crate::mem::{Access, mmu};

use super::code_mem::CodeMem;

/// Indexes into the helper table.
pub mod helper {
    /// `helper_interp_one(cpu, raw, pc) -> u64` (D14).
    pub const INTERP_ONE: usize = 0;
    /// `helper_mmu_access(cpu, va, info, val) -> u64` (softmmu slow path, D48).
    pub const MMU_ACCESS: usize = 1;
    pub const COUNT: usize = 2;
}

/// `[rbp + disp]` addressing a `CpuState` field at byte offset `off` (RBP is biased, §8.1).
pub fn cpu_field(off: usize) -> Mem {
    Mem::base(CPU, off as i32 - CPU_BIAS)
}

/// Exit-code slot meaning "look at `cpu.exit_reason`" (§8.4). Slots 0/1 are direct exits.
pub const SLOT_SPECIAL: u64 = 2;

/// Addresses of the generated trampolines (RX view).
#[derive(Clone, Copy, Debug)]
pub struct Trampolines {
    pub enter: u64,
    pub exit: u64,
    pub fault_exit: u64,
    pub helper_table: u64,
    /// End of the prefix (first TB address).
    pub end: u64,
}

type EnterFn = extern "sysv64" fn(*mut CpuState, u64) -> u64;

impl Trampolines {
    /// Generate the prefix into an empty code buffer and seal it (it survives flushes).
    /// `pinned` lists the guest registers kept in host registers across blocks (§8.2):
    /// `enter_jit` loads them from `CpuState`, `exit_jit` stores them back (§8.3, §8.4).
    /// `pinned`: guest registers living in host registers across TBs; `budget_reg`: the
    /// register holding the budget inside JIT code, if not `CpuState.budget` (D46).
    pub fn generate(
        cm: &mut CodeMem,
        pinned: &[(u8, Reg)],
        budget_reg: Option<Reg>,
    ) -> Trampolines {
        let helpers: [u64; helper::COUNT] = [
            helper_interp_one as *const () as u64,
            helper_mmu_access as *const () as u64,
        ];
        let origin = cm.next_addr();
        let mut a = Asm::new(origin);
        let helper_table = a.here();
        for h in helpers {
            a.data64(h);
        }
        a.align(16, 0);

        // enter_jit: save callee-saved registers, align the stack, set up RBP/RBX, jump.
        let enter = a.here();
        for r in [Reg::Rbp, Reg::Rbx, Reg::R12, Reg::R13, Reg::R14, Reg::R15] {
            a.push(r);
        }
        // Entry RSP ≡ 8 (mod 16); six pushes + 8 make it ≡ 0, as every `call` needs (§8.2).
        a.alu_ri(Size::B64, Alu::Sub, Reg::Rsp, 8);
        // Guest FP code runs with the default MXCSR and no pending exception flags; exit_jit
        // folds the flags it raised into fflags (D47). The alignment pad at [rsp] is scratch.
        let pad = Mem::base(Reg::Rsp, 0);
        a.store_imm(Size::B32, pad, crate::cpu::fp::MXCSR_DEFAULT as i32);
        a.ldmxcsr(pad);
        a.lea(CPU, Mem::base(Reg::Rdi, CPU_BIAS));
        a.load(
            Size::B64,
            MEM_BASE,
            cpu_field(offset_of!(CpuState, mem_base)),
        );
        for &(g, r) in pinned {
            a.load(Size::B64, r, cpu_field(8 * g as usize));
        }
        if let Some(r) = budget_reg {
            a.load(Size::B64, r, cpu_field(offset_of!(CpuState, budget)));
        }
        a.jmp_rm(Reg::Rsi);

        // exit_jit: restore and return RAX to enter_jit's caller.
        a.align(16, 0);
        let exit = a.here();
        for &(g, r) in pinned {
            a.store(Size::B64, cpu_field(8 * g as usize), r);
        }
        if let Some(r) = budget_reg {
            a.store(Size::B64, cpu_field(offset_of!(CpuState, budget)), r);
        }
        emit_fold_mxcsr(&mut a);
        a.alu_ri(Size::B64, Alu::Add, Reg::Rsp, 8);
        for r in [Reg::R15, Reg::R14, Reg::R13, Reg::R12, Reg::Rbx, Reg::Rbp] {
            a.pop(r);
        }
        a.ret();

        // fault_exit: entered from the SIGSEGV handler with the faulting JIT frame's registers
        // (RBP intact, RSP = enter_jit's RSP since JIT code never pushes).
        a.align(16, 0);
        let fault_exit = a.here();
        a.store_imm(
            Size::B32,
            cpu_field(offset_of!(CpuState, exit_reason)),
            exit::HOST_FAULT as i32,
        );
        a.mov_r32_imm(Reg::Rax, SLOT_SPECIAL as u32);
        a.jmp_abs(exit);

        let code = a.finish();
        cm.place(origin, &code)
            .expect("code buffer too small for trampolines");
        cm.seal_prefix();
        Trampolines {
            enter,
            exit,
            fault_exit,
            helper_table,
            end: origin + code.len() as u64,
        }
    }

    /// Address of helper table slot `idx` (for `call [rip + d32]`).
    pub fn helper_slot(&self, idx: usize) -> u64 {
        assert!(idx < helper::COUNT);
        self.helper_table + 8 * idx as u64
    }

    /// Run JIT code at `entry` until it exits; returns the exit code (§8.4).
    ///
    /// # Safety
    /// `entry` must be the start of a translation block in the code buffer these trampolines
    /// were generated into, and that buffer must still be mapped. `cpu.mem_base` must be the
    /// base of the guest space the block was translated for, and `cpu.helper_mem` must point
    /// to that live `DirectMem`.
    pub unsafe fn enter(&self, cpu: &mut CpuState, entry: u64) -> u64 {
        // SAFETY: `self.enter` is the enter_jit trampoline generated above, which follows the
        // SysV ABI (callee-saved registers restored, stack aligned) for this signature.
        let f: EnterFn = unsafe { std::mem::transmute(self.enter as *const u8) };
        f(cpu as *mut CpuState, entry)
    }
}

/// exit_jit: OR the MXCSR exception flags raised by JIT FP code into `cpu.fflags` (same map
/// as `fp::mxcsr_to_fflags`). RAX holds the exit code; RCX, RDX, R10 are free at every exit.
fn emit_fold_mxcsr(a: &mut Asm) {
    use crate::backend::x86::emit::Shift;
    let pad = Mem::base(Reg::Rsp, 0);
    let (m, acc, t) = (Reg::Rcx, Reg::Rdx, Reg::R10);
    a.stmxcsr(pad);
    a.load(Size::B32, m, pad);
    a.mov_r32_imm(acc, 0);
    // (source bit, destination bit): IE→NV, ZE→DZ, OE→OF, UE→UF, PE→NX.
    for (from, to) in [(0u8, 4u8), (2, 3), (3, 2), (4, 1), (5, 0)] {
        a.mov_rr(Size::B32, t, m);
        if from > 0 {
            a.shift_ri(Size::B32, Shift::Shr, t, from);
        }
        a.alu_ri(Size::B32, Alu::And, t, 1);
        if to > 0 {
            a.shift_ri(Size::B32, Shift::Shl, t, to);
        }
        a.alu_rr(Size::B32, Alu::Or, acc, t);
    }
    a.alu_mr(
        Size::B8,
        Alu::Or,
        cpu_field(offset_of!(CpuState, fflags)),
        acc,
    );
}

/// Fold the host FP flags raised so far by JIT code into `fflags` and reset MXCSR, so the
/// interpreter (SoftFloat) sees exact fflags (D47).
fn fold_and_reset_mxcsr(cpu: &mut CpuState) {
    let mut m: u32 = 0;
    // SAFETY: stmxcsr to a valid 4-byte location.
    unsafe { std::arch::asm!("stmxcsr [{}]", in(reg) &mut m, options(nostack)) };
    cpu.fflags |= crate::cpu::fp::mxcsr_to_fflags(m);
    reset_mxcsr();
}

fn reset_mxcsr() {
    let d = crate::cpu::fp::MXCSR_DEFAULT;
    // SAFETY: ldmxcsr of the default value (what Rust code expects) from a valid location.
    unsafe { std::arch::asm!("ldmxcsr [{}]", in(reg) &d, options(nostack, readonly)) };
}

/// Execute one instruction with the interpreter (D14). Called from JIT code with all guest
/// state in `CpuState` and the budget charge for this and later instructions of the TB
/// refunded, so `icount + (budget_ref - budget)` is exact; the helper folds that into
/// `icount` first (D30) so CSR reads of `cycle`/`instret` see the right value. Returns 0 to
/// continue with the next instruction, 1 to leave the block (`cpu.pc`, `cpu.exit_reason`
/// and, for exceptions, `exc_cause`/`exc_tval` are set; `icount` counts the instruction if
/// it retired).
///
/// `raw` holds the instruction bits (16 or 32 of them). Never unwinds into JIT code.
///
/// # Safety
/// `cpu` must point to a live `CpuState` whose `helper_mem` points to the live `DirectMem` of
/// the same guest, neither otherwise borrowed for the duration of the call.
pub unsafe extern "sysv64" fn helper_interp_one(cpu: *mut CpuState, raw: u64, pc: u64) -> u64 {
    let r = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: JIT code passes RBP - 128, the CpuState it runs on; `helper_mem` was set
        // by the dispatcher to the guest's DirectMem, which is not otherwise borrowed while
        // JIT code runs.
        let cpu = unsafe { &mut *cpu };
        // SAFETY: see above.
        let mem = unsafe { &mut *(cpu.helper_mem as *mut DirectMem) };
        cpu.icount += (cpu.budget_ref - cpu.budget) as u64;
        cpu.budget_ref = cpu.budget;
        fold_and_reset_mxcsr(cpu);
        let d = decode_parts::<()>(raw as u16, || Ok((raw >> 16) as u16)).expect("infallible");
        // System mode (D51): SFENCE.VMA, or a CSR write that changes the MMU flags or
        // mappings the TB's successors were looked up with or makes an interrupt deliverable,
        // leaves the TB so the dispatcher re-derives them; otherwise chained exits stay valid.
        let mmu_ctx = |cpu: &CpuState| (super::dispatch::soft_flags(cpu), cpu.jc_gen);
        let before =
            (cpu.softmmu == 1 && !matches!(d.inst, Inst::SfenceVma { .. })).then(|| mmu_ctx(cpu));
        let sfence = cpu.softmmu == 1 && before.is_none();
        let flow = step(cpu, mem, &d, pc);
        // Discard any host flags raised by Rust code in the helper (none expected).
        reset_mxcsr();
        match flow {
            // A store hit a page holding translated code: leave right after it (D49).
            Flow::Next if !mem.smc_pages.is_empty() => {
                cpu.icount += 1;
                cpu.pc = pc.wrapping_add(d.len as u64);
                cpu.exit_reason = exit::SMC;
                1
            }
            Flow::Next
                if sfence
                    || before.is_some_and(|b| {
                        b != mmu_ctx(cpu) || cpu.pending_interrupt().is_some()
                    }) =>
            {
                cpu.icount += 1;
                cpu.pc = pc.wrapping_add(d.len as u64);
                cpu.exit_reason = exit::NONE;
                1
            }
            Flow::Next => 0,
            Flow::Jump(target) => {
                cpu.icount += 1;
                cpu.pc = target;
                cpu.exit_reason = exit::NONE;
                1
            }
            Flow::Flush => {
                cpu.icount += 1;
                cpu.pc = pc.wrapping_add(d.len as u64);
                cpu.exit_reason = exit::FLUSH;
                1
            }
            Flow::Wfi => {
                cpu.icount += 1;
                cpu.pc = pc.wrapping_add(d.len as u64);
                cpu.exit_reason = exit::WFI;
                1
            }
            Flow::Trap(e) => {
                cpu.pc = pc;
                cpu.exc_cause = e.cause;
                cpu.exc_tval = e.tval;
                cpu.exit_reason = exit::EXCEPTION;
                1
            }
            Flow::Ecall => {
                cpu.pc = pc;
                cpu.exit_reason = exit::ECALL;
                1
            }
        }
    }));
    r.unwrap_or_else(|_| {
        eprintln!("bridgev: panic in JIT helper; aborting");
        std::process::abort()
    })
}

/// The softmmu slow path of an inline load or store (D48): translate through the TLB (walking
/// and filling on a miss), then access RAM or a device; misaligned accesses that cross a page
/// are split. `info` = size | signed << 4 | store << 5 | MMU index << 8. Returns the loaded
/// value, sign- or zero-extended. On an exception, sets `exc_cause`/`exc_tval` and
/// `exit_reason = MMU_FAULT` (the JIT code then leaves through its fault exit).
///
/// # Safety
/// Called only from JIT code with the running `CpuState` (as `helper_interp_one`).
pub unsafe extern "sysv64" fn helper_mmu_access(
    cpu: *mut CpuState,
    va: u64,
    info: u64,
    val: u64,
) -> u64 {
    let r = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: as in `helper_interp_one`.
        let cpu = unsafe { &mut *cpu };
        // SAFETY: as in `helper_interp_one`.
        let mem = unsafe { &mut *(cpu.helper_mem as *mut DirectMem) };
        // JIT code refunded the unretired instructions into `budget` (D30).
        cpu.icount += (cpu.budget_ref - cpu.budget) as u64;
        cpu.budget_ref = cpu.budget;
        let size = info & 15;
        let mmu_idx = (info >> 8) as u8;
        let r = if info & 32 != 0 {
            mmu::soft_store(cpu, mem, va, size, val, mmu_idx).map(|_| 0)
        } else {
            mmu::soft_load(cpu, mem, va, size, Access::Load, mmu_idx).map(|v| {
                let sh = 64 - 8 * size as u32;
                if info & 16 != 0 {
                    ((v << sh) as i64 >> sh) as u64
                } else {
                    v
                }
            })
        };
        match r {
            Ok(v) => {
                if !mem.smc_pages.is_empty() {
                    cpu.exit_reason = exit::SMC_STORE;
                }
                v
            }
            Err(e) => {
                cpu.exc_cause = e.cause;
                cpu.exc_tval = e.tval;
                cpu.exit_reason = exit::MMU_FAULT;
                0
            }
        }
    }));
    r.unwrap_or_else(|_| {
        eprintln!("bridgev: panic in JIT helper; aborting");
        std::process::abort()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jit::code_mem::WxMode;

    fn setup() -> (CodeMem, Trampolines) {
        let mut cm = CodeMem::new(1 << 20, WxMode::DualMap).unwrap();
        let t = Trampolines::generate(&mut cm, &[], None);
        (cm, t)
    }

    /// A block that increments `cpu.x[5]` and exits with code 7.
    fn inc_block(cm: &mut CodeMem, t: &Trampolines) -> u64 {
        let mut a = Asm::new(cm.next_addr());
        let x5 = cpu_field(8 * 5);
        a.load(Size::B64, Reg::Rax, x5);
        a.alu_ri(Size::B64, Alu::Add, Reg::Rax, 1);
        a.store(Size::B64, x5, Reg::Rax);
        a.mov_r32_imm(Reg::Rax, 7);
        a.jmp_abs(t.exit);
        let o = a.origin();
        cm.place(o, &a.finish()).unwrap()
    }

    #[test]
    fn enter_block_exit() {
        let (mut cm, t) = setup();
        let b = inc_block(&mut cm, &t);
        let mut cpu = CpuState::new_user(0);
        cpu.x[5] = 41;
        // SAFETY: `b` is a complete block in this buffer that only touches cpu.x[5].
        let code = unsafe { t.enter(&mut cpu, b) };
        assert_eq!((code, cpu.x[5]), (7, 42));
        assert_eq!(unsafe { t.enter(&mut cpu, b) }, 7);
        assert_eq!(cpu.x[5], 43);
    }

    /// Callee-saved registers survive a block that clobbers them (checked from an `asm!`
    /// harness, since Rust code cannot observe RBX/RBP directly). RBP must stay the CpuState
    /// pointer inside JIT code (§8.3: `exit_jit` folds fflags through it); it is still
    /// checked, because `enter_jit` itself replaces the caller's RBP.
    #[test]
    fn callee_saved_registers_preserved() {
        let (mut cm, t) = setup();
        let mut a = Asm::new(cm.next_addr());
        for r in [Reg::Rbx, Reg::R12, Reg::R13, Reg::R14, Reg::R15] {
            a.mov_imm(r, 0xDEAD_0000 + r.num() as u64);
        }
        a.mov_r32_imm(Reg::Rax, 5);
        a.jmp_abs(t.exit);
        let o = a.origin();
        let block = cm.place(o, &a.finish()).unwrap();
        let mut cpu = CpuState::new_user(0);
        let (mut r12, mut r13, mut r14, mut r15) = (0x12u64, 0x13u64, 0x14u64, 0x15u64);
        let (rbx_after, rbp_after, ret): (u64, u64, u64);
        // SAFETY: the asm saves and restores RBX/RBP itself, realigns the stack for the call,
        // and declares every other register it or the callee may clobber.
        unsafe {
            std::arch::asm!(
                "push rbx",
                "push rbp",
                "mov rbx, 0xB0B0",
                "mov rbp, 0xB1B1",
                "mov r11, rsp",
                "and rsp, -16",
                "push r11",
                "sub rsp, 8",
                "call {enter}",
                "add rsp, 8",
                "pop rsp",
                "mov r8, rbx",
                "mov r9, rbp",
                "pop rbp",
                "pop rbx",
                enter = in(reg) t.enter,
                lateout("r8") rbx_after,
                lateout("r9") rbp_after,
                in("rdi") &mut *cpu as *mut CpuState,
                in("rsi") block,
                inout("r12") r12,
                inout("r13") r13,
                inout("r14") r14,
                inout("r15") r15,
                lateout("rax") ret,
                out("r11") _,
                clobber_abi("sysv64"),
            );
        }
        assert_eq!(ret, 5);
        assert_eq!((rbx_after, rbp_after), (0xB0B0, 0xB1B1));
        assert_eq!((r12, r13, r14, r15), (0x12, 0x13, 0x14, 0x15));
    }

    #[test]
    fn prefix_survives_reset() {
        let (mut cm, t) = setup();
        let first = cm.next_addr();
        assert!(first >= t.end);
        inc_block(&mut cm, &t);
        cm.reset();
        assert_eq!(cm.next_addr(), first);
        let b = inc_block(&mut cm, &t);
        let mut cpu = CpuState::new_user(0);
        assert_eq!(unsafe { t.enter(&mut cpu, b) }, 7);
    }

    /// Pinned guest registers (§8.2, §8.3): `enter_jit` loads them from their `CpuState`
    /// homes, JIT code works on the host registers only, and `exit_jit` stores them back.
    #[test]
    fn pinned_registers_round_trip() {
        let mut cm = CodeMem::new(1 << 20, WxMode::DualMap).unwrap();
        let pins = [(2, Reg::R12), (1, Reg::R13), (10, Reg::R14), (15, Reg::R15)];
        let t = Trampolines::generate(&mut cm, &pins, None);
        let mut a = Asm::new(cm.next_addr());
        for (k, &(_, r)) in pins.iter().enumerate() {
            // The home is stale inside the block: clobber it, the exit must overwrite it.
            a.store_imm(Size::B64, cpu_field(8 * pins[k].0 as usize), -1);
            a.alu_ri(Size::B64, Alu::Add, r, 0x100 * (k as i32 + 1));
        }
        a.mov_r32_imm(Reg::Rax, 9);
        a.jmp_abs(t.exit);
        let o = a.origin();
        let b = cm.place(o, &a.finish()).unwrap();
        let mut cpu = CpuState::new_user(0);
        for (g, _) in pins {
            cpu.x[g as usize] = 1000 + g as u64;
        }
        // SAFETY: `b` only touches the pinned registers and their homes.
        assert_eq!(unsafe { t.enter(&mut cpu, b) }, 9);
        for (k, (g, _)) in pins.into_iter().enumerate() {
            assert_eq!(
                cpu.x[g as usize],
                1000 + g as u64 + 0x100 * (k as u64 + 1),
                "x{g}"
            );
        }
        assert_eq!(cpu.x[3], 0, "unpinned registers untouched");
    }
}
