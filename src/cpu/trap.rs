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
    /// The interrupt to take now, if any (priv spec §3.1.9): M-level interrupts (not
    /// delegated) when below M or MIE is set, S-level (delegated) ones when below S or in S
    /// with SIE set. Priority MEI > MSI > MTI > SEI > SSI > STI.
    pub fn pending_interrupt(&self) -> Option<u64> {
        let s = &self.csr;
        let pending = s.mip & s.mie;
        if pending == 0 {
            return None;
        }
        let m_on = self.prv < prv::M || s.mstatus & ms::MIE != 0;
        let s_on = self.prv < prv::S || (self.prv == prv::S && s.mstatus & ms::SIE != 0);
        let pick = |bits: u64| {
            [11u64, 3, 7, 9, 1, 5]
                .into_iter()
                .find(|&c| (bits >> c) & 1 == 1)
        };
        let m_ints = pending & !s.mideleg;
        let s_ints = pending & s.mideleg;
        if m_on && m_ints != 0 {
            return pick(m_ints);
        }
        if s_on && s_ints != 0 {
            return pick(s_ints);
        }
        None
    }

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

#[cfg(test)]
mod tests {
    //! Interrupt enable/priority/delegation matrix (P7.2, priv spec §3.1.9).
    use super::*;

    const SSI: u64 = 1 << 1;
    const MSI: u64 = 1 << 3;
    const STI: u64 = 1 << 5;
    const MTI: u64 = 1 << 7;
    const SEI: u64 = 1 << 9;
    const MEI: u64 = 1 << 11;

    fn hart(p: u8, mstatus: u64, mip: u64, mie: u64, mideleg: u64) -> Box<CpuState> {
        let mut c = CpuState::new_machine(0x8000_0000);
        c.prv = p;
        c.csr.mstatus = mstatus;
        c.csr.mip = mip;
        c.csr.mie = mie;
        c.csr.mideleg = mideleg;
        c
    }

    #[test]
    fn priority_order() {
        let all = MEI | MSI | MTI | SEI | SSI | STI;
        let mut pending = all;
        for want in [11, 3, 7, 9, 1, 5] {
            let c = hart(prv::U, 0, pending, all, 0);
            assert_eq!(c.pending_interrupt(), Some(want));
            pending &= !(1 << want);
        }
        assert_eq!(hart(prv::U, 0, 0, all, 0).pending_interrupt(), None);
        assert_eq!(
            hart(prv::U, 0, all, 0, 0).pending_interrupt(),
            None,
            "masked by mie"
        );
    }

    #[test]
    fn enable_rules_for_every_privilege() {
        // (priv, mstatus, delegated?) → taken?
        for p in [prv::U, prv::S, prv::M] {
            for mstatus in [0, ms::MIE, ms::SIE, ms::MIE | ms::SIE] {
                // Not delegated: an M-level interrupt, taken below M or with MIE.
                let m = hart(p, mstatus, MTI, MTI, 0).pending_interrupt();
                let m_want = p < prv::M || mstatus & ms::MIE != 0;
                assert_eq!(m.is_some(), m_want, "MTI prv {p} mstatus {mstatus:#x}");
                // Delegated: an S-level interrupt, taken below S or in S with SIE, never in M.
                let s = hart(p, mstatus, STI, STI, STI).pending_interrupt();
                let s_want = p < prv::S || (p == prv::S && mstatus & ms::SIE != 0);
                assert_eq!(s.is_some(), s_want, "STI prv {p} mstatus {mstatus:#x}");
            }
        }
        // An enabled M-level interrupt wins over a higher-priority delegated one.
        let c = hart(prv::S, ms::SIE, MEI | MTI, MEI | MTI, MEI);
        assert_eq!(c.pending_interrupt(), Some(7));
    }

    #[test]
    fn delegated_trap_entry_and_vectoring() {
        let mut c = hart(prv::U, ms::SIE, STI, STI, STI);
        c.csr.stvec = 0x1000 | 1; // vectored
        c.csr.mtvec = 0x2000 | 1;
        c.pc = 0x4444;
        let cause = c.pending_interrupt().unwrap();
        c.take_trap(Exception { cause, tval: 0 }, true);
        assert_eq!((c.prv, c.pc), (prv::S, 0x1000 + 4 * 5));
        assert_eq!((c.csr.sepc, c.csr.scause), (0x4444, 1 << 63 | 5));
        assert_eq!(c.csr.mstatus & (ms::SIE | ms::SPIE | ms::SPP), ms::SPIE);
        // Exceptions never use the vector table.
        let mut c = hart(prv::S, 0, 0, 0, 0);
        c.csr.mtvec = 0x2000 | 1;
        c.take_trap(Exception::illegal(0), false);
        assert_eq!((c.prv, c.pc), (prv::M, 0x2000));
        assert_eq!((c.csr.mstatus & ms::MPP) >> ms::MPP_SHIFT, prv::S as u64);
        // medeleg does not apply to traps taken in M-mode.
        let mut c = hart(prv::M, 0, 0, 0, 0);
        c.csr.medeleg = u64::MAX;
        c.csr.mtvec = 0x3000;
        c.take_trap(Exception::illegal(0), false);
        assert_eq!((c.prv, c.pc), (prv::M, 0x3000));
    }
}
