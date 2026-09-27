//! Linux user-mode emulation: process loader, syscall translation, signals (CLAUDE.md §19).

pub mod guest_signal;
pub mod loader;
pub mod signal;
pub mod syscall;
pub mod thread;

use std::path::Path;

use anyhow::Result;

use crate::cpu::trap::{Exception, cause};
use crate::interp::Env;
use crate::jit::{EngineKind, JitOptions};

/// Options for `run`.
#[derive(Clone, Debug, Default)]
pub struct RunOptions {
    pub trace: bool,
    pub strace: bool,
    /// Stop after this many guest instructions (`None` = unlimited).
    pub max_insns: Option<u64>,
    pub engine: EngineKind,
    pub jit: JitOptions,
    /// `--deterministic`: the `time` CSR follows icount instead of the host clock.
    pub deterministic: bool,
    /// `--stats=regs`: collect the register-use histogram (interpreter engine only).
    pub reg_stats: bool,
    /// `--mem=softmmu`: translate every access through the software TLB (D48).
    pub softmmu: bool,
    /// `--sysroot`: where the program interpreter and absolute paths are looked up first.
    pub sysroot: Option<std::path::PathBuf>,
}

/// Outcome of a user-mode run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunResult {
    /// Process exit status as a shell would report it (128 + signal for fatal faults).
    pub exit_code: i32,
    pub icount: u64,
    /// Engine statistics (`--stats`).
    pub engine_stats: String,
}

/// Host signal number a fatal guest exception maps to (Linux delivers these on RISC-V).
fn signal_for(e: &Exception) -> i32 {
    match e.cause {
        cause::ILLEGAL_INSN => libc::SIGILL,
        cause::BREAKPOINT => libc::SIGTRAP,
        cause::LOAD_MISALIGNED | cause::STORE_MISALIGNED | cause::INSN_MISALIGNED => libc::SIGBUS,
        _ => libc::SIGSEGV,
    }
}

/// Load and run a static RISC-V Linux executable.
pub fn run(path: &Path, args: &[String], envs: &[String], opts: RunOptions) -> Result<RunResult> {
    let mut p = loader::load(path, args, envs, opts.sysroot.as_deref())?;
    p.cpu.csr.deterministic_time = opts.deterministic;
    // 2 = flat: translation is the identity and only changes with a TLB flush (D48).
    p.cpu.softmmu = if opts.softmmu { 2 } else { 0 };
    let env = Env {
        user_mode: true,
        tohost: None,
        trace: opts.trace,
        sbi: false,
    };
    let strace = opts.strace;
    thread::run_process(p, opts, env, strace)
}
