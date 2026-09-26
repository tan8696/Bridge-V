//! Architectural CPU state and the parts of the hart that are not memory: CSRs, traps, FP.

pub mod csr;
pub mod fp;
pub mod state;
pub mod trap;
