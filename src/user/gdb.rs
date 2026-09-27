//! GDB remote stub for user mode (Phase 10, D59): `bridgev run --gdb PORT prog` waits for a
//! debugger on 127.0.0.1:PORT and serves the GDB Remote Serial Protocol.
//!
//! Execution under the debugger goes through the interpreter one instruction at a time (a
//! one-instruction block, `exec_block`), whatever `--engine` says. So single steps are exact
//! and software breakpoints need no code patching: `continue` steps until the pc hits one.
//! Syscalls, signals and exits are handled as in a normal run. Threads are not supported
//! under the debugger (clone fails with EAGAIN).
//!
//! Packets served: `?`, `g`/`G` (x0–x31, pc), `p`/`P` (0–31 x, 32 pc, 33–64 f0–f31),
//! `m`/`M`, `Z0`/`z0`, `c`, `s`, `k`, `D`, `qSupported`, `qAttached`, `qC`, `q[fs]ThreadInfo`,
//! `qXfer:features:read:target.xml` (riscv:rv64), `H*`. Anything else gets the empty
//! "unsupported" reply.

use std::collections::BTreeSet;
use std::io::{BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};

use anyhow::{Context, Result};

use super::guest_signal as gs;
use super::loader::Process;
use super::syscall::{SysOut, Syscalls};
use super::{RunResult, signal_for};
use crate::interp::{BlockExit, Engine, build_block_max, exec_block};

const TARGET_XML: &str = r#"<?xml version="1.0"?>
<!DOCTYPE target SYSTEM "gdb-target.dtd">
<target version="1.0">
<architecture>riscv:rv64</architecture>
</target>"#;

/// How the guest stopped (the reply to `?`, `c` and `s`).
enum Halt {
    /// Stopped with this signal (5 = SIGTRAP for breakpoints and steps).
    Signal(u8),
    /// The process exited with this status.
    Exited(i32),
    /// Killed by this signal.
    Killed(u8),
}

impl Halt {
    fn reply(&self) -> String {
        match self {
            Halt::Signal(s) => format!("S{s:02x}"),
            Halt::Exited(c) => format!("W{:02x}", *c as u8),
            Halt::Killed(s) => format!("X{s:02x}"),
        }
    }
}

struct Stub {
    p: Process,
    sys: Syscalls,
    engine: Box<dyn Engine>,
    breakpoints: BTreeSet<u64>,
    /// A signal waiting to be delivered to the guest when it resumes (a fault stops first).
    pending_fault: Option<gs::SigInfo>,
    exited: Option<Halt>,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

fn read_packet(r: &mut impl Read, w: &mut impl Write) -> Result<Option<String>> {
    let mut b = [0u8; 1];
    // Skip acks and anything before '$'; Ctrl-C (0x03) is ignored (no async interrupt).
    loop {
        if r.read(&mut b)? == 0 {
            return Ok(None);
        }
        if b[0] == b'$' {
            break;
        }
    }
    let mut body = Vec::new();
    loop {
        r.read_exact(&mut b)?;
        if b[0] == b'#' {
            break;
        }
        body.push(b[0]);
    }
    let mut sum = [0u8; 2];
    r.read_exact(&mut sum)?;
    w.write_all(b"+")?;
    Ok(Some(String::from_utf8_lossy(&body).into_owned()))
}

fn send(w: &mut impl Write, body: &str) -> Result<()> {
    let sum = body.bytes().fold(0u8, |a, b| a.wrapping_add(b));
    w.write_all(format!("${body}#{sum:02x}").as_bytes())?;
    w.flush()?;
    Ok(())
}

impl Stub {
    /// Execute one guest instruction (or deliver a pending signal first). `Some` when the
    /// guest stops for the debugger or ends.
    fn step(&mut self) -> Option<Halt> {
        if let Some(h) = self.exited.take() {
            return Some(h);
        }
        if let Some(info) = self.pending_fault.take()
            && let gs::Delivery::Fatal(sig) = gs::deliver(&mut self.p, info, true)
        {
            return Some(Halt::Killed(sig as u8));
        }
        let b = build_block_max(self.p.cpu.pc, &self.p.mem, 1);
        let exit = exec_block(
            &mut self.p.cpu,
            &mut self.p.mem,
            &b.insns,
            b.fetch_fault,
            false,
        );
        if !self.p.mem.smc_pages.is_empty() {
            self.p.mem.smc_pages.clear(); // no translations to invalidate here
        }
        match exit {
            BlockExit::Continue | BlockExit::Flush | BlockExit::Wfi => None,
            BlockExit::Trap(e) => {
                // Report the fault to the debugger; it is delivered when the guest resumes.
                let info = gs::fault_info(&e, self.p.cpu.pc, &self.p.mem);
                self.pending_fault = Some(info);
                Some(Halt::Signal(signal_for(&e) as u8))
            }
            BlockExit::Ecall => match self.sys.dispatch(&mut self.p, self.engine.as_mut()) {
                SysOut::Ret(v) => {
                    self.p.cpu.x[10] = v as u64;
                    self.p.cpu.pc += 4;
                    self.p.cpu.icount += 1;
                    self.pending_signals()
                }
                SysOut::NoRet => {
                    self.p.cpu.icount += 1;
                    None
                }
                SysOut::Exit(c) | SysOut::ThreadExit(c) => Some(Halt::Exited(c)),
                SysOut::Clone(_) => {
                    self.p.cpu.x[10] = -(libc::EAGAIN as i64) as u64;
                    self.p.cpu.pc += 4;
                    None
                }
                SysOut::Block(nr, a) => {
                    // SAFETY: arguments checked and translated by the syscall layer.
                    let r = unsafe { libc::syscall(nr, a[0], a[1], a[2], a[3], a[4], a[5]) };
                    let r = if r == -1 {
                        -(std::io::Error::last_os_error()
                            .raw_os_error()
                            .unwrap_or(libc::EINVAL) as i64)
                    } else {
                        r
                    };
                    self.p.cpu.x[10] = r as u64;
                    self.p.cpu.pc += 4;
                    self.p.cpu.icount += 1;
                    None
                }
            },
        }
    }

    fn pending_signals(&mut self) -> Option<Halt> {
        while let Some(sig) = gs::take_pending(&mut self.p) {
            let info = gs::SigInfo {
                signo: sig,
                code: gs::SI_TKILL,
                addr: 0,
                pid: std::process::id(),
                // SAFETY: getuid has no preconditions.
                uid: unsafe { libc::getuid() },
            };
            if let gs::Delivery::Fatal(sig) = gs::deliver(&mut self.p, info, false) {
                return Some(Halt::Killed(sig as u8));
            }
        }
        None
    }

    fn cont(&mut self) -> Halt {
        // Step off a breakpoint at the current pc first.
        if let Some(h) = self.step() {
            return h;
        }
        loop {
            if self.breakpoints.contains(&self.p.cpu.pc) {
                return Halt::Signal(5);
            }
            if let Some(h) = self.step() {
                return h;
            }
        }
    }

    fn reg(&self, n: usize) -> Option<u64> {
        let c = &self.p.cpu;
        match n {
            0..=31 => Some(c.x[n]),
            32 => Some(c.pc),
            33..=64 => Some(c.f[n - 33]),
            _ => None,
        }
    }

    fn set_reg(&mut self, n: usize, v: u64) -> bool {
        let c = &mut self.p.cpu;
        match n {
            0 => true,
            1..=31 => {
                c.x[n] = v;
                true
            }
            32 => {
                c.pc = v;
                true
            }
            33..=64 => {
                c.f[n - 33] = v;
                true
            }
            _ => false,
        }
    }

    /// Handle one packet; `Err(result)` ends the session with that process result.
    fn packet(&mut self, pkt: &str) -> std::result::Result<String, Option<Halt>> {
        let (cmd, rest) = pkt.split_at(pkt.len().min(1));
        Ok(match cmd {
            "?" => Halt::Signal(5).reply(),
            "g" => {
                let v: Vec<u8> = (0..33)
                    .flat_map(|i| self.reg(i).unwrap().to_le_bytes())
                    .collect();
                hex(&v)
            }
            "G" => match unhex(rest) {
                Some(b) if b.len() >= 33 * 8 => {
                    for i in 0..33 {
                        let v = u64::from_le_bytes(b[8 * i..8 * i + 8].try_into().unwrap());
                        self.set_reg(i, v);
                    }
                    "OK".into()
                }
                _ => "E01".into(),
            },
            "p" => match usize::from_str_radix(rest, 16)
                .ok()
                .and_then(|n| self.reg(n))
            {
                Some(v) => hex(&v.to_le_bytes()),
                None => "E01".into(),
            },
            "P" => {
                let parsed = rest.split_once('=').and_then(|(n, v)| {
                    let b = unhex(v)?;
                    let n = usize::from_str_radix(n, 16).ok()?;
                    (b.len() == 8).then(|| (n, u64::from_le_bytes(b.try_into().unwrap())))
                });
                match parsed {
                    Some((n, v)) if self.set_reg(n, v) => "OK".into(),
                    _ => "E01".into(),
                }
            }
            "m" => {
                let parsed = rest.split_once(',').and_then(|(a, l)| {
                    Some((
                        u64::from_str_radix(a, 16).ok()?,
                        u64::from_str_radix(l, 16).ok()?,
                    ))
                });
                match parsed {
                    Some((a, l)) => {
                        let bytes: Option<Vec<u8>> = (0..l.min(4096))
                            .map(|i| self.p.mem.load(a + i, 1).ok().map(|v| v as u8))
                            .collect();
                        bytes.map_or("E01".into(), |b| hex(&b))
                    }
                    None => "E01".into(),
                }
            }
            "M" => {
                let parsed = rest.split_once(':').and_then(|(al, data)| {
                    let (a, _) = al.split_once(',')?;
                    Some((u64::from_str_radix(a, 16).ok()?, unhex(data)?))
                });
                match parsed {
                    Some((a, b))
                        if self.p.mem.write_bytes(crate::mem::GuestVirt(a), &b).is_ok() =>
                    {
                        "OK".into()
                    }
                    _ => "E01".into(),
                }
            }
            "Z" | "z" => {
                let mut f = rest.split(',');
                match (
                    f.next(),
                    f.next().and_then(|a| u64::from_str_radix(a, 16).ok()),
                ) {
                    (Some("0"), Some(a)) => {
                        if cmd == "Z" {
                            self.breakpoints.insert(a);
                        } else {
                            self.breakpoints.remove(&a);
                        }
                        "OK".into()
                    }
                    _ => String::new(),
                }
            }
            "c" => {
                let h = self.cont();
                if matches!(h, Halt::Exited(_) | Halt::Killed(_)) {
                    return Err(Some(h));
                }
                h.reply()
            }
            "s" => {
                let h = self.step().unwrap_or(Halt::Signal(5));
                if matches!(h, Halt::Exited(_) | Halt::Killed(_)) {
                    return Err(Some(h));
                }
                h.reply()
            }
            "k" => return Err(Some(Halt::Killed(9))),
            "D" => return Err(None),
            "H" => "OK".into(),
            "q" => {
                if rest.starts_with("Supported") {
                    "PacketSize=4000;swbreak+;qXfer:features:read+".into()
                } else if rest == "Attached" {
                    "1".into()
                } else if rest == "C" {
                    "QC1".into()
                } else if rest == "fThreadInfo" {
                    "m1".into()
                } else if rest == "sThreadInfo" {
                    "l".into()
                } else if let Some(r) = rest.strip_prefix("Xfer:features:read:target.xml:") {
                    let (o, l) = r.split_once(',').unwrap_or(("0", "0"));
                    let o = usize::from_str_radix(o, 16)
                        .unwrap_or(0)
                        .min(TARGET_XML.len());
                    let l = usize::from_str_radix(l, 16).unwrap_or(0);
                    let end = (o + l).min(TARGET_XML.len());
                    format!(
                        "{}{}",
                        if end == TARGET_XML.len() { "l" } else { "m" },
                        &TARGET_XML[o..end]
                    )
                } else {
                    String::new()
                }
            }
            _ => String::new(),
        })
    }
}

/// Serve one debugger connection, then (on detach) finish the run normally in the stub.
pub fn serve(p: Process, port: u16, engine: Box<dyn Engine>, strace: bool) -> Result<RunResult> {
    let listener = TcpListener::bind(("127.0.0.1", port))
        .with_context(|| format!("listening on 127.0.0.1:{port}"))?;
    eprintln!("bridgev: waiting for gdb on 127.0.0.1:{port} (target remote :{port})");
    let (stream, _) = listener.accept().context("accepting the debugger")?;
    let _ = stream.set_nodelay(true);
    let mut w: TcpStream = stream.try_clone()?;
    let mut r = BufReader::new(stream);
    let mut sys = Syscalls::default();
    sys.strace = strace;
    let mut stub = Stub {
        p,
        sys,
        engine,
        breakpoints: BTreeSet::new(),
        pending_fault: None,
        exited: None,
    };
    let end = loop {
        let Some(pkt) = read_packet(&mut r, &mut w)? else {
            break None; // the debugger went away: run on
        };
        match stub.packet(&pkt) {
            Ok(reply) => send(&mut w, &reply)?,
            Err(Some(h)) => {
                send(&mut w, &h.reply())?;
                break Some(h);
            }
            Err(None) => {
                send(&mut w, "OK")?;
                break None;
            }
        }
    };
    let end = match end {
        Some(h) => h,
        // Detached: run to the end without the debugger.
        None => {
            stub.breakpoints.clear();
            loop {
                if let Some(h) = stub.step() {
                    match h {
                        Halt::Signal(_) => continue, // the fault is delivered on the next step
                        h => break h,
                    }
                }
            }
        }
    };
    let exit_code = match end {
        Halt::Exited(c) => c,
        Halt::Killed(s) | Halt::Signal(s) => 128 + s as i32,
    };
    Ok(RunResult {
        exit_code,
        icount: stub.p.cpu.icount,
        engine_stats: String::from("gdb: interpreted one instruction at a time"),
    })
}
