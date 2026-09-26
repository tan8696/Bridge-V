//! `#[repr(C)] CpuState`, the structure JIT code addresses through RBP (CLAUDE.md §8.1).
//! Every JIT-visible offset is pinned with a compile-time `offset_of!` assert. Phase 1 (P1.7).
