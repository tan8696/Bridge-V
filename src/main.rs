//! `bridgev` command-line interface (CLAUDE.md §23).

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};

use bridgev::system::bare::{self, BareResult};

#[derive(Parser)]
#[command(
    name = "bridgev",
    version,
    about = "RISC-V RV64GC → x86-64 dynamic binary translator"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Mode {
    /// Linux user-mode emulation (syscalls translated to the host).
    User,
    /// Bare-metal M-mode with HTIF `tohost` (riscv-tests `p` environment).
    Bare,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Engine {
    /// Reference interpreter.
    Interp,
    /// JIT translator (Phase 2).
    Jit,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Trace {
    /// Print every executed instruction.
    Insn,
}

#[derive(Subcommand)]
enum Command {
    /// Run a RISC-V ELF executable.
    Run {
        #[arg(long, value_enum, default_value = "user")]
        mode: Mode,
        #[arg(long, value_enum, default_value = "interp")]
        engine: Engine,
        /// Stop after this many guest instructions (bare mode default: 100M).
        #[arg(long)]
        max_insns: Option<u64>,
        /// Execution trace on stderr.
        #[arg(long, value_enum)]
        trace: Option<Trace>,
        /// Print execution statistics on stderr.
        #[arg(long)]
        stats: bool,
        /// Guest executable.
        elf: PathBuf,
        /// Arguments passed to the guest program.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Boot a RISC-V Linux kernel in system mode.
    Boot {
        /// Kernel `Image` file.
        #[arg(long)]
        kernel: PathBuf,
    },
    /// Disassemble the executable segments of a RISC-V ELF.
    Disasm {
        /// Guest executable.
        elf: PathBuf,
    },
    /// Run a benchmark suite across engine configurations.
    Bench {
        /// Suite name (e.g. `coremark`).
        suite: String,
    },
}

/// Exit status for subcommands that are not implemented yet.
const EXIT_NOT_IMPLEMENTED: u8 = 2;

fn not_implemented(what: &str, phase: u32) -> ExitCode {
    eprintln!("bridgev {what}: not implemented yet (Phase {phase}, see docs/ROADMAP.md)");
    ExitCode::from(EXIT_NOT_IMPLEMENTED)
}

fn print_stats(icount: u64, start: Instant) {
    let secs = start.elapsed().as_secs_f64();
    eprintln!(
        "bridgev: {icount} guest instructions in {secs:.3} s ({:.1} MIPS)",
        icount as f64 / secs / 1e6
    );
}

fn run_bare(elf: &PathBuf, max_insns: Option<u64>, trace: bool, stats: bool) -> Result<ExitCode> {
    let data = std::fs::read(elf).with_context(|| format!("reading {}", elf.display()))?;
    let start = Instant::now();
    let (result, icount) = bare::run(&data, max_insns.unwrap_or(100_000_000), trace)?;
    if stats {
        print_stats(icount, start);
    }
    Ok(match result {
        BareResult::Pass => {
            println!("PASS");
            ExitCode::SUCCESS
        }
        BareResult::Fail(n) => {
            println!("FAIL (test case {n})");
            ExitCode::from(1)
        }
        BareResult::Timeout => {
            println!("TIMEOUT (no tohost write after {icount} instructions)");
            ExitCode::from(3)
        }
    })
}

fn main() -> ExitCode {
    let result = match Cli::parse().command {
        Command::Run {
            mode,
            engine,
            max_insns,
            trace,
            stats,
            elf,
            args: _,
        } => {
            if engine == Engine::Jit {
                return not_implemented("run --engine=jit", 2);
            }
            match mode {
                Mode::Bare => run_bare(&elf, max_insns, trace.is_some(), stats),
                Mode::User => return not_implemented("run --mode=user", 1),
            }
        }
        Command::Boot { .. } => return not_implemented("boot", 9),
        Command::Disasm { .. } => return not_implemented("disasm", 1),
        Command::Bench { .. } => return not_implemented("bench", 5),
    };
    result.unwrap_or_else(|e| {
        eprintln!("bridgev: error: {e:#}");
        ExitCode::from(1)
    })
}
