//! Sv39 page-table walker and the softmmu access paths of the interpreter and the JIT's slow
//! paths (CLAUDE.md §14.3, §14.4, D48).
//!
//! With `cpu.softmmu == 0` (user mode, `--mem=direct`) every access goes straight to
//! `DirectMem` at the guest address. With `cpu.softmmu == 1` (bare/system mode, and user mode
//! with `--mem=softmmu`) a virtual address is translated first:
//!
//! * Bare (satp.MODE = 0) or effective M-mode: physical = virtual.
//! * Sv39: the walk below (priv spec §12.3.2), setting A/D itself (as Svadu allows).
//!
//! The physical address is then checked: RAM pages carry `DirectMem` permissions (RWX in
//! system mode; the guest's mmap permissions in user mode, where translation is the identity),
//! other addresses must be an MMIO device. A failed walk is a page fault, a failed physical
//! check an access fault. Translations are cached in the `CpuState` TLB (`tlb.rs`).

use super::direct::DirectMem;
use super::tlb::{self, TLB_MMIO, TlbEntry, idx};
use super::{Access, PAGE_SIZE, prot};
use crate::cpu::csr::mstatus;
use crate::cpu::state::CpuState;
use crate::cpu::trap::{Exception, cause, prv};

/// satp.MODE values.
pub const SATP_BARE: u64 = 0;
pub const SATP_SV39: u64 = 8;
const PPN_MASK: u64 = (1 << 44) - 1;

/// PTE bits.
pub mod pte {
    pub const V: u64 = 1 << 0;
    pub const R: u64 = 1 << 1;
    pub const W: u64 = 1 << 2;
    pub const X: u64 = 1 << 3;
    pub const U: u64 = 1 << 4;
    pub const G: u64 = 1 << 5;
    pub const A: u64 = 1 << 6;
    pub const D: u64 = 1 << 7;
    /// Reserved (60:54), PBMT (62:61) and N (63): must be zero (Svpbmt/Svnapot absent).
    pub const RESERVED: u64 = 0x3ff << 54;
}

fn page_fault(acc: Access, va: u64) -> Exception {
    Exception {
        cause: match acc {
            Access::Load => cause::LOAD_PAGE,
            Access::Store => cause::STORE_PAGE,
            Access::Fetch => cause::INSN_PAGE,
        },
        tval: va,
    }
}

fn access_fault(acc: Access, va: u64) -> Exception {
    Exception {
        cause: match acc {
            Access::Load => cause::LOAD_ACCESS,
            Access::Store => cause::STORE_ACCESS,
            Access::Fetch => cause::INSN_ACCESS,
        },
        tval: va,
    }
}

/// Result of a successful walk: physical page and the permissions (`prot::R/W/X`) this MMU
/// index has on it. W is only granted once D is set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Walk {
    pub ppage: u64,
    pub perms: u8,
}

/// Translate `va` for access `acc` at MMU index `mmu` (priv spec §12.3.2, Sv39).
pub fn walk(
    cpu: &CpuState,
    mem: &mut DirectMem,
    va: u64,
    acc: Access,
    mmu: u8,
) -> Result<Walk, Exception> {
    let satp = cpu.csr.satp;
    if mmu == idx::M || satp >> 60 == SATP_BARE {
        return Ok(Walk {
            ppage: va & !(PAGE_SIZE - 1),
            perms: prot::RWX,
        });
    }
    let pf = || page_fault(acc, va);
    // Bits 63:39 must equal bit 38.
    if (((va << 25) as i64) >> 25) as u64 != va {
        return Err(pf());
    }
    let mut a = (satp & PPN_MASK) << 12;
    let mut level = 2u32;
    let (pte, pte_addr) = loop {
        let pte_addr = a + ((va >> (12 + 9 * level)) & 0x1ff) * 8;
        // A PTE read is a physical access: outside RAM it is an access fault (§12.3.2 step 2).
        let pte = match mem.is_mapped(pte_addr).then(|| mem.load(pte_addr, 8)) {
            Some(Ok(p)) => p,
            _ => return Err(access_fault(acc, va)),
        };
        if pte & pte::V == 0 || (pte & pte::R == 0 && pte & pte::W != 0) || pte & pte::RESERVED != 0
        {
            return Err(pf());
        }
        if pte & (pte::R | pte::X) != 0 {
            break (pte, pte_addr);
        }
        // Non-leaf: D, A and U are reserved and must be zero (as Spike checks).
        if pte & (pte::D | pte::A | pte::U) != 0 || level == 0 {
            return Err(pf());
        }
        level -= 1;
        a = ((pte >> 10) & PPN_MASK) << 12;
    };
    let p = if mmu == idx::U { prv::U } else { prv::S };
    let user = pte & pte::U != 0;
    // U pages: only U-mode, or S-mode data accesses with SUM; S never executes them.
    if (p == prv::U && !user)
        || (p == prv::S && user && (acc == Access::Fetch || mmu != idx::S_SUM))
    {
        return Err(pf());
    }
    let ppn = (pte >> 10) & PPN_MASK;
    let low = (1u64 << (9 * level)) - 1;
    if ppn & low != 0 {
        return Err(pf()); // misaligned superpage
    }
    let mxr = cpu.csr.mstatus & mstatus::MXR != 0;
    let can_r = pte & pte::R != 0 || (mxr && pte & pte::X != 0);
    let can_w = pte & pte::W != 0;
    let can_x = pte & pte::X != 0;
    let ok = match acc {
        Access::Load => can_r,
        Access::Store => can_w,
        Access::Fetch => can_x,
    };
    if !ok {
        return Err(pf());
    }
    let mut pte = pte;
    let want = pte::A | if acc == Access::Store { pte::D } else { 0 };
    if pte & want != want {
        pte |= want;
        if mem.store(pte_addr, 8, pte).is_err() {
            return Err(access_fault(acc, va));
        }
    }
    let mut perms = 0;
    if can_r {
        perms |= prot::R;
    }
    if can_w && pte & pte::D != 0 {
        perms |= prot::W;
    }
    if can_x {
        perms |= prot::X;
    }
    Ok(Walk {
        ppage: (ppn | ((va >> 12) & low)) << 12,
        perms,
    })
}

/// Permissions physical memory grants on `ppage`, and whether it is a device page.
fn phys_perms(mem: &DirectMem, ppage: u64) -> (u8, bool) {
    if mem.is_mapped(ppage) {
        (mem.prot_of(ppage) & prot::RWX, false)
    } else if mem.device_at(ppage).is_some() {
        (prot::R | prot::W, true)
    } else {
        (0, false)
    }
}

/// Translate `va` for `acc` at MMU index `mmu` and fill its TLB entry. Returns the physical
/// address.
pub fn fill(
    cpu: &mut CpuState,
    mem: &mut DirectMem,
    va: u64,
    acc: Access,
    mmu: u8,
) -> Result<u64, Exception> {
    let w = walk(cpu, mem, va, acc, mmu)?;
    let (phys, mmio) = phys_perms(mem, w.ppage);
    let need = match acc {
        Access::Load => prot::R,
        Access::Store => prot::W,
        Access::Fetch => prot::X,
    };
    if phys & need == 0 {
        return Err(access_fault(acc, va));
    }
    let perms = w.perms & phys;
    let vpage = va & !(PAGE_SIZE - 1);
    let flags = if mmio { TLB_MMIO } else { 0 };
    let tag = |p: u8| {
        if perms & p != 0 {
            vpage | flags
        } else {
            tlb::INVALID
        }
    };
    cpu.tlb[mmu as usize][tlb::index(va)] = TlbEntry {
        addr_read: tag(prot::R),
        addr_write: tag(prot::W),
        addr_code: tag(prot::X),
        addend: (mem.base() as u64)
            .wrapping_add(w.ppage)
            .wrapping_sub(vpage),
    };
    Ok(w.ppage | (va & (PAGE_SIZE - 1)))
}

/// Physical address of `va` for `acc` through the TLB (filling it on a miss).
#[inline]
pub fn translate(
    cpu: &mut CpuState,
    mem: &mut DirectMem,
    va: u64,
    acc: Access,
    mmu: u8,
) -> Result<u64, Exception> {
    let e = &cpu.tlb[mmu as usize][tlb::index(va)];
    let tag = match acc {
        Access::Load => e.addr_read,
        Access::Store => e.addr_write,
        Access::Fetch => e.addr_code,
    };
    if tlb::hit(tag, va) {
        return Ok(va.wrapping_add(e.addend).wrapping_sub(mem.base() as u64));
    }
    fill(cpu, mem, va, acc, mmu)
}

fn phys_load(
    mem: &mut DirectMem,
    pa: u64,
    size: u64,
    acc: Access,
    va: u64,
) -> Result<u64, Exception> {
    if mem.is_mapped(pa) {
        return mem.load(pa, size).map_err(|_| access_fault(acc, va));
    }
    mem.mmio_read(pa, size).ok_or_else(|| access_fault(acc, va))
}

fn phys_store(mem: &mut DirectMem, pa: u64, size: u64, val: u64, va: u64) -> Result<(), Exception> {
    if mem.is_mapped(pa) {
        return mem
            .store(pa, size, val)
            .map_err(|_| access_fault(Access::Store, va));
    }
    mem.mmio_write(pa, size, val)
        .ok_or_else(|| access_fault(Access::Store, va))
}

#[inline(always)]
fn crosses_page(va: u64, size: u64) -> bool {
    (va & (PAGE_SIZE - 1)) + size > PAGE_SIZE
}

/// Load `size` bytes at virtual `va` (zero-extended). `acc == Store`: the read side of an AMO,
/// which also needs write permission and faults as a store.
pub fn soft_load(
    cpu: &mut CpuState,
    mem: &mut DirectMem,
    va: u64,
    size: u64,
    acc: Access,
    mmu: u8,
) -> Result<u64, Exception> {
    if !crosses_page(va, size) {
        if acc == Access::Store {
            translate(cpu, mem, va, Access::Store, mmu)?;
        }
        let pa = translate(cpu, mem, va, Access::Load, mmu).map_err(|e| {
            if acc == Access::Store {
                // An AMO's read side faults as a store (priv spec: AMOs raise store faults).
                Exception {
                    cause: if e.cause == cause::LOAD_PAGE {
                        cause::STORE_PAGE
                    } else {
                        cause::STORE_ACCESS
                    },
                    ..e
                }
            } else {
                e
            }
        })?;
        return phys_load(mem, pa, size, acc, va);
    }
    // Misaligned across a page boundary: byte by byte (each byte translated on its own).
    let mut v = 0u64;
    for k in 0..size {
        let a = va.wrapping_add(k);
        let pa = translate(cpu, mem, a, Access::Load, mmu)?;
        v |= phys_load(mem, pa, 1, Access::Load, a)? << (8 * k);
    }
    Ok(v)
}

/// Load `size` (1, 2, 4, 8) bytes at guest virtual `va`, zero-extended.
#[inline(always)]
pub fn load(cpu: &mut CpuState, mem: &mut DirectMem, va: u64, size: u64) -> Result<u64, Exception> {
    if cpu.softmmu == 0 {
        return mem.load(va, size).map_err(Exception::from);
    }
    let mmu = tlb::data_idx(cpu);
    soft_load(cpu, mem, va, size, Access::Load, mmu)
}

/// Load for a read-modify-write (AMO): needs read and write permission, faults as a store.
#[inline(always)]
pub fn load_for_amo(
    cpu: &mut CpuState,
    mem: &mut DirectMem,
    va: u64,
    size: u64,
) -> Result<u64, Exception> {
    if cpu.softmmu == 0 {
        return mem.load_for_amo(va, size).map_err(Exception::from);
    }
    let mmu = tlb::data_idx(cpu);
    soft_load(cpu, mem, va, size, Access::Store, mmu)
}

/// Store the low `size` bytes of `val` at guest virtual `va`.
#[inline(always)]
pub fn store(
    cpu: &mut CpuState,
    mem: &mut DirectMem,
    va: u64,
    size: u64,
    val: u64,
) -> Result<(), Exception> {
    if cpu.softmmu == 0 {
        return mem.store(va, size, val).map_err(Exception::from);
    }
    let mmu = tlb::data_idx(cpu);
    soft_store(cpu, mem, va, size, val, mmu)
}

/// Softmmu store through MMU index `mmu`.
pub fn soft_store(
    cpu: &mut CpuState,
    mem: &mut DirectMem,
    va: u64,
    size: u64,
    val: u64,
    mmu: u8,
) -> Result<(), Exception> {
    if !crosses_page(va, size) {
        let pa = translate(cpu, mem, va, Access::Store, mmu)?;
        return phys_store(mem, pa, size, val, va);
    }
    // Both pages must be writable before any byte is written (a faulting store has no effect).
    let split = PAGE_SIZE - (va & (PAGE_SIZE - 1));
    let hi = va.wrapping_add(split);
    let pa_lo = translate(cpu, mem, va, Access::Store, mmu)?;
    let pa_hi = translate(cpu, mem, hi, Access::Store, mmu)?;
    for k in 0..size {
        let pa = if k < split {
            pa_lo + k
        } else {
            pa_hi + (k - split)
        };
        phys_store(mem, pa, 1, val >> (8 * k), va.wrapping_add(k))?;
    }
    Ok(())
}

/// Fetch an instruction halfword at guest virtual `va`.
#[inline(always)]
pub fn fetch16(cpu: &mut CpuState, mem: &mut DirectMem, va: u64) -> Result<u16, Exception> {
    if cpu.softmmu == 0 {
        return mem.fetch16(va).map_err(Exception::from);
    }
    let mmu = tlb::fetch_idx(cpu);
    let pa = translate(cpu, mem, va, Access::Fetch, mmu)?;
    if !mem.is_mapped(pa) {
        return Err(access_fault(Access::Fetch, va));
    }
    mem.fetch16(pa).map_err(|_| access_fault(Access::Fetch, va))
}

/// Physical page of the instruction at `va` for the current privilege (the JIT keys system-mode
/// translations by it).
pub fn fetch_page(cpu: &mut CpuState, mem: &mut DirectMem, va: u64) -> Result<u64, Exception> {
    if cpu.softmmu == 0 {
        return Ok(0);
    }
    fetch16(cpu, mem, va)?;
    let mmu = tlb::fetch_idx(cpu);
    Ok(translate(cpu, mem, va, Access::Fetch, mmu)? & !(PAGE_SIZE - 1))
}
