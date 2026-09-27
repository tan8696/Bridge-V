//! Linux user-mode emulation: process loader, syscall translation, signals (CLAUDE.md §19).

pub mod loader;
pub mod signal;
pub mod syscall;

use std::path::Path;

use anyhow::Result;

use crate::cpu::trap::{Exception, cause};
use crate::interp::{Env, Stop};
use crate::jit::{EngineKind, JitOptions, make_engine};
use crate::mem::tlb;
use syscall::{SysOut, Syscalls};

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
    let mut p = loader::load(path, args, envs)?;
    p.cpu.csr.deterministic_time = opts.deterministic;
    // 2 = flat: translation is the identity and only changes with a TLB flush (D48).
    p.cpu.softmmu = if opts.softmmu { 2 } else { 0 };
    let mut engine = make_engine(opts.engine, &opts.jit)?;
    if opts.reg_stats && !engine.enable_reg_stats() {
        anyhow::bail!("--stats=regs needs --engine interp");
    }
    let mut sys = Syscalls::default();
    sys.strace = opts.strace;
    let env = Env {
        user_mode: true,
        tohost: None,
        trace: opts.trace,
    };
    let limit = opts.max_insns.unwrap_or(u64::MAX);
    let stats = |engine: &dyn crate::interp::Engine, cpu: &crate::cpu::state::CpuState| {
        let mut s = engine.stats();
        if cpu.softmmu != 0 {
            s += &format!("\nsoftmmu: {} TLB fills", cpu.tlb_fills);
        }
        s
    };
    loop {
        let left = limit.saturating_sub(p.cpu.icount);
        match engine.run(&mut p.cpu, &mut p.mem, &env, left) {
            Stop::Ecall => match sys.dispatch(&mut p, engine.as_mut()) {
                SysOut::Ret(v) => {
                    // The user-mode "page table" is the mmap state: drop cached translations
                    // when it changes (brk, munmap, mremap, mmap, mprotect).
                    if p.cpu.softmmu != 0 && matches!(p.cpu.x[17], 214 | 215 | 216 | 222 | 226) {
                        tlb::flush_all(&mut p.cpu);
                    }
                    p.cpu.x[10] = v as u64;
                    p.cpu.pc += 4; // ECALL has no compressed form
                    p.cpu.icount += 1;
                }
                SysOut::Exit(code) => {
                    return Ok(RunResult {
                        exit_code: code,
                        icount: p.cpu.icount + 1,
                        engine_stats: stats(engine.as_ref(), &p.cpu),
                    });
                }
            },
            Stop::Fault(e) => {
                eprintln!("bridgev: guest {e} at pc {:#x}", p.cpu.pc);
                return Ok(RunResult {
                    exit_code: 128 + signal_for(&e),
                    icount: p.cpu.icount,
                    engine_stats: stats(engine.as_ref(), &p.cpu),
                });
            }
            Stop::Limit => {
                anyhow::bail!("instruction limit reached ({} instructions)", p.cpu.icount)
            }
            Stop::Diverged => anyhow::bail!("lockstep divergence (see above)"),
            Stop::Tohost(_) => unreachable!("no tohost in user mode"),
        }
    }
}
