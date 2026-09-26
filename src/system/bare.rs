//! Bare-metal harness for the riscv-tests `p` environment (P1.13, CLAUDE.md §21 item 3).
//!
//! The ELF is loaded at its physical link address into a RAM region at 0x8000_0000, the hart
//! starts in M-mode at the entry point, and the run ends when the guest writes the HTIF
//! `tohost` word: 1 means pass, `(n << 1) | 1` means test case `n` failed.

use anyhow::{Context, Result, bail};

use crate::cpu::state::CpuState;
use crate::elf::Elf;
use crate::interp::{Env, Interp, Stop};
use crate::mem::direct::DirectMem;
use crate::mem::{GuestVirt, prot};

pub const RAM_BASE: u64 = 0x8000_0000;
pub const RAM_SIZE: u64 = 32 << 20;

/// Outcome of one bare-metal test run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BareResult {
    Pass,
    /// Failing test case number (`tohost >> 1`).
    Fail(u64),
    /// No `tohost` write within the instruction limit.
    Timeout,
}

/// Load `elf_bytes` and run it to completion (or `max_insns`). Returns the result and the
/// number of retired instructions.
pub fn run(elf_bytes: &[u8], max_insns: u64, trace: bool) -> Result<(BareResult, u64)> {
    let elf = Elf::parse(elf_bytes)?;
    let tohost = elf.symbol("tohost").context("ELF has no `tohost` symbol")?;
    let mut mem = DirectMem::new()?;
    mem.map(GuestVirt(RAM_BASE), RAM_SIZE, prot::RWX)?;
    for seg in elf.loads() {
        if seg.paddr < RAM_BASE || seg.paddr + seg.memsz > RAM_BASE + RAM_SIZE {
            bail!("segment at {:#x} is outside RAM", seg.paddr);
        }
        mem.write_bytes(GuestVirt(seg.paddr), elf.segment_data(seg))
            .map_err(|f| anyhow::anyhow!("loading segment: {f:?}"))?;
    }
    let mut cpu = CpuState::new_machine(elf.entry);
    let env = Env {
        user_mode: false,
        tohost: Some(tohost),
        trace,
    };
    let mut interp = Interp::new();
    let result = match interp.run(&mut cpu, &mut mem, &env, max_insns) {
        Stop::Tohost(1) => BareResult::Pass,
        Stop::Tohost(v) => BareResult::Fail(v >> 1),
        Stop::Limit => BareResult::Timeout,
        other => bail!("unexpected stop in bare mode: {other:?}"),
    };
    Ok((result, cpu.icount))
}
