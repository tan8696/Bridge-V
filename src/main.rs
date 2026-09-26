//! `bridgev` command-line interface (CLAUDE.md §23).

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};

use bridgev::elf::{Elf, PF_X};
use bridgev::isa::{decode_parts, disasm};
use bridgev::jit::code_mem::WxMode;
use bridgev::jit::{EngineKind, JitOptions, RegAlloc};
use bridgev::system::bare::{self, BareOptions, BareResult};
use bridgev::user::{self, RunOptions};

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
enum StatsArg {
    /// Counters and MIPS.
    Basic,
    /// Also the static and dynamic guest register-use histogram (P4.9).
    Regs,
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
    /// JIT translator.
    Jit,
    /// JIT checked against the interpreter after every translation block.
    Lockstep,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum RegAllocArg {
    None,
    Pinned,
    Linear,
}

/// `--pin` value: guest register numbers.
#[derive(Clone, Debug)]
struct PinList(Vec<u8>);

/// Parse `x2,x1,x10,x15` (ABI names like `sp,ra,a0,a5` are not accepted, to stay unambiguous).
fn parse_pin(s: &str) -> Result<PinList, String> {
    if s.is_empty() {
        return Ok(PinList(Vec::new()));
    }
    s.split(',')
        .map(|t| {
            t.trim()
                .strip_prefix('x')
                .and_then(|n| n.parse::<u8>().ok())
                .filter(|&n| (1..32).contains(&n))
                .ok_or_else(|| format!("bad register `{t}` (expected x1..x31)"))
        })
        .collect::<Result<Vec<u8>, String>>()
        .map(PinList)
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Wx {
    /// memfd mapped twice: RW view for writing, RX view for executing.
    Dualmap,
    /// One mapping, toggled RW/RX with mprotect around every write.
    Mprotect,
}

/// Parse a size with an optional K/M/G suffix (powers of 1024).
fn parse_size(s: &str) -> Result<usize, String> {
    let (num, mul) = match s.as_bytes().last() {
        Some(b'K' | b'k') => (&s[..s.len() - 1], 1 << 10),
        Some(b'M' | b'm') => (&s[..s.len() - 1], 1 << 20),
        Some(b'G' | b'g') => (&s[..s.len() - 1], 1 << 30),
        _ => (s, 1),
    };
    num.parse::<usize>()
        .ok()
        .and_then(|n| n.checked_mul(mul))
        .ok_or_else(|| format!("invalid size `{s}`"))
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
        /// Print execution statistics on stderr; `--stats=regs` adds the guest register-use
        /// histogram (needs --engine interp).
        #[arg(long, value_enum, num_args = 0..=1, require_equals = true, default_missing_value = "basic")]
        stats: Option<StatsArg>,
        /// Log every syscall on stderr (user mode).
        #[arg(long)]
        strace: bool,
        /// Maximum guest instructions per translation block.
        #[arg(long, default_value_t = 128, value_parser = clap::value_parser!(u32).range(1..=4096))]
        max_block: u32,
        /// Code cache size (K/M/G suffix, at most 1G).
        #[arg(long, default_value = "256M", value_parser = parse_size)]
        code_cache: usize,
        /// How the code cache enforces W^X.
        #[arg(long, value_enum, default_value = "dualmap")]
        wx: Wx,
        /// Use only baseline x86-64 in generated code (ignore BMI2, FMA, ...).
        #[arg(long)]
        no_host_features: bool,
        /// Write every translation block's host code to this directory.
        #[arg(long)]
        dump_x86: Option<PathBuf>,
        /// Append JIT symbols to /tmp/perf-<pid>.map.
        #[arg(long)]
        perf_map: bool,
        /// Make the `time` CSR follow the instruction count (reproducible runs).
        #[arg(long)]
        deterministic: bool,
        /// Guest register mapping: none (all in memory), pinned (R12–R15 only), linear (full).
        #[arg(long, value_enum, default_value = "linear")]
        regalloc: RegAllocArg,
        /// Guest registers pinned to R12–R15 (comma-separated, at most 4), e.g. x2,x1,x10,x15.
        #[arg(long, default_value = "x2,x1,x10,x15", value_parser = parse_pin)]
        pin: PinList,
        /// Write every translation block's IR (before/after the passes) to this directory.
        #[arg(long)]
        dump_ir: Option<PathBuf>,
        /// Never link exits or use the jump cache: every block returns to the dispatcher.
        #[arg(long)]
        no_chain: bool,
        /// Count JALR executions in JIT code (jump-cache hit rate in --stats; adds overhead).
        #[arg(long)]
        profile_jit: bool,
        /// Testing only: deliberately miscompile ADDI (lockstep must catch it).
        #[arg(long, hide = true)]
        inject_bug: bool,
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

fn run_bare(elf: &Path, opts: BareOptions, stats: bool) -> Result<ExitCode> {
    let data = std::fs::read(elf).with_context(|| format!("reading {}", elf.display()))?;
    let start = Instant::now();
    let r = bare::run(&data, &opts)?;
    let icount = r.icount;
    if stats {
        print_stats(icount, start);
        eprintln!("bridgev: {}", r.engine_stats);
    }
    Ok(match r.result {
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

fn run_user(elf: &Path, args: Vec<String>, opts: RunOptions, stats: bool) -> Result<ExitCode> {
    let mut argv = vec![elf.to_string_lossy().into_owned()];
    argv.extend(args);
    let envs: Vec<String> = std::env::vars().map(|(k, v)| format!("{k}={v}")).collect();
    let start = Instant::now();
    let r = user::run(elf, &argv, &envs, opts)?;
    if stats {
        print_stats(r.icount, start);
        eprintln!("bridgev: {}", r.engine_stats);
    }
    Ok(ExitCode::from(r.exit_code as u8))
}

/// Linear-sweep disassembly of every executable segment.
fn disasm_file(path: &Path) -> Result<ExitCode> {
    let data = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let elf = Elf::parse(&data)?;
    for seg in elf.loads().filter(|s| s.flags & PF_X != 0) {
        let bytes = elf.segment_data(seg);
        let half = |i: usize| -> Result<u16, ()> {
            bytes
                .get(i..i + 2)
                .map(|b| u16::from_le_bytes([b[0], b[1]]))
                .ok_or(())
        };
        let mut off = 0;
        while let Ok(lo) = half(off) {
            let Ok(d) = decode_parts(lo, || half(off + 2)) else {
                break;
            };
            let raw = if d.len == 2 {
                format!("    {:04x}", d.raw)
            } else {
                format!("{:08x}", d.raw)
            };
            println!(
                "{:16x}:  {raw}  {}",
                seg.vaddr + off as u64,
                disasm(&d.inst)
            );
            off += d.len as usize;
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn main() -> ExitCode {
    let result = match Cli::parse().command {
        Command::Run {
            mode,
            engine,
            max_insns,
            trace,
            stats,
            strace,
            max_block,
            code_cache,
            wx,
            no_host_features,
            dump_x86,
            perf_map,
            deterministic,
            inject_bug,
            no_chain,
            profile_jit,
            regalloc,
            pin,
            dump_ir,
            elf,
            args,
        } => {
            let engine = match engine {
                Engine::Interp => EngineKind::Interp,
                Engine::Jit => EngineKind::Jit,
                Engine::Lockstep => EngineKind::Lockstep,
            };
            if trace.is_some() && engine != EngineKind::Interp {
                eprintln!("bridgev: --trace is only supported with --engine=interp");
                return ExitCode::from(EXIT_NOT_IMPLEMENTED);
            }
            let reg_stats = stats == Some(StatsArg::Regs);
            let stats = stats.is_some();
            let jit = JitOptions {
                max_block: max_block as usize,
                code_cache,
                wx: match wx {
                    Wx::Dualmap => WxMode::DualMap,
                    Wx::Mprotect => WxMode::Mprotect,
                },
                host_features: !no_host_features,
                dump_x86,
                perf_map,
                inject_bug,
                chain: !no_chain,
                profile: profile_jit,
                regalloc: match regalloc {
                    RegAllocArg::None => RegAlloc::None,
                    RegAllocArg::Pinned => RegAlloc::Pinned,
                    RegAllocArg::Linear => RegAlloc::Linear,
                },
                pin: pin.0,
                dump_ir,
                ..JitOptions::default()
            };
            match mode {
                Mode::Bare => {
                    let opts = BareOptions {
                        max_insns: max_insns.unwrap_or(100_000_000),
                        trace: trace.is_some(),
                        engine,
                        jit,
                        reg_stats,
                    };
                    run_bare(&elf, opts, stats)
                }
                Mode::User => {
                    let opts = RunOptions {
                        trace: trace.is_some(),
                        strace,
                        max_insns,
                        engine,
                        jit,
                        deterministic,
                        reg_stats,
                    };
                    run_user(&elf, args, opts, stats)
                }
            }
        }
        Command::Boot { .. } => return not_implemented("boot", 9),
        Command::Disasm { elf } => disasm_file(&elf),
        Command::Bench { .. } => return not_implemented("bench", 5),
    };
    result.unwrap_or_else(|e| {
        eprintln!("bridgev: error: {e:#}");
        ExitCode::from(1)
    })
}
