//! Guest signals (Phase 10, D56): handlers registered with rt_sigaction run on an
//! `rt_sigframe` built on the guest stack in Linux's riscv64 layout, and return through a
//! sigreturn trampoline. Sources are synchronous faults (SIGSEGV, SIGBUS, SIGILL, SIGTRAP) and
//! signals a thread sends to its own process (kill, tkill, tgkill: `raise`, `abort`).
//! Asynchronous host signals are not forwarded.
//!
//! Frame layout (arch/riscv/kernel/signal.c, uapi asm/ucontext.h and asm/sigcontext.h):
//! ```text
//!   frame + 0     siginfo_t (128): si_signo, si_errno, si_code, then si_addr / si_pid, si_uid
//!   frame + 128   ucontext: uc_flags, uc_link, uc_stack (24), uc_sigmask (8), 120 unused,
//!   frame + 304   uc_mcontext (16-aligned): sc_regs {pc, x1..x31} (256),
//!   frame + 560                            sc_fpregs.d {f[32] (256), fcsr} (528-byte union)
//!   size 1088
//! ```

use crate::cpu::state::CpuState;
use crate::cpu::trap::{Exception, cause};
use crate::mem::direct::DirectMem;

use super::loader::Process;

pub const NSIG: usize = 64;
pub const SIGILL: u32 = 4;
pub const SIGTRAP: u32 = 5;
pub const SIGBUS: u32 = 7;
pub const SIGKILL: u32 = 9;
pub const SIGSEGV: u32 = 11;
pub const SIGSTOP: u32 = 19;

const SIG_DFL: u64 = 0;
const SIG_IGN: u64 = 1;
const SA_SIGINFO: u64 = 4;
const SA_ONSTACK: u64 = 0x0800_0000;
const SA_NODEFER: u64 = 0x4000_0000;
const SA_RESETHAND: u64 = 0x8000_0000;

/// si_code values.
pub const SI_USER: i32 = 0;
pub const SI_TKILL: i32 = -6;

/// The sigreturn trampoline: one page just above the stack, `li a7, 139; ecall`.
pub const SIGTRAMP: u64 = super::loader::STACK_TOP;
pub const SIGTRAMP_CODE: [u32; 2] = [0x08b0_0893, 0x0000_0073];

const FRAME_SIZE: u64 = 1088;
const UC: u64 = 128;
const UC_STACK: u64 = UC + 16;
const UC_SIGMASK: u64 = UC + 40;
const MCTX: u64 = UC + 176;
const FPREGS: u64 = MCTX + 256;

/// `struct sigaction` as the riscv64 kernel sees it (no sa_restorer): 24 bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SigAction {
    pub handler: u64,
    pub flags: u64,
    pub mask: u64,
}

/// Why a signal is delivered (fills siginfo).
#[derive(Clone, Copy, Debug)]
pub struct SigInfo {
    pub signo: u32,
    pub code: i32,
    /// si_addr for faults; (si_pid, si_uid) for kill/tgkill.
    pub addr: u64,
    pub pid: u32,
    pub uid: u32,
}

fn bit(sig: u32) -> u64 {
    1 << (sig - 1)
}

/// Signals that can never be blocked or caught.
pub const UNBLOCKABLE: u64 = (1 << (SIGKILL - 1)) | (1 << (SIGSTOP - 1));

/// The signal a guest exception raises (Linux's riscv trap handlers), with si_code and si_addr.
pub fn fault_info(e: &Exception, pc: u64, mem: &DirectMem) -> SigInfo {
    let (signo, code, addr) = match e.cause {
        cause::ILLEGAL_INSN => (SIGILL, 1, pc), // ILL_ILLOPC
        cause::BREAKPOINT => (SIGTRAP, 1, pc),  // TRAP_BRKPT
        cause::LOAD_MISALIGNED | cause::STORE_MISALIGNED | cause::INSN_MISALIGNED => {
            (SIGBUS, 1, e.tval) // BUS_ADRALN
        }
        _ => {
            // SEGV_MAPERR (no mapping) or SEGV_ACCERR (mapped, wrong permissions).
            let code = if mem.is_mapped(e.tval) { 2 } else { 1 };
            (SIGSEGV, code, e.tval)
        }
    };
    SigInfo {
        signo,
        code,
        addr,
        pid: 0,
        uid: 0,
    }
}

/// Default action: true = terminate the process (with or without a core dump), false = ignore.
fn default_terminates(sig: u32) -> bool {
    // SIGCHLD, SIGCONT, SIGURG, SIGWINCH are ignored by default.
    !matches!(sig, 17 | 18 | 23 | 28)
}

/// Outcome of trying to deliver a signal.
pub enum Delivery {
    /// A handler frame was pushed (or the signal was ignored): continue.
    Continue,
    /// The process dies from the signal: exit status 128 + sig.
    Fatal(u32),
}

fn put(mem: &mut DirectMem, addr: u64, size: u64, val: u64) -> Result<(), ()> {
    mem.store(addr, size, val).map_err(|_| ())
}

/// Deliver `info` to the running thread in `p` (its registers are `p.cpu`). `forced`: a
/// synchronous fault, which kills the process if the signal is blocked or ignored (as Linux's
/// force_sig does).
pub fn deliver(p: &mut Process, info: SigInfo, forced: bool) -> Delivery {
    let sig = info.signo;
    let act = p.sigactions[sig as usize];
    let blocked = p.sigmask & bit(sig) != 0;
    if act.handler == SIG_IGN || (act.handler == SIG_DFL && !default_terminates(sig)) {
        return if forced {
            Delivery::Fatal(sig)
        } else {
            Delivery::Continue
        };
    }
    if act.handler == SIG_DFL || (forced && blocked) {
        return Delivery::Fatal(sig);
    }
    // The handler's stack: the alternate one if asked for and not already on it.
    let sp = p.cpu.x[2];
    let [ss_sp, ss_flags, ss_size] = p.altstack;
    let on_alt = ss_size != 0 && sp.wrapping_sub(ss_sp) < ss_size;
    let top = if act.flags & SA_ONSTACK != 0 && ss_size != 0 && ss_flags & 2 == 0 && !on_alt {
        ss_sp + ss_size
    } else {
        sp
    };
    let frame = (top - FRAME_SIZE) & !15;
    if push_frame(p, frame, &info).is_err() {
        // Linux: a frame that cannot be written is fatal (SIGSEGV).
        return Delivery::Fatal(SIGSEGV);
    }
    let cpu = &mut p.cpu;
    cpu.x[10] = sig as u64;
    cpu.x[11] = if act.flags & SA_SIGINFO != 0 {
        frame
    } else {
        0
    };
    cpu.x[12] = if act.flags & SA_SIGINFO != 0 {
        frame + UC
    } else {
        0
    };
    cpu.x[1] = SIGTRAMP;
    cpu.x[2] = frame;
    cpu.pc = act.handler;
    let mut mask = p.sigmask | act.mask;
    if act.flags & SA_NODEFER == 0 {
        mask |= bit(sig);
    }
    p.sigmask = mask & !UNBLOCKABLE;
    if act.flags & SA_RESETHAND != 0 {
        p.sigactions[sig as usize] = SigAction::default();
    }
    Delivery::Continue
}

fn push_frame(p: &mut Process, frame: u64, info: &SigInfo) -> Result<(), ()> {
    let cpu: &CpuState = &p.cpu;
    let (regs, fregs, fcsr) = (cpu.x, cpu.f, ((cpu.frm as u64) << 5) | cpu.fflags as u64);
    let (pc, mask, alt) = (cpu.pc, p.sigmask, p.altstack);
    let m = &mut p.mem;
    for off in (0..FRAME_SIZE).step_by(8) {
        put(m, frame + off, 8, 0)?;
    }
    put(m, frame, 4, info.signo as u64)?;
    put(m, frame + 8, 4, info.code as u32 as u64)?;
    if matches!(info.signo, SIGSEGV | SIGBUS | SIGILL | SIGTRAP) && info.code > 0 {
        put(m, frame + 16, 8, info.addr)?;
    } else {
        put(m, frame + 16, 4, info.pid as u64)?;
        put(m, frame + 20, 4, info.uid as u64)?;
    }
    put(m, frame + UC_STACK, 8, alt[0])?;
    put(m, frame + UC_STACK + 8, 4, alt[1])?;
    put(m, frame + UC_STACK + 16, 8, alt[2])?;
    put(m, frame + UC_SIGMASK, 8, mask)?;
    put(m, frame + MCTX, 8, pc)?;
    for (i, &x) in regs.iter().enumerate().skip(1) {
        put(m, frame + MCTX + 8 * i as u64, 8, x)?;
    }
    for (i, &f) in fregs.iter().enumerate() {
        put(m, frame + FPREGS + 8 * i as u64, 8, f)?;
    }
    put(m, frame + FPREGS + 256, 4, fcsr)
}

/// rt_sigreturn: restore the registers, FP state and signal mask saved in the frame at `sp`.
/// `false` if the frame is unreadable (the caller kills the process with SIGSEGV).
pub fn sigreturn(p: &mut Process) -> bool {
    let frame = p.cpu.x[2];
    let get = |p: &Process, off: u64, size: u64| p.mem.load(frame + off, size).ok();
    let mut regs = [0u64; 32];
    let mut fregs = [0u64; 32];
    for i in 0..32 {
        let Some(v) = get(p, MCTX + 8 * i as u64, 8) else {
            return false;
        };
        regs[i] = v;
        let Some(f) = get(p, FPREGS + 8 * i as u64, 8) else {
            return false;
        };
        fregs[i] = f;
    }
    let (Some(fcsr), Some(mask)) = (get(p, FPREGS + 256, 4), get(p, UC_SIGMASK, 8)) else {
        return false;
    };
    let cpu = &mut p.cpu;
    cpu.pc = regs[0];
    cpu.x[1..].copy_from_slice(&regs[1..]);
    cpu.f = fregs;
    cpu.fflags = (fcsr & 0x1f) as u8;
    cpu.frm = ((fcsr >> 5) & 7) as u8;
    p.sigmask = mask & !UNBLOCKABLE;
    true
}

/// The lowest pending signal the running thread does not block, removed from the pending set.
pub fn take_pending(p: &mut Process) -> Option<u32> {
    let ready = p.sigpending & !p.sigmask;
    if ready == 0 {
        return None;
    }
    let sig = ready.trailing_zeros() + 1;
    p.sigpending &= !bit(sig);
    Some(sig)
}

/// Mark `sig` pending for the running thread.
pub fn raise(p: &mut Process, sig: u32) {
    if (1..=NSIG as u32).contains(&sig) {
        p.sigpending |= bit(sig);
    }
}
