//! `bridgev` command-line interface (CLAUDE.md §23). In Phase 0 every subcommand is a stub that
//! reports which roadmap phase implements it and exits with status 2.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

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

#[derive(Subcommand)]
enum Command {
    /// Run a RISC-V Linux ELF executable in user mode.
    Run {
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

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Run { .. } => not_implemented("run", 1),
        Command::Boot { .. } => not_implemented("boot", 9),
        Command::Disasm { .. } => not_implemented("disasm", 1),
        Command::Bench { .. } => not_implemented("bench", 5),
    }
}
