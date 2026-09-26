//! Exception and interrupt entry, delegation, MRET/SRET (CLAUDE.md §15; priv spec §3.1.6–3.3).

use super::csr::mstatus as ms;
use super::state::CpuState;
use crate::mem::{Access, MemFault};

/// Privilege levels.
pub mod prv {
    pub const U: u8 = 0;
    pub const S: u8 = 1;
    pub const M: u8 = 3;
}

/// Exception cause codes (`mcause` with the interrupt bit clear).
pub mod cause {
    pub const INSN_MISALIGNED: u64 = 0;
    pub const INSN_ACCESS: u64 = 1;
    pub const ILLEGAL_INSN: u64 = 2;
    pub const BREAKPOINT: u64 = 3;
    pub const LOAD_MISALIGNED: u64 = 4;
    pub const LOAD_ACCESS: u64 = 5;
    pub const STORE_MISALIGNED: u64 = 6;
    pub const STORE_ACCESS: u64 = 7;
    pub const ECALL_U: u64 = 8;
    pub const ECALL_S: u64 = 9;
    pub const ECALL_M: u64 = 11;
    pub const INSN_PAGE: u64 = 12;
    pub const LOAD_PAGE: u64 = 13;
    pub const STORE_PAGE: u64 = 15;
}

/// A synchronous exception: cause code and trap value (`xtval`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Exception {
    pub cause: u64,
    pub tval: u64,
}

impl Exception {
    /// Illegal instruction; `tval` holds the instruction bits (as Spike does).
    pub fn illegal(raw: u32) -> Self {
        Exception {
            cause: cause::ILLEGAL_INSN,
            tval: raw as u64,
        }
    }
}

impl From<MemFault> for Exception {
    /// A failed guest access is an access fault of the matching kind (tval = address).
    fn from(f: MemFault) -> Self {
        let cause = match f.access {
            Access::Load => cause::LOAD_ACCESS,
            Access::Store => cause::STORE_ACCESS,
            Access::Fetch => cause::INSN_ACCESS,
        };
        Exception {
            cause,
            tval: f.addr,
        }
    }
}

impl std::fmt::Display for Exception {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self.cause {
            cause::INSN_MISALIGNED => "instruction address misaligned",
            cause::INSN_ACCESS => "instruction access fault",
            cause::ILLEGAL_INSN => "illegal instruction",
            cause::BREAKPOINT => "breakpoint",
            cause::LOAD_MISALIGNED => "load address misaligned",
            cause::LOAD_ACCESS => "load access fault",
            cause::STORE_MISALIGNED => "store/AMO address misaligned",
            cause::STORE_ACCESS => "store/AMO access fault",
            cause::ECALL_U => "ecall from U-mode",
            cause::ECALL_S => "ecall from S-mode",
            cause::ECALL_M => "ecall from M-mode",
            cause::INSN_PAGE => "instruction page fault",
            cause::LOAD_PAGE => "load page fault",
            cause::STORE_PAGE => "store/AMO page fault",
            _ => "unknown exception",
        };
        write!(f, "{name} (cause {}, tval {:#x})", self.cause, self.tval)
    }
}

impl CpuState {
    /// Take a trap at `self.pc`: record cause/epc/tval, update the status stack, switch
    /// privilege and jump to the trap vector. Chooses S-mode when the cause is delegated and
    /// the hart is not in M-mode.
    pub fn take_trap(&mut self, e: Exception, interrupt: bool) {
        let cause = e.cause | if interrupt { 1 << 63 } else { 0 };
        let deleg = if interrupt {
            self.csr.mideleg
        } else {
            self.csr.medeleg
        };
        // A trap clears any LR reservation (implementations may fail SC after a trap).
        self.res_valid = 0;
        let vectored = |tvec: u64| {
            if interrupt && tvec & 1 == 1 {
                (tvec & !3) + 4 * e.cause
            } else {
                tvec & !3
            }
        };
        let s = &mut self.csr;
        if self.prv <= prv::S && (deleg >> e.cause) & 1 == 1 {
            s.sepc = self.pc;
            s.scause = cause;
            s.stval = e.tval;
            let sie = s.mstatus & ms::SIE != 0;
            s.mstatus = (s.mstatus & !(ms::SPIE | ms::SIE | ms::SPP))
                | if sie { ms::SPIE } else { 0 }
                | if self.prv == prv::S { ms::SPP } else { 0 };
            self.prv = prv::S;
            self.pc = vectored(s.stvec);
        } else {
            s.mepc = self.pc;
            s.mcause = cause;
            s.mtval = e.tval;
            let mie = s.mstatus & ms::MIE != 0;
            s.mstatus = (s.mstatus & !(ms::MPIE | ms::MIE | ms::MPP))
                | if mie { ms::MPIE } else { 0 }
                | ((self.prv as u64) << ms::MPP_SHIFT);
            self.prv = prv::M;
            self.pc = vectored(s.mtvec);
        }
    }

    /// MRET (only legal in M-mode; the caller checks). Returns the new pc.
    pub fn mret(&mut self) -> u64 {
        let s = &mut self.csr;
        let mpp = ((s.mstatus & ms::MPP) >> ms::MPP_SHIFT) as u8;
        let mpie = s.mstatus & ms::MPIE != 0;
        s.mstatus = (s.mstatus & !(ms::MIE | ms::MPP)) | ms::MPIE | if mpie { ms::MIE } else { 0 };
        if mpp != prv::M {
            s.mstatus &= !ms::MPRV;
        }
        self.prv = mpp;
        s.mepc
    }

    /// SRET (legal in S/M-mode unless TSR; the caller checks). Returns the new pc.
    pub fn sret(&mut self) -> u64 {
        let s = &mut self.csr;
        let spp = if s.mstatus & ms::SPP != 0 {
            prv::S
        } else {
            prv::U
        };
        let spie = s.mstatus & ms::SPIE != 0;
        s.mstatus = (s.mstatus & !(ms::SIE | ms::SPP | ms::MPRV))
            | ms::SPIE
            | if spie { ms::SIE } else { 0 };
        self.prv = spp;
        s.sepc
    }
}
