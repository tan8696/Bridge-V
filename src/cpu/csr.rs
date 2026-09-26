//! CSR file: read/write semantics, WARL legalization, privilege and counter-enable checks
//! (CLAUDE.md §7.5, §15; priv spec chapters 2–4). Phase 1 implements the machine- and
//! supervisor-level registers the riscv-tests environments and Linux user programs touch;
//! Phase 7 completes S-mode (Sv39, interrupts).

use std::time::Instant;

use super::state::CpuState;
use super::trap::prv;

/// `mstatus` fields.
pub mod mstatus {
    pub const SIE: u64 = 1 << 1;
    pub const MIE: u64 = 1 << 3;
    pub const SPIE: u64 = 1 << 5;
    pub const MPIE: u64 = 1 << 7;
    pub const SPP: u64 = 1 << 8;
    pub const MPP_SHIFT: u32 = 11;
    pub const MPP: u64 = 3 << MPP_SHIFT;
    pub const FS_SHIFT: u32 = 13;
    pub const FS: u64 = 3 << FS_SHIFT;
    pub const XS: u64 = 3 << 15;
    pub const MPRV: u64 = 1 << 17;
    pub const SUM: u64 = 1 << 18;
    pub const MXR: u64 = 1 << 19;
    pub const TVM: u64 = 1 << 20;
    pub const TW: u64 = 1 << 21;
    pub const TSR: u64 = 1 << 22;
    /// UXL = SXL = 2 (64-bit), read-only.
    pub const UXL_SXL_64: u64 = (2 << 32) | (2 << 34);
    pub const SD: u64 = 1 << 63;
    /// Bits software can write through `mstatus`.
    pub const WMASK: u64 =
        SIE | MIE | SPIE | MPIE | SPP | MPP | FS | MPRV | SUM | MXR | TVM | TW | TSR;
    /// Bits visible through `sstatus`.
    pub const SSTATUS_RMASK: u64 = SIE | SPIE | SPP | FS | XS | SUM | MXR | (3 << 32) | SD;
    /// Bits writable through `sstatus`.
    pub const SSTATUS_WMASK: u64 = SIE | SPIE | SPP | FS | SUM | MXR;
}

/// FS field values.
pub mod fs {
    pub const OFF: u64 = 0;
    pub const INITIAL: u64 = 1;
    pub const DIRTY: u64 = 3;
}

/// `misa`: MXL = 2 (RV64), extensions A C D F I M S U.
pub const MISA: u64 = (2 << 62)
    | (1 << 0)
    | (1 << 2)
    | (1 << 3)
    | (1 << 5)
    | (1 << 8)
    | (1 << 12)
    | (1 << 18)
    | (1 << 20);

/// Interrupt bits in mip/mie: SSI 1, MSI 3, STI 5, MTI 7, SEI 9, MEI 11.
const MIE_WMASK: u64 = 0xaaa;
/// Supervisor interrupts M-mode may delegate / software may set in mip.
const S_INTS: u64 = 0x222;
/// Delegable synchronous exceptions: causes 0–9, 12, 13, 15 (not ecall-from-M).
const MEDELEG_WMASK: u64 = 0xb3ff;
/// Sv39 translation arrives in Phase 7; until then only Bare may be written to satp.
const SV39_SUPPORTED: bool = false;

/// Supervisor/machine CSR storage. Lives at the end of `CpuState`: JIT code never touches it.
#[derive(Clone, Debug)]
pub struct Csrs {
    pub mstatus: u64,
    pub medeleg: u64,
    pub mideleg: u64,
    pub mie: u64,
    pub mip: u64,
    pub mtvec: u64,
    pub mscratch: u64,
    pub mepc: u64,
    pub mcause: u64,
    pub mtval: u64,
    pub mcounteren: u64,
    pub scounteren: u64,
    pub mcountinhibit: u64,
    pub stvec: u64,
    pub sscratch: u64,
    pub sepc: u64,
    pub scause: u64,
    pub stval: u64,
    pub satp: u64,
    pub pmpcfg: [u64; 8],
    pub pmpaddr: [u64; 64],
    pub mhartid: u64,
    /// `minstret` = icount + instret_offset (writes adjust the offset).
    pub instret_offset: u64,
    /// `mcycle` = icount + cycle_offset.
    pub cycle_offset: u64,
    /// Origin of the `time` CSR (10 MHz, CLAUDE.md §15).
    pub time_origin: Instant,
}

impl Default for Csrs {
    fn default() -> Self {
        Csrs {
            mstatus: 0,
            medeleg: 0,
            mideleg: 0,
            mie: 0,
            mip: 0,
            mtvec: 0,
            mscratch: 0,
            mepc: 0,
            mcause: 0,
            mtval: 0,
            mcounteren: 0,
            scounteren: 0,
            mcountinhibit: 0,
            stvec: 0,
            sscratch: 0,
            sepc: 0,
            scause: 0,
            stval: 0,
            satp: 0,
            pmpcfg: [0; 8],
            pmpaddr: [0; 64],
            mhartid: 0,
            instret_offset: 0,
            cycle_offset: 0,
            time_origin: Instant::now(),
        }
    }
}

/// CSR addresses referenced by name elsewhere.
pub mod addr {
    pub const FFLAGS: u16 = 0x001;
    pub const FRM: u16 = 0x002;
    pub const FCSR: u16 = 0x003;
    pub const CYCLE: u16 = 0xc00;
    pub const TIME: u16 = 0xc01;
    pub const INSTRET: u16 = 0xc02;
}

impl CpuState {
    fn fs(&self) -> u64 {
        (self.csr.mstatus & mstatus::FS) >> mstatus::FS_SHIFT
    }

    /// Mark the FP state dirty (after any write to f registers or fcsr).
    #[inline]
    pub fn set_fs_dirty(&mut self) {
        self.csr.mstatus |= mstatus::FS;
    }

    /// Is the FPU enabled (`mstatus.FS != Off`)?
    #[inline]
    pub fn fp_enabled(&self) -> bool {
        self.fs() != fs::OFF
    }

    fn time_now(&self) -> u64 {
        // 10 MHz timebase: 100 ns per tick.
        (self.csr.time_origin.elapsed().as_nanos() / 100) as u64
    }

    /// Counter-enable check for cycle/time/instret/hpmcounterN (priv spec §3.1.11, §4.1.3).
    fn counter_allowed(&self, csr: u16) -> bool {
        let bit = 1u64 << (csr - 0xc00);
        match self.prv {
            prv::M => true,
            prv::S => self.csr.mcounteren & bit != 0,
            _ => self.csr.mcounteren & self.csr.scounteren & bit != 0,
        }
    }

    /// Common access check: CSR exists (checked by the callers' match), privilege is high
    /// enough, and writes do not target read-only CSRs (csr[11:10] = 3).
    fn csr_access_ok(&self, csr: u16, write: bool) -> bool {
        let min_prv = ((csr >> 8) & 3) as u8;
        if self.prv < min_prv || (write && (csr >> 10) & 3 == 3) {
            return false;
        }
        if csr == 0x180 && self.prv == prv::S && self.csr.mstatus & mstatus::TVM != 0 {
            return false; // satp access traps with TVM
        }
        true
    }

    /// Read a CSR. `None` means the access raises illegal-instruction.
    pub fn csr_read(&self, csr: u16) -> Option<u64> {
        if !self.csr_access_ok(csr, false) {
            return None;
        }
        let s = &self.csr;
        Some(match csr {
            addr::FFLAGS | addr::FRM | addr::FCSR if !self.fp_enabled() => return None,
            addr::FFLAGS => self.fflags as u64,
            addr::FRM => self.frm as u64,
            addr::FCSR => ((self.frm as u64) << 5) | self.fflags as u64,
            0xc00..=0xc1f if !self.counter_allowed(csr) => return None,
            addr::CYCLE => self.icount.wrapping_add(s.cycle_offset),
            addr::TIME => self.time_now(),
            addr::INSTRET => self.icount.wrapping_add(s.instret_offset),
            0xc03..=0xc1f => 0, // hpmcounter3..31: not implemented, read zero
            0x100 => self.mstatus_read() & mstatus::SSTATUS_RMASK,
            0x104 => s.mie & s.mideleg,
            0x105 => s.stvec,
            0x106 => s.scounteren,
            0x10a => 0, // senvcfg: no optional features
            0x140 => s.sscratch,
            0x141 => s.sepc,
            0x142 => s.scause,
            0x143 => s.stval,
            0x144 => s.mip & s.mideleg,
            0x180 => s.satp,
            0x300 => self.mstatus_read(),
            0x301 => MISA,
            0x302 => s.medeleg,
            0x303 => s.mideleg,
            0x304 => s.mie,
            0x305 => s.mtvec,
            0x306 => s.mcounteren,
            0x30a => 0, // menvcfg: no optional features
            0x320 => s.mcountinhibit,
            0x323..=0x33f => 0, // mhpmevent3..31
            0x340 => s.mscratch,
            0x341 => s.mepc,
            0x342 => s.mcause,
            0x343 => s.mtval,
            0x344 => s.mip,
            0x3a0..=0x3af if csr & 1 == 0 => s.pmpcfg[((csr - 0x3a0) / 2) as usize],
            0x3b0..=0x3ef => s.pmpaddr[(csr - 0x3b0) as usize],
            0xb00 => self.icount.wrapping_add(s.cycle_offset),
            0xb02 => self.icount.wrapping_add(s.instret_offset),
            0xb03..=0xb1f => 0,         // mhpmcounter3..31
            0xf11..=0xf13 | 0xf15 => 0, // mvendorid, marchid, mimpid, mconfigptr
            0xf14 => s.mhartid,
            _ => return None,
        })
    }

    fn mstatus_read(&self) -> u64 {
        let v = self.csr.mstatus | mstatus::UXL_SXL_64;
        if self.fs() == fs::DIRTY {
            v | mstatus::SD
        } else {
            v
        }
    }

    /// Write a CSR. `false` means the access raises illegal-instruction. The value is
    /// legalized (WARL) as the priv spec allows.
    pub fn csr_write(&mut self, csr: u16, val: u64) -> bool {
        if !self.csr_access_ok(csr, true) {
            return false;
        }
        if matches!(csr, addr::FFLAGS | addr::FRM | addr::FCSR) {
            if !self.fp_enabled() {
                return false;
            }
            if csr != addr::FRM {
                self.fflags = (val & 0x1f) as u8;
            }
            match csr {
                addr::FRM => self.frm = (val & 7) as u8,
                addr::FCSR => self.frm = ((val >> 5) & 7) as u8,
                _ => {}
            }
            self.set_fs_dirty();
            return true;
        }
        // The writing instruction retires before the new counter value becomes visible.
        let next_icount = self.icount.wrapping_add(1);
        let s = &mut self.csr;
        match csr {
            0x100 => {
                let v = (s.mstatus & !mstatus::SSTATUS_WMASK) | (val & mstatus::SSTATUS_WMASK);
                s.mstatus = v;
            }
            0x104 => s.mie = (s.mie & !s.mideleg) | (val & s.mideleg & MIE_WMASK),
            0x105 => s.stvec = legalize_tvec(val),
            0x106 => s.scounteren = val & 0xffff_ffff,
            0x10a => {}
            0x140 => s.sscratch = val,
            0x141 => s.sepc = val & !1,
            0x142 => s.scause = val,
            0x143 => s.stval = val,
            // Only SSIP is writable through sip.
            0x144 => s.mip = (s.mip & !(s.mideleg & 2)) | (val & s.mideleg & 2),
            0x180 => {
                let mode = val >> 60;
                if mode == 0 || (mode == 8 && SV39_SUPPORTED) {
                    // MODE (4) + ASID (16, all implemented) + PPN (44) cover all 64 bits.
                    s.satp = val;
                }
            }
            0x300 => {
                let mut v = (s.mstatus & !mstatus::WMASK) | (val & mstatus::WMASK);
                // MPP is WARL: the reserved value 2 legalizes to U (as Spike does).
                if (v & mstatus::MPP) >> mstatus::MPP_SHIFT == 2 {
                    v &= !mstatus::MPP;
                }
                s.mstatus = v;
            }
            0x301 => {} // misa is read-only in this implementation (WARL)
            0x302 => s.medeleg = val & MEDELEG_WMASK,
            0x303 => s.mideleg = val & S_INTS,
            0x304 => s.mie = val & MIE_WMASK,
            0x305 => s.mtvec = legalize_tvec(val),
            0x306 => s.mcounteren = val & 0xffff_ffff,
            0x30a => {}
            0x320 => s.mcountinhibit = val & 0xffff_fffd, // bit 1 (TM) is read-only zero
            0x323..=0x33f => {}
            0x340 => s.mscratch = val,
            0x341 => s.mepc = val & !1,
            0x342 => s.mcause = val,
            0x343 => s.mtval = val,
            0x344 => s.mip = (s.mip & !S_INTS) | (val & S_INTS),
            0x3a0..=0x3af if csr & 1 == 0 => s.pmpcfg[((csr - 0x3a0) / 2) as usize] = val,
            0x3b0..=0x3ef => s.pmpaddr[(csr - 0x3b0) as usize] = val & ((1 << 54) - 1),
            0xb00 => s.cycle_offset = val.wrapping_sub(next_icount),
            0xb02 => s.instret_offset = val.wrapping_sub(next_icount),
            0xb03..=0xb1f => {}
            _ => return false,
        }
        true
    }
}

/// xtvec is WARL: MODE 0 (direct) or 1 (vectored); reserved modes become direct.
fn legalize_tvec(val: u64) -> u64 {
    let mode = val & 3;
    (val & !3) | if mode <= 1 { mode } else { 0 }
}
