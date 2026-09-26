//! Floating-point helpers backed by Berkeley SoftFloat 3e: NaN-boxing, canonical NaNs, rounding
//! modes, fflags (CLAUDE.md §17, D13). Phase 1 (P1.11).

use crate::cpu::state::CpuState;
use crate::cpu::trap::Exception;
use crate::isa::inst::Inst;
use crate::mem::direct::DirectMem;

/// Execute an F/D instruction. Until SoftFloat is wired in (P1.11) every FP instruction
/// raises illegal-instruction.
pub fn exec(
    _cpu: &mut CpuState,
    _mem: &mut DirectMem,
    _inst: &Inst,
    raw: u32,
) -> Result<(), Exception> {
    Err(Exception::illegal(raw))
}
