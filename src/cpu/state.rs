//! `#[repr(C)] CpuState`, the structure JIT code addresses through RBP (CLAUDE.md §8.1).
//! Every JIT-visible offset is pinned with a compile-time `offset_of!` assert; the source of
//! truth for the layout is this file.

use std::mem::offset_of;

use super::csr::{Csrs, fs, mstatus};
use super::trap::prv;
use crate::mem::tlb::{NB_MMU_IDX, TLB_SIZE, TlbEntry};

/// Values of `CpuState::exit_reason` when JIT code returns to the dispatcher (D25).
pub mod exit {
    /// Plain exit: continue at `pc`.
    pub const NONE: u32 = 0;
    /// ECALL at `pc`.
    pub const ECALL: u32 = 1;
    /// Exception at `pc`: `exc_cause`, `exc_tval`.
    pub const EXCEPTION: u32 = 2;
    /// FENCE.I retired: flush translated code, continue at `pc`.
    pub const FLUSH: u32 = 3;
    /// Host SIGSEGV inside JIT code: `fault_rip`, `fault_addr` (resolved by the dispatcher).
    pub const HOST_FAULT: u32 = 4;
    /// A TB prologue found the budget exhausted (D12); continue at `pc`.
    pub const BUDGET: u32 = 5;
    /// JALR target not in the jump cache (§13.4); continue at `pc`.
    pub const LOOKUP: u32 = 6;
    /// A fast-FP-variant TB was entered with FS not Dirty or (dynamic rm) frm not RNE (D47);
    /// nothing executed, continue at `pc` with the matching variant.
    pub const FP_VARIANT: u32 = 7;
    /// A softmmu slow path raised `exc_cause`/`exc_tval` at the fault site whose host address
    /// is `fault_rip` (the dispatcher makes the state precise, D48).
    pub const MMU_FAULT: u32 = 8;
    pub const COUNT: usize = 9;
}

/// Spill slots for IR temporaries (§8.1, §10).
pub const SPILL_SLOTS: usize = 64;

/// Number of jump-cache entries (§13.4); a power of two.
pub const JC_SIZE: usize = 4096;

/// One jump-cache entry: guest pc → host address of its TB. `pc` is odd (never a valid
/// target, which is always 2-byte aligned) when the entry is empty.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JcEntry {
    pub pc: u64,
    pub host: u64,
}

impl JcEntry {
    pub const EMPTY: JcEntry = JcEntry {
        pc: u64::MAX,
        host: 0,
    };
}

/// Jump-cache slot of guest `pc`: `(pc >> 1) & (JC_SIZE - 1)` (§13.4).
#[inline]
pub fn jc_index(pc: u64) -> usize {
    (pc >> 1) as usize & (JC_SIZE - 1)
}

/// Architectural state of one hart. The first fields have fixed offsets that generated code
/// relies on; `csr` (Rust-only) must stay last.
#[repr(C, align(64))]
#[derive(Clone)]
pub struct CpuState {
    /// Integer registers; `x[0]` is always 0.
    pub x: [u64; 32],
    pub pc: u64,
    /// Remaining instruction budget for the current time slice (D12).
    pub budget: i64,
    pub exit_reason: u32,
    /// Current privilege level (`prv::U/S/M`).
    pub prv: u8,
    pub mmu_idx: u8,
    /// 1: memory accesses are translated (softmmu, D48); 0: direct user-mode memory.
    pub softmmu: u8,
    _pad0: u8,
    /// Retired guest instructions.
    pub icount: u64,
    /// Direct mode: host address of guest address 0.
    pub mem_base: u64,
    /// LR/SC reservation.
    pub res_addr: u64,
    pub res_val: u64,
    pub res_valid: u64,
    /// FP registers (single-precision values are NaN-boxed).
    pub f: [u64; 32],
    /// Accrued FP exception flags (fcsr[4:0]).
    pub fflags: u8,
    /// Dynamic rounding mode (fcsr[7:5]).
    pub frm: u8,
    _pad1: [u8; 6],
    /// Exception raised by JIT code or a JIT helper (`exit_reason == EXIT_EXCEPTION`).
    pub exc_cause: u64,
    pub exc_tval: u64,
    /// Host fault recorded by the SIGSEGV handler (`exit_reason == EXIT_HOST_FAULT`): the
    /// faulting host RIP (inside the code buffer) and host data address.
    pub fault_rip: u64,
    pub fault_addr: u64,
    /// `*mut DirectMem` of the running guest, for JIT helpers (set by the dispatcher).
    pub helper_mem: u64,
    /// `budget` value at which `icount` was last synchronized: retired instructions not yet in
    /// `icount` = `budget_ref - budget` (D30).
    pub budget_ref: i64,
    /// Identifies the translation cache generation the jump cache belongs to (D30).
    pub jc_tag: u64,
    /// `--profile-jit`: JALR executions counted by JIT code.
    pub prof_jalr: u64,
    /// Bumped whenever virtual-to-physical translation may have changed (TLB flush): engines
    /// revalidate their virtually keyed caches (decoded blocks, jump cache) against it.
    pub mmu_gen: u64,
    _pad2: [u64; 14],
    /// Inline JALR lookup table (§13.4).
    pub jmp_cache: [JcEntry; JC_SIZE],
    /// Register-allocator spill slots for values that are not guest registers (§10).
    pub spill: [u64; SPILL_SLOTS],
    /// Host general-purpose registers at the last host fault in JIT code, indexed by
    /// `backend::x86::regs::Reg` number (the SIGSEGV handler copies them from the ucontext);
    /// the dispatcher recovers dirty cached guest registers from them (§15).
    pub fault_regs: [u64; 16],
    /// Software TLB, one per MMU index (§14.4, D48).
    pub tlb: [[TlbEntry; TLB_SIZE]; NB_MMU_IDX],
    pub csr: Csrs,
}

// Offsets the JIT depends on (CLAUDE.md §8.1). Changing any of these requires updating
// CLAUDE.md in the same commit.
const _: () = {
    assert!(offset_of!(CpuState, x) == 0x000);
    assert!(offset_of!(CpuState, pc) == 0x100);
    assert!(offset_of!(CpuState, budget) == 0x108);
    assert!(offset_of!(CpuState, exit_reason) == 0x110);
    assert!(offset_of!(CpuState, prv) == 0x114);
    assert!(offset_of!(CpuState, mmu_idx) == 0x115);
    assert!(offset_of!(CpuState, softmmu) == 0x116);
    assert!(offset_of!(CpuState, icount) == 0x118);
    assert!(offset_of!(CpuState, mem_base) == 0x120);
    assert!(offset_of!(CpuState, res_addr) == 0x128);
    assert!(offset_of!(CpuState, res_val) == 0x130);
    assert!(offset_of!(CpuState, res_valid) == 0x138);
    assert!(offset_of!(CpuState, f) == 0x140);
    assert!(offset_of!(CpuState, fflags) == 0x240);
    assert!(offset_of!(CpuState, frm) == 0x241);
    assert!(offset_of!(CpuState, exc_cause) == 0x248);
    assert!(offset_of!(CpuState, exc_tval) == 0x250);
    assert!(offset_of!(CpuState, fault_rip) == 0x258);
    assert!(offset_of!(CpuState, fault_addr) == 0x260);
    assert!(offset_of!(CpuState, helper_mem) == 0x268);
    assert!(offset_of!(CpuState, budget_ref) == 0x270);
    assert!(offset_of!(CpuState, jc_tag) == 0x278);
    assert!(offset_of!(CpuState, prof_jalr) == 0x280);
    assert!(offset_of!(CpuState, jmp_cache) == 0x300);
    assert!(offset_of!(CpuState, spill) == 0x10300);
    assert!(offset_of!(CpuState, fault_regs) == 0x10500);
    assert!(offset_of!(CpuState, tlb) == 0x10580);
    assert!(std::mem::size_of::<JcEntry>() == 16);
};

impl CpuState {
    /// A hart in M-mode at `pc` with the FPU off (reset state, bare-metal environments).
    pub fn new_machine(pc: u64) -> Box<Self> {
        Box::new(CpuState {
            x: [0; 32],
            pc,
            budget: 0,
            exit_reason: 0,
            prv: prv::M,
            mmu_idx: 0,
            softmmu: 0,
            _pad0: 0,
            icount: 0,
            mem_base: 0,
            res_addr: 0,
            res_val: 0,
            res_valid: 0,
            f: [0; 32],
            fflags: 0,
            frm: 0,
            _pad1: [0; 6],
            exc_cause: 0,
            exc_tval: 0,
            fault_rip: 0,
            fault_addr: 0,
            helper_mem: 0,
            budget_ref: 0,
            jc_tag: 0,
            prof_jalr: 0,
            mmu_gen: 0,
            _pad2: [0; 14],
            jmp_cache: [JcEntry::EMPTY; JC_SIZE],
            spill: [0; SPILL_SLOTS],
            fault_regs: [0; 16],
            tlb: [[TlbEntry::EMPTY; TLB_SIZE]; NB_MMU_IDX],
            csr: Csrs::default(),
        })
    }

    /// A hart for Linux user-mode emulation: U-mode, FPU enabled, counters readable.
    pub fn new_user(pc: u64) -> Box<Self> {
        let mut c = Self::new_machine(pc);
        c.prv = prv::U;
        c.csr.mstatus = fs::INITIAL << mstatus::FS_SHIFT;
        c.csr.mcounteren = 0x7;
        c.csr.scounteren = 0x7;
        c
    }

    /// Empty the jump cache.
    pub fn clear_jump_cache(&mut self) {
        self.jmp_cache.fill(JcEntry::EMPTY);
    }

    /// Write integer register `r`, discarding writes to x0.
    #[inline(always)]
    pub fn set_x(&mut self, r: u8, v: u64) {
        if r != 0 {
            self.x[r as usize] = v;
        }
    }
}
