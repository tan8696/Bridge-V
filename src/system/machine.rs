//! The `virt`-compatible machine and the Linux boot flow (CLAUDE.md §20, P9.1–P9.3).
//!
//! RAM at 0x8000_0000, the kernel `Image` at RAM + `text_offset`, the devicetree at the top of
//! RAM and the initrd just below it. The hart starts in S-mode at the kernel entry with
//! `a0 = 0` (hart id) and `a1 = dtb`, `satp = 0`; M-mode is the built-in SBI (D15), so
//! synchronous exceptions except S-mode ecalls and S-level interrupts are delegated to S.
//!
//! The machine loop runs the engine in slices of instructions. Between slices it moves device
//! state into `mip` (timer deadline → STIP, CLINT → MTIP/MSIP, UART → PLIC → SEIP/MEIP), serves
//! SBI calls (S-mode ECALL stops the engine), and checks for poweroff/reset requests.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};

use crate::cpu::state::CpuState;
use crate::cpu::trap::prv;
use crate::interp::{Env, Stop};
use crate::jit::{EngineKind, JitOptions, make_engine};
use crate::mem::direct::DirectMem;
use crate::mem::{GuestVirt, prot, tlb};

use super::clint::Clint;
use super::fdt::{VirtConfig, virt_dtb};
use super::plic::MAX_HARTS;
use super::plic::Plic;
use super::sbi::{self, HartStatus, SbiAction, SbiState};
use super::syscon::{Finish, Syscon};
use super::uart16550::{Sink, Uart, UartState};
use super::virtio_blk::{VIRTIO_BASE, VIRTIO_IRQ, VirtioBlk};

pub const RAM_BASE: u64 = 0x8000_0000;
pub const SYSCON_BASE: u64 = 0x10_0000;
pub const CLINT_BASE: u64 = 0x200_0000;
pub const PLIC_BASE: u64 = 0xc00_0000;
pub const UART_BASE: u64 = 0x1000_0000;
/// PLIC source of the UART.
pub const UART_IRQ: usize = 10;

const MIP_SSIP: u64 = 1 << 1;
const MIP_MSIP: u64 = 1 << 3;
const MIP_STIP: u64 = 1 << 5;
const MIP_MTIP: u64 = 1 << 7;
const MIP_SEIP: u64 = 1 << 9;
const MIP_MEIP: u64 = 1 << 11;

/// `bridgev boot` options.
pub struct BootOptions {
    pub kernel: PathBuf,
    /// M-mode firmware (OpenSBI `fw_dynamic` or `fw_jump`) loaded at the start of RAM and
    /// entered in M-mode; `None` = the built-in SBI (D15).
    pub firmware: Option<PathBuf>,
    /// Offer Sv48 as well as Sv39 (satp mode 9, devicetree `riscv,sv48`).
    pub sv48: bool,
    /// Disk image for a virtio-blk device (`/dev/vda`), read-write (Phase 10).
    pub disk: Option<PathBuf>,
    /// Number of harts (Phase 10 SMP, 1..=8).
    pub harts: usize,
    pub initrd: Option<PathBuf>,
    /// Use this DTB instead of the generated one.
    pub dtb: Option<PathBuf>,
    /// Write the generated DTB here.
    pub dump_dtb: Option<PathBuf>,
    pub ram: u64,
    pub bootargs: String,
    pub engine: EngineKind,
    pub jit: JitOptions,
    pub deterministic: bool,
    /// Stop after this many guest instructions.
    pub max_insns: Option<u64>,
    /// Instructions per engine slice (device/interrupt update interval).
    pub slice: u64,
    pub console: Sink,
    /// Console input (the UART's receive queue); the caller may keep feeding it.
    pub input: Option<Arc<Mutex<UartState>>>,
}

/// How a boot ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootExit {
    PowerOff,
    /// The guest reported failure (SBI SRST reason, or the test finisher's code).
    Failure(u32),
    Reset,
    InstructionLimit,
}

pub struct BootRun {
    pub exit: BootExit,
    pub icount: u64,
    pub engine_stats: String,
    pub sbi_calls: u64,
    /// Host time spent idle in WFI, and how many WFIs stopped the engine.
    pub idle: std::time::Duration,
    pub wfis: u64,
}

/// A handle the caller can use to feed console input while the machine runs.
pub struct Console {
    pub uart: Arc<Mutex<UartState>>,
}

/// Build the machine and create a console handle (for a caller that wants to type into it).
pub fn console(sink: Sink) -> (Uart, Console) {
    let uart = Uart::new(sink);
    let c = Console {
        uart: uart.state.clone(),
    };
    (uart, c)
}

/// Linux RISC-V `Image` header (Documentation/arch/riscv/boot-image-header.rst).
fn image_layout(img: &[u8]) -> Result<(u64, u64)> {
    if img.len() < 64 || &img[0x30..0x38] != b"RISCV\0\0\0" || &img[0x38..0x3c] != b"RSC\x05" {
        bail!("not a RISC-V Linux Image (bad header magic)");
    }
    let text_offset = u64::from_le_bytes(img[8..16].try_into().unwrap());
    let image_size = u64::from_le_bytes(img[16..24].try_into().unwrap());
    Ok((text_offset, image_size.max(img.len() as u64)))
}

/// Boot a Linux kernel (or any S-mode payload with an Image header) and run until poweroff,
/// reset or the instruction limit. `uart` is the console device from `console()`.
pub fn boot(opts: &BootOptions, uart: Uart) -> Result<BootRun> {
    let img = std::fs::read(&opts.kernel)
        .with_context(|| format!("reading {}", opts.kernel.display()))?;
    let (text_offset, image_size) = image_layout(&img)?;
    let ram_end = RAM_BASE + opts.ram;
    let entry = RAM_BASE + text_offset;
    let mut mem = DirectMem::new()?;
    mem.map(GuestVirt(RAM_BASE), opts.ram, prot::RWX)?;
    mem.write_bytes(GuestVirt(entry), &img)
        .map_err(|f| anyhow::anyhow!("kernel does not fit in RAM: {f:?}"))?;

    // DTB at the top of RAM (2 MiB aligned), the initrd right below it.
    let dtb_addr = (ram_end - (2 << 20)) & !((2 << 20) - 1);
    let initrd = match &opts.initrd {
        Some(p) => {
            let data = std::fs::read(p).with_context(|| format!("reading {}", p.display()))?;
            let start = (dtb_addr - data.len() as u64) & !0xfff;
            if start < entry + image_size {
                bail!(
                    "initrd ({} bytes) overlaps the kernel: use more --ram",
                    data.len()
                );
            }
            mem.write_bytes(GuestVirt(start), &data)
                .map_err(|f| anyhow::anyhow!("loading initrd: {f:?}"))?;
            Some((start, start + data.len() as u64))
        }
        None => None,
    };
    let dtb = match &opts.dtb {
        Some(p) => std::fs::read(p).with_context(|| format!("reading {}", p.display()))?,
        None => virt_dtb(&VirtConfig {
            ram_base: RAM_BASE,
            ram_size: opts.ram,
            bootargs: opts.bootargs.clone(),
            initrd,
            sv48: opts.sv48,
            virtio_blk: opts.disk.is_some(),
            harts: opts.harts.clamp(1, MAX_HARTS),
        }),
    };
    if let Some(p) = &opts.dump_dtb {
        std::fs::write(p, &dtb).with_context(|| format!("writing {}", p.display()))?;
    }
    mem.write_bytes(GuestVirt(dtb_addr), &dtb)
        .map_err(|f| anyhow::anyhow!("loading dtb: {f:?}"))?;

    // Firmware (Phase 10): at the start of RAM, below the kernel. It is entered like QEMU's
    // reset vector does: a0 = hart id, a1 = DTB, a2 = `struct fw_dynamic_info` (OpenSBI
    // include/sbi/fw_dynamic.h, version 2: magic "OSBI", version, next_addr, next_mode = S,
    // options, boot_hart), placed 1 MiB above the DTB. fw_jump ignores a2.
    let fw_info = match &opts.firmware {
        Some(p) => {
            let fw = std::fs::read(p).with_context(|| format!("reading {}", p.display()))?;
            if RAM_BASE + fw.len() as u64 > entry {
                bail!("firmware ({} bytes) overlaps the kernel", fw.len());
            }
            mem.write_bytes(GuestVirt(RAM_BASE), &fw)
                .map_err(|f| anyhow::anyhow!("loading firmware: {f:?}"))?;
            let info = dtb_addr + (1 << 20);
            let words = [0x4942_534f, 2, entry, 1, 0, 0];
            let bytes: Vec<u8> = words.iter().flat_map(|w: &u64| w.to_le_bytes()).collect();
            mem.write_bytes(GuestVirt(info), &bytes)
                .map_err(|f| anyhow::anyhow!("loading fw_dynamic_info: {f:?}"))?;
            Some(info)
        }
        None => None,
    };
    // Loader writes are not code modification.
    mem.smc_pages.clear();

    // Devices.
    let mtime = Arc::new(AtomicU64::new(0));
    let clint = Clint::new(mtime.clone());
    let clint_state = clint.state.clone();
    let plic = Plic::new();
    let plic_state = plic.state.clone();
    let uart_state = uart.state.clone();
    let syscon = Syscon::new();
    let finish = syscon.request.clone();
    mem.add_device(CLINT_BASE, 0x10000, Box::new(clint));
    mem.add_device(PLIC_BASE, 0x60_0000, Box::new(plic));
    mem.add_device(UART_BASE, 0x100, Box::new(uart));
    mem.add_device(SYSCON_BASE, 0x1000, Box::new(syscon));
    let blk = match &opts.disk {
        Some(p) => {
            let b = VirtioBlk::open(p).with_context(|| format!("opening disk {}", p.display()))?;
            mem.add_device(VIRTIO_BASE, 0x1000, Box::new(b.regs()));
            Some(b)
        }
        None => None,
    };

    // Harts (Phase 10 SMP): the first one boots; with the built-in SBI the others wait
    // (stopped) for an HSM hart_start, with firmware they all enter it, as on QEMU.
    let builtin_sbi = fw_info.is_none();
    let n = opts.harts.clamp(1, MAX_HARTS);
    let template = {
        let mut c = CpuState::new_machine(entry);
        c.softmmu = 1;
        c.csr.deterministic_time = opts.deterministic;
        c.csr.wfi_idle = true;
        c.csr.sv48 = opts.sv48;
        if builtin_sbi {
            c.csr.medeleg = 0xb3ff & !(1 << 9); // everything delegable except ecall from S
            c.csr.mideleg = 0x222; // SSI, STI, SEI
            c.csr.mcounteren = 0x7;
        }
        c
    };
    let mut harts = Vec::with_capacity(n);
    for h in 0..n {
        let mut cpu = template.clone();
        cpu.csr.mhartid = h as u64;
        cpu.x[10] = h as u64;
        cpu.x[11] = dtb_addr;
        let status = match fw_info {
            // All harts in M-mode at the firmware, with reset CSR values: it delegates.
            Some(info) => {
                cpu.pc = RAM_BASE;
                cpu.x[12] = info;
                HartStatus::Started
            }
            // Hart 0 in S-mode at the kernel entry (§20.2).
            None => {
                cpu.prv = prv::S;
                if h == 0 {
                    HartStatus::Started
                } else {
                    HartStatus::Stopped
                }
            }
        };
        harts.push(Hart {
            cpu,
            engine: make_engine(opts.engine, &opts.jit)?,
            status,
            waiting: false,
            smc_seen: 0,
        });
    }

    // With firmware, S-mode ECALLs trap to it; otherwise the built-in SBI serves them.
    let env = Env {
        user_mode: false,
        tohost: None,
        trace: false,
        sbi: builtin_sbi,
    };
    let mut st = SbiState {
        stimecmp: [u64::MAX; MAX_HARTS],
        console: opts.console.clone(),
        calls: 0,
    };
    let limit = opts.max_insns.unwrap_or(u64::MAX);
    let (mut idle, mut wfis) = (std::time::Duration::ZERO, 0);
    let mut cur = 0;
    let exit = loop {
        let total: u64 = harts.iter().map(|h| h.cpu.icount).sum();
        if total >= limit {
            break BootExit::InstructionLimit;
        }
        // Devices → mip of every hart. Hart 0's clock is the machine's (all harts share its
        // time origin).
        let now = harts[0].cpu.time();
        mtime.store(now, Ordering::Relaxed);
        let uart_irq = uart_state.lock().unwrap().irq();
        // Serve the disk requests the driver queued during the last slice.
        let blk_irq = blk.as_ref().is_some_and(|b| {
            b.process(&mut mem);
            b.state.lock().unwrap().irq()
        });
        {
            let mut p = plic_state.lock().unwrap();
            p.set_level(UART_IRQ, uart_irq);
            p.set_level(VIRTIO_IRQ, blk_irq);
            let c = clint_state.lock().unwrap();
            for (h, hart) in harts.iter_mut().enumerate() {
                let (meip, seip) = p.outputs(h);
                let cpu = &mut hart.cpu;
                // STIP is the built-in SBI's timer; with firmware M-mode software writes it.
                let sw_stip = if builtin_sbi {
                    0
                } else {
                    cpu.csr.mip & MIP_STIP
                };
                let mut mip =
                    cpu.csr.mip & !(MIP_MSIP | MIP_MTIP | MIP_MEIP | MIP_SEIP | MIP_STIP) | sw_stip;
                for (on, bit) in [
                    (c.msip[h], MIP_MSIP),
                    (now >= c.mtimecmp[h], MIP_MTIP),
                    (meip, MIP_MEIP),
                    (seip, MIP_SEIP),
                    (builtin_sbi && now >= st.stimecmp[h], MIP_STIP),
                ] {
                    if on {
                        mip |= bit;
                    }
                }
                cpu.csr.mip = mip;
                // WFI ends when an interrupt is pending locally, enabled or not globally.
                if hart.waiting && mip & cpu.csr.mie != 0 {
                    hart.waiting = false;
                }
            }
        }

        // The next runnable hart, round-robin.
        let runnable = |h: &Hart| h.status == HartStatus::Started && !h.waiting;
        let next = (1..=n)
            .map(|k| (cur + k) % n)
            .find(|&h| runnable(&harts[h]));
        let Some(h) = next else {
            // Every hart waits in WFI: idle the host until the next timer deadline, console
            // input, a disk request or a finish request (§15).
            let deadline = {
                let c = clint_state.lock().unwrap();
                (0..n)
                    .filter(|&h| harts[h].status == HartStatus::Started)
                    .map(|h| c.mtimecmp[h].min(st.stimecmp[h]))
                    .min()
                    .unwrap_or(u64::MAX)
            };
            let t0 = std::time::Instant::now();
            // Firmware parks harts in WFI after writing the test finisher (poweroff): never
            // sleep past a pending finish request.
            while finish.lock().unwrap().is_none() {
                let now = harts[0].cpu.time();
                let disk = blk
                    .as_ref()
                    .is_some_and(|b| b.state.lock().unwrap().pending());
                if now >= deadline || disk || !uart_state.lock().unwrap().rx.is_empty() {
                    break;
                }
                // `time` ticks at 10 MHz; poll input at least every millisecond.
                let ns = (deadline - now).saturating_mul(100).min(1_000_000);
                std::thread::sleep(std::time::Duration::from_nanos(ns));
            }
            idle += t0.elapsed();
            if let Some(f) = finish.lock().unwrap().take() {
                break f.into();
            }
            // Deadlines passed or input arrived: the loop top raises the interrupts, which
            // end the harts' WFI.
            continue;
        };
        if h != cur && n > 1 {
            // Another hart may have stored to the reserved address meanwhile.
            harts[h].cpu.res_valid = 0;
        }
        cur = h;
        let hart = &mut harts[h];
        // Code other harts overwrote since this hart's engine last ran (D49 across harts).
        if n > 1 {
            let seen = hart.smc_seen.min(mem.smc_log.len());
            let fresh: Vec<u64> = mem.smc_log[seen..].to_vec();
            mem.smc_pages.extend(fresh);
        }
        let left = (limit - total).min(opts.slice);
        let stop = hart.engine.run(&mut hart.cpu, &mut mem, &env, left);
        hart.smc_seen = mem.smc_log.len();
        if mem.smc_log.len() > 4096 {
            let low = harts.iter().map(|h| h.smc_seen).min().unwrap_or(0);
            mem.smc_log.drain(..low);
            for hart in harts.iter_mut() {
                hart.smc_seen -= low;
            }
        }
        match stop {
            Stop::Limit => {}
            Stop::Ecall => {
                let status: Vec<HartStatus> = harts.iter().map(|h| h.status).collect();
                let action = {
                    let mut u = uart_state.lock().unwrap();
                    sbi::call(&mut harts[h].cpu, h, &status, &mut mem, &mut st, &mut u.rx)
                };
                let cpu = &mut harts[h].cpu;
                cpu.pc += 4; // ECALL has no compressed form
                cpu.icount += 1;
                let each = |mask: u64| (0..n).filter(move |k| mask >> k & 1 == 1);
                match action {
                    SbiAction::None => {}
                    SbiAction::Shutdown(false) => break BootExit::PowerOff,
                    SbiAction::Shutdown(true) => break BootExit::Failure(1),
                    SbiAction::Reset => break BootExit::Reset,
                    SbiAction::Ipi(mask) => {
                        for k in each(mask) {
                            harts[k].cpu.csr.mip |= MIP_SSIP;
                        }
                    }
                    SbiAction::RemoteFenceI(mask) => {
                        for k in each(mask) {
                            harts[k].engine.fence_i();
                        }
                    }
                    SbiAction::RemoteSfence(mask) => {
                        for k in each(mask) {
                            tlb::flush_all(&mut harts[k].cpu);
                        }
                    }
                    SbiAction::HartStart {
                        hart: k,
                        addr,
                        opaque,
                    } => {
                        // SBI HSM: S-mode at `addr`, a0 = hart id, a1 = opaque, satp = 0,
                        // interrupts off.
                        let mut cpu = template.clone();
                        cpu.csr.time_origin = harts[0].cpu.csr.time_origin;
                        cpu.csr.mhartid = k as u64;
                        cpu.prv = prv::S;
                        cpu.pc = addr;
                        cpu.x[10] = k as u64;
                        cpu.x[11] = opaque;
                        harts[k].cpu = cpu;
                        harts[k].status = HartStatus::Started;
                        harts[k].waiting = false;
                    }
                    SbiAction::HartStop => harts[h].status = HartStatus::Stopped,
                }
            }
            Stop::Wfi => {
                wfis += 1;
                harts[h].waiting = true;
            }
            Stop::Diverged => bail!("lockstep divergence (see above)"),
            other => bail!("unexpected stop in system mode: {other:?}"),
        }
        if let Some(f) = finish.lock().unwrap().take() {
            break f.into();
        }
    };
    let icount = harts.iter().map(|h| h.cpu.icount).sum();
    let per_hart = if n > 1 {
        let counts: Vec<String> = harts.iter().map(|h| h.cpu.icount.to_string()).collect();
        format!("\nharts: {n}, instructions per hart {}", counts.join(" / "))
    } else {
        String::new()
    };
    let tlb_fills: u64 = harts.iter().map(|h| h.cpu.tlb_fills).sum();
    Ok(BootRun {
        exit,
        icount,
        engine_stats: harts[0].engine.stats()
            + &format!("\nsoftmmu: {tlb_fills} TLB fills")
            + &per_hart
            + &blk.map_or(String::new(), |b| {
                let s = b.state.lock().unwrap();
                format!(
                    "\nvirtio-blk: {} requests, {} sectors read, {} written",
                    s.requests, s.sectors_read, s.sectors_written
                )
            }),
        sbi_calls: st.calls,
        idle,
        wfis,
    })
}

/// One hart of the machine (Phase 10 SMP).
struct Hart {
    cpu: Box<CpuState>,
    engine: Box<dyn crate::interp::Engine>,
    status: HartStatus,
    /// Stopped in WFI with nothing pending: skipped until an interrupt is pending.
    waiting: bool,
    /// Length of `DirectMem::smc_log` when this hart's engine last ran.
    smc_seen: usize,
}

impl From<Finish> for BootExit {
    fn from(f: Finish) -> Self {
        match f {
            Finish::Pass => BootExit::PowerOff,
            Finish::Fail(c) => BootExit::Failure(c),
            Finish::Reset => BootExit::Reset,
        }
    }
}

/// Build an initramfs (`newc` cpio) holding BusyBox and an `/init` script, without needing
/// root: device nodes are written straight into the archive (P9.4).
pub fn initramfs(busybox: &[u8], init: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut ino = 1u32;
    let mut entry = |name: &str, mode: u32, data: &[u8], rdev: (u32, u32)| {
        let hdr = format!(
            "070701{ino:08x}{mode:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}",
            0,
            0,
            1,
            0,
            data.len(),
            0,
            0,
            rdev.0,
            rdev.1,
            name.len() + 1,
            0
        );
        ino += 1;
        out.extend_from_slice(hdr.as_bytes());
        out.extend_from_slice(name.as_bytes());
        out.push(0);
        while !out.len().is_multiple_of(4) {
            out.push(0);
        }
        out.extend_from_slice(data);
        while !out.len().is_multiple_of(4) {
            out.push(0);
        }
    };
    const DIR: u32 = 0o040755;
    for d in [
        "bin", "sbin", "dev", "proc", "sys", "tmp", "etc", "root", "usr", "usr/bin",
    ] {
        entry(d, DIR, &[], (0, 0));
    }
    entry("dev/console", 0o020600, &[], (5, 1));
    entry("dev/null", 0o020666, &[], (1, 3));
    entry("bin/busybox", 0o100755, busybox, (0, 0));
    entry("bin/sh", 0o120777, b"busybox", (0, 0));
    entry("init", 0o100755, init.as_bytes(), (0, 0));
    entry("TRAILER!!!", 0, &[], (0, 0));
    out
}

/// The `/init` of the Bridge-V initramfs: mount the pseudo file systems, install the BusyBox
/// applets, print a banner and start a shell on the console.
pub const INIT_SCRIPT: &str = "#!/bin/sh
/bin/busybox --install -s /bin
mount -t proc proc /proc
mount -t sysfs sysfs /sys
mount -t devtmpfs devtmpfs /dev 2>/dev/null
echo
echo \"Bridge-V: BusyBox $(busybox | head -1 | cut -d' ' -f2) on Linux $(uname -r)\"
export HOME=/root
command -v cttyhack >/dev/null && exec setsid cttyhack sh
exec sh
";
