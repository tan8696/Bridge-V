//! Linux user-mode emulation: process loader, syscall translation, signals (CLAUDE.md §19).

pub mod loader;
pub mod signal;
pub mod syscall;

use std::path::Path;

use anyhow::Result;

use crate::cpu::trap::{Exception, cause};
use crate::interp::{Env, Interp, Stop};
use syscall::{SysOut, Syscalls};

/// Options for `run`.
#[derive(Clone, Copy, Debug, Default)]
pub struct RunOptions {
    pub trace: bool,
    pub strace: bool,
    /// Stop after this many guest instructions (`None` = unlimited).
    pub max_insns: Option<u64>,
}

/// Outcome of a user-mode run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunResult {
    /// Process exit status as a shell would report it (128 + signal for fatal faults).
    pub exit_code: i32,
    pub icount: u64,
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

/// Load and run a static RISC-V Linux executable under the interpreter.
pub fn run(path: &Path, args: &[String], envs: &[String], opts: RunOptions) -> Result<RunResult> {
    let mut p = loader::load(path, args, envs)?;
    let mut interp = Interp::new();
    let mut sys = Syscalls::default();
    sys.strace = opts.strace;
    let env = Env {
        user_mode: true,
        tohost: None,
        trace: opts.trace,
    };
    let limit = opts.max_insns.unwrap_or(u64::MAX);
    loop {
        let left = limit.saturating_sub(p.cpu.icount);
        match interp.run(&mut p.cpu, &mut p.mem, &env, left) {
            Stop::Ecall => match sys.dispatch(&mut p, &mut interp) {
                SysOut::Ret(v) => {
                    p.cpu.x[10] = v as u64;
                    p.cpu.pc += 4; // ECALL has no compressed form
                    p.cpu.icount += 1;
                }
                SysOut::Exit(code) => {
                    return Ok(RunResult {
                        exit_code: code,
                        icount: p.cpu.icount + 1,
                    });
                }
            },
            Stop::Fault(e) => {
                eprintln!("bridgev: guest {e} at pc {:#x}", p.cpu.pc);
                return Ok(RunResult {
                    exit_code: 128 + signal_for(&e),
                    icount: p.cpu.icount,
                });
            }
            Stop::Limit => {
                anyhow::bail!("instruction limit reached ({} instructions)", p.cpu.icount)
            }
            Stop::Tohost(_) => unreachable!("no tohost in user mode"),
        }
    }
}
