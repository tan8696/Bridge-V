//! `bridgev` command-line interface (CLAUDE.md §23).

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};

use bridgev::elf::{Elf, PF_X};
use bridgev::isa::{decode_parts, disasm};
use bridgev::jit::code_mem::WxMode;
use bridgev::jit::{DEFAULT_TIER, EngineKind, JitOptions, RegAlloc};
use bridgev::system::bare::{self, BareOptions, BareResult};
use bridgev::system::machine::{self, BootExit, BootOptions};
use bridgev::system::uart16550::Sink;
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

/// `bridgev boot --mmu`.
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum MmuArg {
    Sv39,
    Sv48,
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
enum SmcArg {
    /// Writes to code pages invalidate their translations at once; FENCE.I is cheap.
    Eager,
    /// Additionally flush every translation on FENCE.I (debug cross-check).
    FlushOnFence,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum MemArg {
    /// Guest address g at host base + g, no checks in JIT code (user mode).
    Direct,
    /// Every access through the software TLB and MMU (always used in bare mode).
    Softmmu,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Wx {
    /// memfd mapped twice: RW view for writing, RX view for executing.
    Dualmap,
    /// One mapping, toggled RW/RX with mprotect around every write.
    Mprotect,
}

/// Exit status for an unsupported option combination.
const EXIT_NOT_IMPLEMENTED: u8 = 2;

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
        /// Look up the program interpreter and absolute paths here first, like qemu's -L
        /// (default for dynamically linked programs: /usr/riscv64-linux-gnu).
        #[arg(long, short = 'L')]
        sysroot: Option<PathBuf>,
        /// Wait for a debugger (GDB remote protocol) on 127.0.0.1:PORT; execution then steps
        /// through the interpreter.
        #[arg(long, value_name = "PORT")]
        gdb: Option<u16>,
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
        /// Sample where host time goes (1 kHz SIGPROF) and report the hottest TBs in --stats.
        #[arg(long)]
        profile_tbs: bool,
        /// Run every FP instruction through the interpreter helper (no inline SSE code).
        #[arg(long)]
        no_inline_fp: bool,
        /// Guest memory backend in user mode (bare mode always uses softmmu).
        #[arg(long, value_enum, default_value = "direct")]
        mem: MemArg,
        /// Self-modifying code handling.
        #[arg(long, value_enum, default_value = "eager")]
        smc: SmcArg,
        /// JIT: run each block N times in the interpreter before translating it (0: translate
        /// every block on its first run).
        #[arg(long, default_value_t = DEFAULT_TIER, value_name = "N")]
        tier: u32,
        /// Testing only: deliberately miscompile ADDI (lockstep must catch it).
        #[arg(long, hide = true)]
        inject_bug: bool,
        /// Guest executable.
        elf: PathBuf,
        /// Arguments passed to the guest program.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Boot a RISC-V Linux kernel in system mode (QEMU `virt`-like machine, built-in SBI).
    Boot {
        /// Kernel `Image` file.
        #[arg(long)]
        kernel: PathBuf,
        /// M-mode firmware (OpenSBI fw_dynamic/fw_jump .bin) instead of the built-in SBI.
        #[arg(long)]
        firmware: Option<PathBuf>,
        /// Disk image served as a virtio-blk device (/dev/vda), read-write.
        #[arg(long)]
        disk: Option<PathBuf>,
        /// Number of harts (1-8); they run one at a time, round-robin per slice.
        #[arg(long, default_value_t = 1)]
        smp: usize,
        /// Virtual-memory modes offered to the guest (sv48 also allows Sv39).
        #[arg(long, value_enum, default_value = "sv39")]
        mmu: MmuArg,
        /// Initial ramdisk (cpio archive).
        #[arg(long)]
        initrd: Option<PathBuf>,
        /// Use this devicetree blob instead of the generated one.
        #[arg(long)]
        dtb: Option<PathBuf>,
        /// Write the generated devicetree blob to this file.
        #[arg(long)]
        dump_dtb: Option<PathBuf>,
        /// Guest RAM size (K/M/G suffix).
        #[arg(long, default_value = "512M", value_parser = parse_size)]
        ram: usize,
        /// Kernel command line.
        #[arg(long, default_value = "console=ttyS0 earlycon=sbi")]
        append: String,
        /// Execution engine.
        #[arg(long, value_enum, default_value = "jit")]
        engine: Engine,
        /// Guest register mapping (JIT).
        #[arg(long, value_enum, default_value = "linear")]
        regalloc: RegAllocArg,
        /// Never link exits or use the jump cache.
        #[arg(long)]
        no_chain: bool,
        /// JIT: run each block N times in the interpreter before translating it (0: translate
        /// every block on its first run).
        #[arg(long, default_value_t = DEFAULT_TIER, value_name = "N")]
        tier: u32,
        /// Make the `time` CSR and the timers follow the instruction count (reproducible).
        #[arg(long)]
        deterministic: bool,
        /// Stop after this many guest instructions.
        #[arg(long)]
        max_insns: Option<u64>,
        /// Instructions per slice between device/interrupt updates.
        #[arg(long, default_value_t = 100_000)]
        slice: u64,
        /// Print statistics on exit.
        #[arg(long)]
        stats: bool,
    },
    /// Write an initramfs (newc cpio) with BusyBox and Bridge-V's /init script.
    Mkinitramfs {
        /// Statically linked riscv64 BusyBox binary.
        #[arg(long)]
        busybox: PathBuf,
        /// Output file.
        #[arg(long)]
        out: PathBuf,
    },
    /// Disassemble the executable segments of a RISC-V ELF.
    Disasm {
        /// Guest executable.
        elf: PathBuf,
    },
}

fn print_stats(icount: u64, start: Instant) {
    let secs = start.elapsed().as_secs_f64();
    eprintln!(
        "bridgev: {icount} guest instructions in {secs:.3} s ({:.1} MIPS)",
        icount as f64 / secs / 1e6
    );
}

fn run_boot(opts: BootOptions, stats: bool) -> Result<ExitCode> {
    let (uart, console) = machine::console(opts.console.clone());
    // Host stdin feeds the UART receive queue.
    let rx = console.uart.clone();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = [0u8; 256];
        let mut stdin = std::io::stdin();
        while let Ok(n) = stdin.read(&mut buf) {
            if n == 0 {
                break;
            }
            rx.lock().unwrap().rx.extend(&buf[..n]);
        }
    });
    let start = Instant::now();
    let r = machine::boot(&opts, uart)?;
    eprintln!("\nbridgev: machine stopped: {:?}", r.exit);
    if stats {
        print_stats(r.icount, start);
        eprintln!(
            "bridgev: {} SBI calls; {} WFI idles, {:.3} s idle; {}",
            r.sbi_calls,
            r.wfis,
            r.idle.as_secs_f64(),
            r.engine_stats
        );
    }
    Ok(match r.exit {
        BootExit::PowerOff | BootExit::Reset => ExitCode::SUCCESS,
        BootExit::Failure(_) => ExitCode::from(1),
        BootExit::InstructionLimit => ExitCode::from(3),
    })
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
            sysroot,
            gdb,
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
            profile_tbs,
            no_inline_fp,
            mem,
            smc,
            tier,
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
                profile_tbs,
                regalloc: match regalloc {
                    RegAllocArg::None => RegAlloc::None,
                    RegAllocArg::Pinned => RegAlloc::Pinned,
                    RegAllocArg::Linear => RegAlloc::Linear,
                },
                pin: pin.0,
                dump_ir,
                inline_fp: !no_inline_fp,
                smc_flush_on_fence: smc == SmcArg::FlushOnFence,
                tier,
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
                        softmmu: mem == MemArg::Softmmu,
                        sysroot,
                        gdb,
                    };
                    run_user(&elf, args, opts, stats)
                }
            }
        }
        Command::Boot {
            kernel,
            firmware,
            disk,
            smp,
            mmu,
            initrd,
            dtb,
            dump_dtb,
            ram,
            append,
            engine,
            regalloc,
            no_chain,
            tier,
            deterministic,
            max_insns,
            slice,
            stats,
        } => {
            let engine = match engine {
                Engine::Interp => EngineKind::Interp,
                Engine::Jit => EngineKind::Jit,
                Engine::Lockstep => EngineKind::Lockstep,
            };
            let jit = JitOptions {
                chain: !no_chain,
                regalloc: match regalloc {
                    RegAllocArg::None => RegAlloc::None,
                    RegAllocArg::Pinned => RegAlloc::Pinned,
                    RegAllocArg::Linear => RegAlloc::Linear,
                },
                tier,
                ..JitOptions::default()
            };
            let opts = BootOptions {
                kernel,
                firmware,
                sv48: mmu == MmuArg::Sv48,
                disk,
                harts: smp,
                initrd,
                dtb,
                dump_dtb,
                ram: ram as u64,
                bootargs: append,
                engine,
                jit,
                deterministic,
                max_insns,
                slice,
                console: Sink::Stdout,
                input: None,
            };
            run_boot(opts, stats)
        }
        Command::Mkinitramfs { busybox, out } => (|| -> Result<ExitCode> {
            let bb = std::fs::read(&busybox)?;
            std::fs::write(&out, machine::initramfs(&bb, machine::INIT_SCRIPT))?;
            Ok(ExitCode::SUCCESS)
        })(),
        Command::Disasm { elf } => disasm_file(&elf),
    };
    result.unwrap_or_else(|e| {
        eprintln!("bridgev: error: {e:#}");
        ExitCode::from(1)
    })
}
