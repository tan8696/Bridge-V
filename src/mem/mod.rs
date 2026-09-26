//! Guest memory: the direct user-mode backend, physical memory bus, SV39 MMU, software TLB and
//! self-modifying-code tracking (CLAUDE.md §14, §16).

pub mod direct;
pub mod mmu;
pub mod phys;
pub mod smc;
pub mod tlb;

/// A guest virtual address. Used at API boundaries where guest addresses meet host pointers
/// (mapping, host slices), so the two can never be confused (CLAUDE.md §25).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GuestVirt(pub u64);

/// A guest physical address (system mode).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GuestPhys(pub u64);

/// Guest page permissions.
pub mod prot {
    pub const R: u8 = 1;
    pub const W: u8 = 2;
    pub const X: u8 = 4;
    pub const RW: u8 = R | W;
    pub const RWX: u8 = R | W | X;
}

pub const PAGE_SIZE: u64 = 4096;

/// Round `x` down / up to a page boundary.
pub fn page_floor(x: u64) -> u64 {
    x & !(PAGE_SIZE - 1)
}
pub fn page_ceil(x: u64) -> u64 {
    x.wrapping_add(PAGE_SIZE - 1) & !(PAGE_SIZE - 1)
}

/// Why a guest memory access failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Load,
    Store,
    Fetch,
}

/// A failed guest access: kind and faulting guest address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemFault {
    pub access: Access,
    pub addr: u64,
}
