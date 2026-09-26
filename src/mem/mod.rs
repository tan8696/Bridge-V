//! Guest memory: the direct user-mode backend, physical memory bus, SV39 MMU, software TLB and
//! self-modifying-code tracking (CLAUDE.md §14, §16).

pub mod direct;
pub mod mmu;
pub mod phys;
pub mod smc;
pub mod tlb;
