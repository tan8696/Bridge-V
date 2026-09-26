//! Bare-metal harness for the riscv-tests `p` environment (P1.13, CLAUDE.md §21 item 3).
//!
//! The ELF is loaded at its physical link address into a RAM region at 0x8000_0000, the hart
//! starts in M-mode at the entry point, and the run ends when the guest writes the HTIF
//! `tohost` word: 1 means pass, `(n << 1) | 1` means test case `n` failed.

use anyhow::{Context, Result, bail};

use crate::cpu::state::CpuState;
use crate::elf::Elf;
use crate::interp::{Env, Stop};
use crate::jit::{EngineKind, JitOptions, make_engine};
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

/// Options for `run`.
#[derive(Clone, Debug)]
pub struct BareOptions {
    pub max_insns: u64,
    pub trace: bool,
    pub engine: EngineKind,
    pub jit: JitOptions,
    /// `--stats=regs`: collect the register-use histogram (interpreter engine only).
    pub reg_stats: bool,
}

impl Default for BareOptions {
    fn default() -> Self {
        BareOptions {
            max_insns: 100_000_000,
            trace: false,
            engine: EngineKind::Interp,
            jit: JitOptions::default(),
            reg_stats: false,
        }
    }
}

/// Outcome of `run`.
#[derive(Clone, Debug)]
pub struct BareRun {
    pub result: BareResult,
    /// Retired instructions.
    pub icount: u64,
    pub engine_stats: String,
}

/// Load `elf_bytes` and run it to completion (or `max_insns`).
pub fn run(elf_bytes: &[u8], opts: &BareOptions) -> Result<BareRun> {
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
    // Bare metal is system mode: every access is translated (Bare/Sv39, D48).
    cpu.softmmu = 1;
    let env = Env {
        user_mode: false,
        tohost: Some(tohost),
        trace: opts.trace,
    };
    let mut engine = make_engine(opts.engine, &opts.jit)?;
    if opts.reg_stats && !engine.enable_reg_stats() {
        bail!("--stats=regs needs --engine interp");
    }
    let result = match engine.run(&mut cpu, &mut mem, &env, opts.max_insns) {
        Stop::Tohost(1) => BareResult::Pass,
        Stop::Tohost(v) => BareResult::Fail(v >> 1),
        Stop::Limit => BareResult::Timeout,
        other => bail!("unexpected stop in bare mode: {other:?}"),
    };
    Ok(BareRun {
        result,
        icount: cpu.icount,
        engine_stats: engine.stats(),
    })
}
