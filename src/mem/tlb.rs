//! Direct-mapped software TLB per MMU index, with fill, flush and flag bits (CLAUDE.md §14.4,
//! D10, D48). The TLB lives in `CpuState` so JIT code can probe it inline:
//!
//! ```text
//!   entry = cpu.tlb[mmu_idx][(va >> 12) & (TLB_SIZE - 1)]
//!   hit   = entry.addr_read == (va & !0xfff) | (va & (size - 1))     (addr_write, addr_code)
//!   host  = va + entry.addend
//! ```
//!
//! A tag is the virtual page, or `INVALID`. Flag bits 3–11 sit between the largest alignment
//! mask (7) and the page offset: the compared value always has them clear, so a set flag forces
//! the slow path (MMIO pages, and Phase 8's code pages on `addr_write`). Permissions,
//! privilege, SUM/MXR and the dirty bit are folded in at fill time: a tag is only valid for an
//! access kind the fill found allowed (`addr_write` also needs D = 1, so the first store to a
//! clean page walks and sets D).

use crate::cpu::csr::mstatus;
use crate::cpu::state::CpuState;
use crate::cpu::trap::prv;

/// log2 of the number of entries per MMU index.
pub const TLB_BITS: u32 = 8;
pub const TLB_SIZE: usize = 1 << TLB_BITS;

/// MMU indices (D48): one TLB each. S with SUM = 1 is separate from S so Linux's frequent
/// SUM toggling needs no flush; M is also used for Bare translation.
pub mod idx {
    pub const U: u8 = 0;
    pub const S: u8 = 1;
    pub const S_SUM: u8 = 2;
    pub const M: u8 = 3;
}
pub const NB_MMU_IDX: usize = 4;

/// Device page: every access takes the slow path.
pub const TLB_MMIO: u64 = 1 << 3;
/// Page holds translated code (Phase 8, `addr_write` only).
pub const TLB_CODE: u64 = 1 << 4;
/// Flag bits that may be set in a valid tag.
pub const TLB_FLAGS: u64 = TLB_MMIO | TLB_CODE;
/// An invalid tag: its low bits never match a compared value.
pub const INVALID: u64 = u64::MAX;

/// One TLB entry (32 bytes, JIT-visible).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TlbEntry {
    pub addr_read: u64,
    pub addr_write: u64,
    pub addr_code: u64,
    /// Host address of the page minus its virtual address (`host = va + addend`). For MMIO
    /// pages, `va + addend - mem_base` is still the physical address.
    pub addend: u64,
}

impl TlbEntry {
    pub const EMPTY: TlbEntry = TlbEntry {
        addr_read: INVALID,
        addr_write: INVALID,
        addr_code: INVALID,
        addend: 0,
    };
}

const _: () = assert!(std::mem::size_of::<TlbEntry>() == 32);

/// TLB slot of virtual address `va`.
#[inline(always)]
pub fn index(va: u64) -> usize {
    (va >> 12) as usize & (TLB_SIZE - 1)
}

/// Does `tag` hit for the page of `va` (flags ignored)?
#[inline(always)]
pub fn hit(tag: u64, va: u64) -> bool {
    tag & !TLB_FLAGS == va & !0xfff
}

/// Privilege that data accesses use: MPP when M-mode sets MPRV (priv spec §3.1.6.3).
#[inline]
pub fn data_prv(cpu: &CpuState) -> u8 {
    let ms = cpu.csr.mstatus;
    if cpu.prv == prv::M && ms & mstatus::MPRV != 0 {
        ((ms & mstatus::MPP) >> mstatus::MPP_SHIFT) as u8
    } else {
        cpu.prv
    }
}

/// MMU index of data accesses (loads, stores, AMOs).
#[inline]
pub fn data_idx(cpu: &CpuState) -> u8 {
    match data_prv(cpu) {
        prv::M => idx::M,
        prv::S if cpu.csr.mstatus & mstatus::SUM != 0 => idx::S_SUM,
        prv::S => idx::S,
        _ => idx::U,
    }
}

/// MMU index of instruction fetch (SUM never applies to execution).
#[inline]
pub fn fetch_idx(cpu: &CpuState) -> u8 {
    match cpu.prv {
        prv::M => idx::M,
        prv::S => idx::S,
        _ => idx::U,
    }
}

/// Invalidate every entry of every MMU index, and tell the engines that virtual-to-physical
/// translation may have changed (their virtually keyed caches must be revalidated).
pub fn flush_all(cpu: &mut CpuState) {
    for t in cpu.tlb.iter_mut() {
        t.fill(TlbEntry::EMPTY);
    }
    cpu.tlb_super = [u64::MAX, 0];
    cpu.mmu_gen = cpu.mmu_gen.wrapping_add(1);
    cpu.jc_gen = cpu.jc_gen.wrapping_add(1);
}

/// SFENCE.VMA with an address (priv spec §12.2.1; ASIDs are ignored): drop the entries of the
/// page of `va` in every MMU index and its jump-cache entries (D51). Everything is flushed
/// instead if `va` lies in a superpage that may be cached. `mmu_gen` still moves, so the
/// interpreter's decoded blocks are revalidated.
pub fn flush_page(cpu: &mut CpuState, va: u64) {
    let [lo, hi] = cpu.tlb_super;
    if (lo..=hi).contains(&va) {
        return flush_all(cpu);
    }
    let i = index(va);
    for t in cpu.tlb.iter_mut() {
        let e = &mut t[i];
        if [e.addr_read, e.addr_write, e.addr_code]
            .iter()
            .any(|&tag| tag != INVALID && hit(tag, va))
        {
            *e = TlbEntry::EMPTY;
        }
    }
    cpu.clear_jump_cache_page(va);
    cpu.mmu_gen = cpu.mmu_gen.wrapping_add(1);
}

/// Set (`on`) or clear the `TLB_CODE` flag on every write tag that maps physical page `page`
/// (D49): stores to a page holding translated code must take the slow path. `mem_base` is the
/// host address of physical 0 (`DirectMem::base`).
pub fn set_code_flag(cpu: &mut CpuState, mem_base: u64, page: u64, on: bool) {
    for t in cpu.tlb.iter_mut() {
        for e in t.iter_mut() {
            if e.addr_write == INVALID {
                continue;
            }
            let vpage = e.addr_write & !0xfff;
            if vpage.wrapping_add(e.addend).wrapping_sub(mem_base) == page {
                if on {
                    e.addr_write |= TLB_CODE;
                } else {
                    e.addr_write &= !TLB_CODE;
                }
            }
        }
    }
}
