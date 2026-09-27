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
    cpu.tlb_fills += 1;
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

#[cfg(test)]
mod tests {
    //! Walker matrix (P7.4, §21 item 8): hand-built Sv39 tables in RAM at 0x8000_0000.
    use super::*;
    use crate::mem::GuestVirt;
    use crate::mem::phys::Mmio;

    const RAM: u64 = 0x8000_0000;
    const ROOT: u64 = RAM + 0x1000;
    const L1: u64 = RAM + 0x2000;
    const L0: u64 = RAM + 0x3000;
    const R: u64 = pte::R;
    const W: u64 = pte::W;
    const X: u64 = pte::X;
    const U: u64 = pte::U;
    const AD: u64 = pte::A | pte::D;

    fn leaf(pa: u64, flags: u64) -> u64 {
        (pa >> 12) << 10 | flags | pte::V
    }
    fn table(pa: u64) -> u64 {
        (pa >> 12) << 10 | pte::V
    }
    fn vpn(va: u64, level: u32) -> u64 {
        (va >> (12 + 9 * level)) & 0x1ff
    }

    struct Dev;
    impl Mmio for Dev {
        fn name(&self) -> &str {
            "test"
        }
        fn read(&mut self, off: u64, size: u64) -> u64 {
            0x1000 + off * 16 + size
        }
        fn write(&mut self, _: u64, _: u64, _: u64) {}
    }

    /// RAM (1 MiB) + an MMIO device at 0x1000_0000, S-mode, Sv39 with root at ROOT.
    fn setup() -> (Box<CpuState>, DirectMem) {
        let mut mem = DirectMem::new().unwrap();
        mem.map(GuestVirt(RAM), 1 << 20, prot::RWX).unwrap();
        mem.add_device(0x1000_0000, 0x1000, Box::new(Dev));
        let mut cpu = CpuState::new_machine(0);
        cpu.softmmu = 1;
        cpu.prv = prv::S;
        cpu.csr.satp = SATP_SV39 << 60 | ROOT >> 12;
        (cpu, mem)
    }

    fn put(mem: &mut DirectMem, a: u64, v: u64) {
        mem.store(a, 8, v).unwrap();
    }

    /// Map a 4 KiB page va → pa through ROOT → L1 → L0.
    fn map4k(mem: &mut DirectMem, va: u64, pa: u64, flags: u64) {
        put(mem, ROOT + vpn(va, 2) * 8, table(L1));
        put(mem, L1 + vpn(va, 1) * 8, table(L0));
        put(mem, L0 + vpn(va, 0) * 8, leaf(pa, flags));
    }

    fn cause_of(r: Result<Walk, Exception>) -> u64 {
        r.expect_err("expected a fault").cause
    }

    #[test]
    fn page_sizes_4k_2m_1g() {
        let (cpu, mut mem) = setup();
        map4k(&mut mem, 0x4000_1000, RAM + 0x5_0000, R | W | AD);
        // 2 MiB megapage at VA 0x4020_0000 (level 1) → PA RAM + 0x20_0000 (2 MiB aligned).
        put(&mut mem, L1 + vpn(0x4020_0000, 1) * 8, leaf(RAM, R | AD));
        // 1 GiB gigapage at VA 0x8000_0000 (level 2) → PA 0x8000_0000.
        put(&mut mem, ROOT + 2 * 8, leaf(RAM, R | X | AD));
        let w = walk(&cpu, &mut mem, 0x4000_1abc, Access::Load, idx::S).unwrap();
        assert_eq!(w.ppage, RAM + 0x5_0000);
        assert_eq!(w.perms, prot::R | prot::W);
        let w = walk(&cpu, &mut mem, 0x4031_2345, Access::Load, idx::S).unwrap();
        assert_eq!(
            w.ppage,
            RAM + 0x11_2000,
            "megapage: low VPN bits come from the VA"
        );
        let w = walk(&cpu, &mut mem, 0xbfff_f123, Access::Fetch, idx::S).unwrap();
        assert_eq!(w.ppage, RAM + 0x3fff_f000);
        assert_eq!(w.perms, prot::R | prot::X);
    }

    #[test]
    fn misaligned_superpages_fault() {
        let (cpu, mut mem) = setup();
        put(&mut mem, ROOT + vpn(0x4000_0000, 2) * 8, table(L1));
        put(&mut mem, L1, leaf(RAM + 0x1000, R | AD)); // 2 MiB leaf, ppn[0] != 0
        put(&mut mem, ROOT + 3 * 8, leaf(RAM + 0x20_0000, R | AD)); // 1 GiB leaf, ppn[1] != 0
        assert_eq!(
            cause_of(walk(&cpu, &mut mem, 0x4000_0000, Access::Load, idx::S)),
            cause::LOAD_PAGE
        );
        assert_eq!(
            cause_of(walk(&cpu, &mut mem, 0xc000_0000, Access::Store, idx::S)),
            cause::STORE_PAGE
        );
    }

    #[test]
    fn user_sum_mxr_and_privilege() {
        let (mut cpu, mut mem) = setup();
        let (upage, spage, xpage) = (0x1000, 0x2000, 0x3000);
        map4k(&mut mem, upage, RAM + 0x6_0000, R | W | X | U | AD);
        map4k(&mut mem, spage, RAM + 0x7_0000, R | W | X | AD);
        map4k(&mut mem, xpage, RAM + 0x8_0000, X | AD);
        let ok = |cpu: &CpuState, mem: &mut DirectMem, va, acc, mmu| {
            walk(cpu, mem, va, acc, mmu).is_ok()
        };
        for acc in [Access::Load, Access::Store, Access::Fetch] {
            assert!(ok(&cpu, &mut mem, upage, acc, idx::U), "U on a U page");
            assert!(!ok(&cpu, &mut mem, spage, acc, idx::U), "U on an S page");
            assert!(
                !ok(&cpu, &mut mem, upage, acc, idx::S),
                "S on a U page without SUM"
            );
            assert!(ok(&cpu, &mut mem, spage, acc, idx::S), "S on an S page");
            assert!(
                ok(&cpu, &mut mem, spage, acc, idx::S_SUM),
                "S+SUM on an S page"
            );
        }
        // SUM permits data accesses to U pages, never execution.
        assert!(ok(&cpu, &mut mem, upage, Access::Load, idx::S_SUM));
        assert!(ok(&cpu, &mut mem, upage, Access::Store, idx::S_SUM));
        assert!(!ok(&cpu, &mut mem, upage, Access::Fetch, idx::S_SUM));
        // Execute-only: loads need MXR.
        assert!(!ok(&cpu, &mut mem, xpage, Access::Load, idx::S));
        assert!(ok(&cpu, &mut mem, xpage, Access::Fetch, idx::S));
        cpu.csr.mstatus |= mstatus::MXR;
        assert!(ok(&cpu, &mut mem, xpage, Access::Load, idx::S));
        assert!(!ok(&cpu, &mut mem, xpage, Access::Store, idx::S));
        // M-mode and Bare translate nothing.
        assert_eq!(
            walk(&cpu, &mut mem, 0x1234_5678, Access::Store, idx::M)
                .unwrap()
                .ppage,
            0x1234_5000
        );
        cpu.csr.satp = 0;
        assert_eq!(
            walk(&cpu, &mut mem, 0x1234_5678, Access::Fetch, idx::U)
                .unwrap()
                .ppage,
            0x1234_5000
        );
    }

    #[test]
    fn mprv_uses_mpp_for_data_only() {
        let (mut cpu, mut mem) = setup();
        map4k(&mut mem, 0x1000, RAM + 0x6_0000, R | W | U | AD);
        cpu.prv = prv::M;
        cpu.csr.mstatus |= mstatus::MPRV; // MPP = U (0)
        assert_eq!(tlb::data_idx(&cpu), idx::U);
        assert_eq!(tlb::fetch_idx(&cpu), idx::M);
        assert_eq!(load(&mut cpu, &mut mem, 0x1008, 8).unwrap(), 0);
        mem.store(RAM + 0x6_0008, 8, 0xabcd).unwrap();
        assert_eq!(load(&mut cpu, &mut mem, 0x1008, 8).unwrap(), 0xabcd);
        cpu.csr.mstatus |= 1 << mstatus::MPP_SHIFT; // MPP = S: the U page needs SUM
        assert_eq!(
            load(&mut cpu, &mut mem, 0x1008, 8).unwrap_err().cause,
            cause::LOAD_PAGE
        );
    }

    #[test]
    fn accessed_and_dirty_bits() {
        let (mut cpu, mut mem) = setup();
        map4k(&mut mem, 0x5000, RAM + 0x9_0000, R | W);
        let slot = L0 + vpn(0x5000, 0) * 8;
        assert_eq!(load(&mut cpu, &mut mem, 0x5000, 4).unwrap(), 0);
        assert_eq!(
            mem.load(slot, 8).unwrap() & AD,
            pte::A,
            "a load sets A only"
        );
        // The fill after a load of a clean page grants no write: the store walks and sets D.
        assert_eq!(
            cpu.tlb[idx::S as usize][tlb::index(0x5000)].addr_write,
            tlb::INVALID
        );
        store(&mut cpu, &mut mem, 0x5004, 4, 7).unwrap();
        assert_eq!(mem.load(slot, 8).unwrap() & AD, AD);
        assert_eq!(
            cpu.tlb[idx::S as usize][tlb::index(0x5000)].addr_write,
            0x5000
        );
        assert_eq!(mem.load(RAM + 0x9_0004, 4).unwrap(), 7);
    }

    #[test]
    fn invalid_ptes_and_non_canonical_addresses() {
        let (cpu, mut mem) = setup();
        let va = 0x7000;
        let cases: &[(u64, &str)] = &[
            (leaf(RAM, R | AD) & !pte::V, "V = 0"),
            (leaf(RAM, W | AD), "W without R"),
            (leaf(RAM, R | AD) | 1 << 54, "reserved bit 54"),
            (leaf(RAM, R | AD) | 1 << 61, "PBMT"),
            (leaf(RAM, R | AD) | 1 << 63, "N"),
            (table(L0), "pointer at level 0"),
        ];
        for &(p, why) in cases {
            map4k(&mut mem, va, RAM, R);
            put(&mut mem, L0 + vpn(va, 0) * 8, p);
            put(&mut mem, L0, table(L0)); // for "pointer at level 0": points back at L0
            assert_eq!(
                cause_of(walk(&cpu, &mut mem, va, Access::Load, idx::S)),
                cause::LOAD_PAGE,
                "{why}"
            );
        }
        // Non-leaf entries with A, D or U set are reserved.
        for bit in [pte::A, pte::D, pte::U] {
            map4k(&mut mem, va, RAM, R | AD);
            put(&mut mem, L1 + vpn(va, 1) * 8, table(L0) | bit);
            assert_eq!(
                cause_of(walk(&cpu, &mut mem, va, Access::Fetch, idx::S)),
                cause::INSN_PAGE
            );
        }
        // Bits 63:39 must copy bit 38.
        for bad in [
            1u64 << 39,
            0x0000_8000_0000_0000,
            0xffff_ff00_0000_0000 ^ (1 << 38),
        ] {
            assert_eq!(
                cause_of(walk(&cpu, &mut mem, bad, Access::Store, idx::S)),
                cause::STORE_PAGE
            );
        }
    }

    #[test]
    fn pte_outside_ram_is_an_access_fault() {
        let (mut cpu, mut mem) = setup();
        cpu.csr.satp = SATP_SV39 << 60 | 0x4000_0000 >> 12; // root table not in RAM
        assert_eq!(
            cause_of(walk(&cpu, &mut mem, 0x1000, Access::Load, idx::S)),
            cause::LOAD_ACCESS
        );
        assert_eq!(
            cause_of(walk(&cpu, &mut mem, 0x1000, Access::Fetch, idx::S)),
            cause::INSN_ACCESS
        );
    }

    #[test]
    fn mmio_and_unbacked_physical_addresses() {
        let (mut cpu, mut mem) = setup();
        map4k(&mut mem, 0x8000, 0x1000_0000, R | W | X | AD); // device page
        map4k(&mut mem, 0x9000, 0x2000_0000, R | W | X | AD); // nothing there
        assert_eq!(
            load(&mut cpu, &mut mem, 0x8010, 4).unwrap(),
            0x1000 + 0x10 * 16 + 4
        );
        let e = &cpu.tlb[idx::S as usize][tlb::index(0x8000)];
        assert_eq!(
            e.addr_read,
            0x8000 | TLB_MMIO,
            "device pages always take the slow path"
        );
        store(&mut cpu, &mut mem, 0x8000, 8, 1).unwrap();
        assert_eq!(
            fetch16(&mut cpu, &mut mem, 0x8000).unwrap_err().cause,
            cause::INSN_ACCESS
        );
        assert_eq!(
            load(&mut cpu, &mut mem, 0x9000, 1).unwrap_err().cause,
            cause::LOAD_ACCESS
        );
        assert_eq!(
            store(&mut cpu, &mut mem, 0x9000, 1, 0).unwrap_err().cause,
            cause::STORE_ACCESS
        );
    }

    #[test]
    fn page_crossing_accesses_split_and_fault_atomically() {
        let (mut cpu, mut mem) = setup();
        map4k(&mut mem, 0xa000, RAM + 0xa_0000, R | W | AD);
        map4k(&mut mem, 0xb000, RAM + 0x5_0000, R | AD); // read-only, not contiguous
        mem.store(RAM + 0xa_0ffc, 4, 0x4433_2211).unwrap();
        mem.store(RAM + 0x5_0000, 4, 0x8877_6655).unwrap();
        assert_eq!(
            load(&mut cpu, &mut mem, 0xaffc, 8).unwrap(),
            0x8877_6655_4433_2211
        );
        // A store into the read-only second page faults there and writes nothing.
        let e = store(&mut cpu, &mut mem, 0xaffe, 4, u64::MAX).unwrap_err();
        assert_eq!((e.cause, e.tval), (cause::STORE_PAGE, 0xb000));
        assert_eq!(mem.load(RAM + 0xa_0ffc, 4).unwrap(), 0x4433_2211);
    }
}
