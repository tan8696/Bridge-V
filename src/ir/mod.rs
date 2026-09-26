//! Per-block intermediate representation and its passes (CLAUDE.md §9, Phase 4).

pub mod eval;
pub mod lift;
pub mod liveness;
pub mod ops;
pub mod opt;
