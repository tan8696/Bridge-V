//! Host SIGSEGV handling for JIT code (CLAUDE.md §14.1, P2.7).
//!
//! JIT code accesses guest memory unchecked (`[rbx + addr]`); host page protection mirrors the
//! guest permissions, so a bad guest access raises SIGSEGV. If the faulting RIP lies in the
//! code buffer this thread is running, the handler records `fault_rip`/`fault_addr` in the
//! `CpuState` (found through RBP, which JIT code always keeps at `&CpuState + 128`) and
//! resumes at the `fault_exit` trampoline, which returns to the dispatcher. The dispatcher
//! then maps RIP → TB → guest instruction (`pcmap`) and raises the guest exception, so the
//! fault is precise. Any other SIGSEGV is passed on to the previously installed handler (Rust's
//! stack-overflow handler) or the default action.
//!
//! Guest signal delivery (rt_sigframe, sigreturn) is a Phase 10 stretch goal.

use std::cell::Cell;
use std::mem::MaybeUninit;
use std::sync::Once;

use crate::cpu::state::CpuState;

thread_local! {
    /// (code buffer start, end, fault_exit address) of the JIT running on this thread.
    static JIT_RANGE: Cell<(u64, u64, u64)> = const { Cell::new((0, 0, 0)) };
}

static INSTALL: Once = Once::new();
static mut OLD_ACTION: MaybeUninit<libc::sigaction> = MaybeUninit::zeroed();

/// Install the process-wide SIGSEGV handler (idempotent).
pub fn install() {
    INSTALL.call_once(|| {
        // SAFETY: sigaction with a fully initialised struct; OLD_ACTION is written once here,
        // before the handler can run, and only read afterwards.
        unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = handler as *const () as usize;
            sa.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK;
            libc::sigemptyset(&mut sa.sa_mask);
            let old = &raw mut OLD_ACTION;
            let rc = libc::sigaction(libc::SIGSEGV, &sa, (*old).as_mut_ptr());
            assert_eq!(
                rc,
                0,
                "sigaction(SIGSEGV): {}",
                std::io::Error::last_os_error()
            );
        }
    });
}

/// Declare that this thread runs JIT code in `[start, end)` with the given `fault_exit`.
pub fn set_jit_range(start: u64, end: u64, fault_exit: u64) {
    JIT_RANGE.with(|c| c.set((start, end, fault_exit)));
}

/// This thread no longer runs JIT code.
pub fn clear_jit_range() {
    JIT_RANGE.with(|c| c.set((0, 0, 0)));
}

/// ucontext `gregs` index of each host register, in `Reg` order (RAX, RCX, …, R15).
const GREG_OF: [libc::c_int; 16] = [
    libc::REG_RAX,
    libc::REG_RCX,
    libc::REG_RDX,
    libc::REG_RBX,
    libc::REG_RSP,
    libc::REG_RBP,
    libc::REG_RSI,
    libc::REG_RDI,
    libc::REG_R8,
    libc::REG_R9,
    libc::REG_R10,
    libc::REG_R11,
    libc::REG_R12,
    libc::REG_R13,
    libc::REG_R14,
    libc::REG_R15,
];

extern "C" fn handler(sig: libc::c_int, info: *mut libc::siginfo_t, ctx: *mut libc::c_void) {
    let (start, end, fault_exit) = JIT_RANGE.try_with(|c| c.get()).unwrap_or((0, 0, 0));
    // SAFETY: the kernel passes a valid ucontext_t for SA_SIGINFO handlers.
    let uc = unsafe { &mut *(ctx as *mut libc::ucontext_t) };
    let gregs = &mut uc.uc_mcontext.gregs;
    let rip = gregs[libc::REG_RIP as usize] as u64;
    if rip >= start && rip < end {
        let cpu = (gregs[libc::REG_RBP as usize] as u64).wrapping_sub(128) as *mut CpuState;
        // SAFETY: inside JIT code RBP is always the biased CpuState pointer (§8.3), and the
        // dispatcher that entered the code holds that CpuState exclusively. Only plain field
        // stores happen here (async-signal-safe).
        unsafe {
            (*cpu).fault_rip = rip;
            (*cpu).fault_addr = (*info).si_addr() as u64;
            // Snapshot every GPR in `Reg` numbering: the register allocator may hold dirty
            // guest registers in them (state maps, §15).
            for (n, &greg) in GREG_OF.iter().enumerate() {
                (*cpu).fault_regs[n] = gregs[greg as usize] as u64;
            }
        }
        gregs[libc::REG_RIP as usize] = fault_exit as i64;
        return;
    }
    // Not ours: chain to the previous handler, or fall back to the default action (the fault
    // re-executes on return and kills the process with SIGSEGV).
    // SAFETY: OLD_ACTION was initialised by `install` before this handler was registered.
    unsafe {
        let old: libc::sigaction = std::ptr::read((&raw const OLD_ACTION).cast());
        let h = old.sa_sigaction;
        if h == libc::SIG_DFL || h == libc::SIG_IGN {
            let mut dfl: libc::sigaction = std::mem::zeroed();
            dfl.sa_sigaction = libc::SIG_DFL;
            libc::sigaction(sig, &dfl, std::ptr::null_mut());
        } else if old.sa_flags & libc::SA_SIGINFO != 0 {
            let f: extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut libc::c_void) =
                std::mem::transmute(h);
            f(sig, info, ctx);
        } else {
            let f: extern "C" fn(libc::c_int) = std::mem::transmute(h);
            f(sig);
        }
    }
}
