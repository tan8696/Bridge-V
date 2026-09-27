//! Guest threads (Phase 10, D55). Every guest thread is a host thread with its own `CpuState`
//! and its own engine (and so its own translation cache). Guest code runs under one fair lock
//! (`Gil`): a thread holds it for one slice or until its next syscall. Blocking syscalls
//! (futex waits, sleeps) run without it, and futexes are the host's, on the guest word's host
//! address.
//!
//! So threads interleave but never execute guest code in parallel. That keeps AMOs and LR/SC
//! atomic without host atomics, and lets the shared `DirectMem` and syscall state live behind
//! the lock unchanged. The price is no parallel speedup.
//!
//! Two per-thread caches are kept coherent across threads:
//! - A thread whose engine did not see a code write (another thread made it) flushes its
//!   translations (`DirectMem::smc_epoch`).
//! - With `--mem=softmmu`, a thread flushes its TLB after another thread changed the mappings.
//!
//! A switch between threads clears the LR reservation, so an SC may fail spuriously, which the
//! ISA allows.

use std::collections::HashMap;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use anyhow::{Result, anyhow};

use super::guest_signal as gs;
use super::loader::Process;
use super::syscall::{
    CLONE_CHILD_CLEARTID, CLONE_CHILD_SETTID, CLONE_PARENT_SETTID, CLONE_SETTLS, CloneArgs, SysOut,
    Syscalls,
};
use super::{RunOptions, RunResult, signal_for};
use crate::cpu::state::CpuState;
use crate::interp::{Engine, Env, Stop};
use crate::jit::make_engine;
use crate::mem::tlb;

/// Guest instructions a thread runs before letting the others in (when it is not alone):
/// about 4 ms of JIT time. Handing the lock over costs a host thread wake-up (tens of µs),
/// which a 100k slice made dominant (MT CoreMark ran at half the single-thread speed).
const SLICE: u64 = 10_000_000;

/// A fair (FIFO ticket) lock: a thread that releases it and asks again queues behind the
/// threads already waiting, so guest threads take turns.
pub struct Gil<T> {
    data: Mutex<T>,
    /// (next ticket, ticket being served)
    turn: Mutex<(u64, u64)>,
    cv: Condvar,
}

pub struct GilGuard<'a, T> {
    gil: &'a Gil<T>,
    guard: Option<MutexGuard<'a, T>>,
}

impl<T> Gil<T> {
    pub fn new(v: T) -> Self {
        Gil {
            data: Mutex::new(v),
            turn: Mutex::new((0, 0)),
            cv: Condvar::new(),
        }
    }

    pub fn lock(&self) -> GilGuard<'_, T> {
        let mut t = self.turn.lock().unwrap();
        let me = t.0;
        t.0 += 1;
        while t.1 != me {
            t = self.cv.wait(t).unwrap();
        }
        drop(t);
        // Only the ticket holder gets here: uncontended.
        GilGuard {
            gil: self,
            guard: Some(self.data.lock().unwrap()),
        }
    }
}

impl<T> Deref for GilGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.guard.as_ref().expect("held")
    }
}

impl<T> DerefMut for GilGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.guard.as_mut().expect("held")
    }
}

impl<T> Drop for GilGuard<'_, T> {
    fn drop(&mut self) {
        drop(self.guard.take());
        self.gil.turn.lock().unwrap().1 += 1;
        self.gil.cv.notify_all();
    }
}

/// State shared by the guest threads, behind the GIL.
pub struct Shared {
    /// The address space and process data; `p.cpu` holds the running thread's registers.
    pub p: Process,
    pub sys: Syscalls,
    /// Threads not yet exited.
    live: usize,
    next_tid: i64,
    /// Retired instructions per thread (updated when a thread leaves the lock).
    icounts: HashMap<i64, u64>,
    /// Bumped when a syscall changes the mappings (softmmu: other threads flush their TLBs).
    map_epoch: u64,
}

/// A running guest process.
pub struct Proc {
    gil: Gil<Shared>,
    result: Mutex<Option<Result<RunResult>>>,
    done: Condvar,
    opts: RunOptions,
    env: Env,
    /// More than one thread was ever created (then thread switches clear LR reservations).
    multi: AtomicBool,
}

impl Proc {
    fn finish(&self, r: Result<RunResult>) {
        let mut g = self.result.lock().unwrap();
        if g.is_none() {
            *g = Some(r);
        }
        self.done.notify_all();
    }

    fn finished(&self) -> bool {
        self.result.lock().unwrap().is_some()
    }
}

/// Run the loaded process `p` to completion: its first thread gets the process id as its tid.
/// Returns when the process ends (exit_group, the last thread's exit, a fatal fault, the
/// instruction limit or a lockstep divergence); threads still running are left to the process
/// exit.
pub fn run_process(p: Process, opts: RunOptions, env: Env, strace: bool) -> Result<RunResult> {
    let cpu0 = p.cpu.clone();
    let tid0 = p.tid;
    let mut sys = Syscalls::default();
    sys.strace = strace;
    let proc = Arc::new(Proc {
        gil: Gil::new(Shared {
            p,
            sys,
            live: 1,
            next_tid: tid0 + 1,
            icounts: HashMap::new(),
            map_epoch: 0,
        }),
        result: Mutex::new(None),
        done: Condvar::new(),
        opts,
        env,
        multi: AtomicBool::new(false),
    });
    spawn(proc.clone(), cpu0, tid0, 0, 0)?;
    let mut r = proc.result.lock().unwrap();
    while r.is_none() {
        r = proc.done.wait(r).unwrap();
    }
    r.take().expect("checked")
}

fn spawn(
    proc: Arc<Proc>,
    cpu: Box<CpuState>,
    tid: i64,
    clear_tid: u64,
    sigmask: u64,
) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name(format!("guest-{tid}"))
        .stack_size(16 << 20)
        .spawn(move || guest_thread(&proc, cpu, tid, clear_tid, sigmask))
        .map(|_| ())
}

/// Per-thread signal state, swapped into `Process` while the thread runs.
#[derive(Default)]
struct ThreadSignals {
    mask: u64,
    pending: u64,
    altstack: [u64; 3],
}

fn stats(engine: &dyn Engine, cpu: &CpuState, threads: usize) -> String {
    let mut s = engine.stats();
    if cpu.softmmu != 0 {
        s += &format!("\nsoftmmu: {} TLB fills", cpu.tlb_fills);
    }
    if threads > 1 {
        s += &format!("\nthreads: {threads} guest threads (statistics above: the last one)");
    }
    s
}

/// Store a 32-bit tid into guest memory (errors ignored, as the kernel does).
fn put_tid(p: &mut Process, addr: u64, v: u32) {
    if addr != 0 {
        let _ = p.mem.store(addr, 4, v as u64);
    }
}

fn guest_thread(
    proc: &Arc<Proc>,
    mut cpu: Box<CpuState>,
    tid: i64,
    mut clear_tid: u64,
    sigmask: u64,
) {
    let mut sigs = ThreadSignals {
        mask: sigmask,
        ..Default::default()
    };
    let mut engine = match make_engine(proc.opts.engine, &proc.opts.jit) {
        Ok(e) => e,
        Err(e) => return proc.finish(Err(e.into())),
    };
    if proc.opts.reg_stats && !engine.enable_reg_stats() {
        return proc.finish(Err(anyhow!("--stats=regs needs --engine interp")));
    }
    let limit = proc.opts.max_insns.unwrap_or(u64::MAX);
    let (mut map_seen, mut smc_seen) = (0, 0);
    let mut first = true;
    loop {
        let mut g = proc.gil.lock();
        if proc.finished() {
            return;
        }
        let s = &mut *g;
        if first {
            (map_seen, smc_seen) = (s.map_epoch, s.p.mem.smc_epoch);
            first = false;
        }
        // Other threads wrote code or changed the mappings since this one last ran.
        if s.p.mem.smc_epoch != smc_seen {
            engine.flush();
        }
        if cpu.softmmu != 0 && s.map_epoch != map_seen {
            tlb::flush_all(&mut cpu);
        }
        std::mem::swap(&mut s.p.cpu, &mut cpu);
        (s.p.tid, s.p.clear_tid) = (tid, clear_tid);
        let sent = s.p.thread_pending.remove(&tid).unwrap_or(0);
        (s.p.sigmask, s.p.sigpending, s.p.altstack) =
            (sigs.mask, sigs.pending | sent, sigs.altstack);
        if proc.multi.load(Ordering::Relaxed) {
            s.p.cpu.res_valid = 0;
        }
        let mut left = limit.saturating_sub(s.p.cpu.icount);
        if s.live > 1 {
            left = left.min(SLICE);
        }
        let mut block = None;
        match engine.run(&mut s.p.cpu, &mut s.p.mem, &proc.env, left) {
            Stop::Ecall => match s.sys.dispatch(&mut s.p, engine.as_mut()) {
                SysOut::Ret(v) => {
                    // The user-mode "page table" is the mmap state: drop cached translations
                    // when it changes (brk, munmap, mremap, mmap, mprotect).
                    if s.p.cpu.softmmu != 0 && matches!(s.p.cpu.x[17], 214 | 215 | 216 | 222 | 226)
                    {
                        tlb::flush_all(&mut s.p.cpu);
                        s.map_epoch += 1;
                    }
                    s.p.cpu.x[10] = v as u64;
                    s.p.cpu.pc += 4; // ECALL has no compressed form
                    s.p.cpu.icount += 1;
                }
                SysOut::NoRet => s.p.cpu.icount += 1,
                SysOut::Exit(code) => {
                    s.p.cpu.icount += 1;
                    return end_process(proc, s, tid, code, engine.as_ref());
                }
                SysOut::ThreadExit(code) => {
                    s.p.cpu.icount += 1;
                    // CLONE_CHILD_CLEARTID / set_tid_address: zero the word and wake a joiner.
                    let ct = s.p.clear_tid;
                    if ct != 0 && s.p.mem.store(ct, 4, 0).is_ok() {
                        let host = (s.p.mem.base() as u64).wrapping_add(ct);
                        // SAFETY: the guest word was just written, so it is mapped at `host`;
                        // FUTEX_WAKE does not access it.
                        unsafe {
                            libc::syscall(libc::SYS_futex, host, libc::FUTEX_WAKE, i32::MAX);
                        }
                    }
                    s.live -= 1;
                    if s.live == 0 {
                        return end_process(proc, s, tid, code, engine.as_ref());
                    }
                    s.icounts.insert(tid, s.p.cpu.icount);
                    return;
                }
                SysOut::Clone(c) => {
                    let r = clone_thread(proc, s, c);
                    s.p.cpu.x[10] = r as u64;
                    s.p.cpu.pc += 4;
                    s.p.cpu.icount += 1;
                }
                SysOut::Block(nr, a) => block = Some((nr, a)),
            },
            Stop::Fault(e) => {
                let info = gs::fault_info(&e, s.p.cpu.pc, &s.p.mem);
                if let gs::Delivery::Fatal(_) = gs::deliver(&mut s.p, info, true) {
                    eprintln!("bridgev: guest {e} at pc {:#x}", s.p.cpu.pc);
                    return end_process(proc, s, tid, 128 + signal_for(&e), engine.as_ref());
                }
            }
            Stop::Limit if s.p.cpu.icount >= limit => {
                return proc.finish(Err(anyhow!(
                    "instruction limit reached ({} instructions)",
                    s.p.cpu.icount
                )));
            }
            Stop::Limit => {}
            Stop::Diverged => return proc.finish(Err(anyhow!("lockstep divergence (see above)"))),
            Stop::Tohost(_) => unreachable!("no tohost in user mode"),
            Stop::Wfi => unreachable!("WFI is illegal in U-mode"),
        }
        // Signals raised by the syscall (raise, abort) or unblocked by it.
        while let Some(sig) = gs::take_pending(&mut s.p) {
            let info = gs::SigInfo {
                signo: sig,
                code: gs::SI_TKILL,
                addr: 0,
                pid: std::process::id(),
                // SAFETY: getuid has no preconditions.
                uid: unsafe { libc::getuid() },
            };
            if let gs::Delivery::Fatal(sig) = gs::deliver(&mut s.p, info, false) {
                eprintln!("bridgev: guest killed by signal {sig}");
                return end_process(proc, s, tid, 128 + sig as i32, engine.as_ref());
            }
        }
        clear_tid = s.p.clear_tid;
        (sigs.mask, sigs.pending, sigs.altstack) = (s.p.sigmask, s.p.sigpending, s.p.altstack);
        smc_seen = s.p.mem.smc_epoch;
        map_seen = s.map_epoch;
        std::mem::swap(&mut s.p.cpu, &mut cpu);
        s.icounts.insert(tid, cpu.icount);
        drop(g);
        if let Some((nr, a)) = block {
            // SAFETY: the syscall's pointer arguments were checked and translated to host
            // addresses of mapped guest memory by the syscall layer.
            let r = unsafe { libc::syscall(nr, a[0], a[1], a[2], a[3], a[4], a[5]) };
            let r = if r == -1 {
                -(std::io::Error::last_os_error()
                    .raw_os_error()
                    .unwrap_or(libc::EINVAL) as i64)
            } else {
                r
            };
            cpu.x[10] = r as u64;
            cpu.pc += 4;
            cpu.icount += 1;
        }
    }
}

/// clone(CLONE_VM | CLONE_THREAD, ...): start a thread with a copy of the caller's registers.
/// Returns the new tid, or -EAGAIN.
fn clone_thread(proc: &Arc<Proc>, s: &mut Shared, c: CloneArgs) -> i64 {
    let tid = s.next_tid;
    let mut child = s.p.cpu.clone();
    child.x[10] = 0;
    if c.stack != 0 {
        child.x[2] = c.stack;
    }
    if c.flags & CLONE_SETTLS != 0 {
        child.x[4] = c.tls;
    }
    child.pc += 4;
    child.icount = 0;
    child.res_valid = 0;
    if c.flags & CLONE_PARENT_SETTID != 0 {
        put_tid(&mut s.p, c.parent_tid, tid as u32);
    }
    if c.flags & CLONE_CHILD_SETTID != 0 {
        put_tid(&mut s.p, c.child_tid, tid as u32);
    }
    let clear = if c.flags & CLONE_CHILD_CLEARTID != 0 {
        c.child_tid
    } else {
        0
    };
    proc.multi.store(true, Ordering::Relaxed);
    if spawn(proc.clone(), child, tid, clear, s.p.sigmask).is_err() {
        return -(libc::EAGAIN as i64);
    }
    s.next_tid += 1;
    s.live += 1;
    tid
}

fn end_process(proc: &Proc, s: &mut Shared, tid: i64, code: i32, engine: &dyn Engine) {
    s.icounts.insert(tid, s.p.cpu.icount);
    let threads = s.next_tid - s.icounts.keys().min().copied().unwrap_or(tid);
    proc.finish(Ok(RunResult {
        exit_code: code,
        icount: s.icounts.values().sum(),
        engine_stats: stats(engine, &s.p.cpu, threads as usize),
    }));
}
