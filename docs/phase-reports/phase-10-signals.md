# Phase 10 mini-report: guest signals

| Field | Value |
|---|---|
| Item | Guest signals (Phase 10 stretch goal 5, `docs/ROADMAP.md`) |
| Status | Complete for synchronous and self-directed signals; host asynchronous signals are not forwarded |
| Date | 2026-09-27 |
| Commit | the commit adding this report |

## 1. Summary
Before this item, `rt_sigaction` was accepted and ignored, and any fault killed the process. Now guest programs can install signal handlers and get them called with Linux's riscv64 signal frame. Handlers can:
- recover from their own faults with `siglongjmp`;
- read `siginfo` (`si_addr`, `si_code`, `si_pid`);
- return normally through `rt_sigreturn`, with every register and the FP state restored;
- run on an alternate stack.

Blocked signals stay pending until unblocked. `SIG_IGN`, `SA_RESETHAND` and `SA_NODEFER` behave as on Linux, and default actions end the process with 128 + signal (e.g. `abort()` → 134). A test program covering all of this prints the same bytes as `qemu-riscv64` under every engine.

## 2. What was built (`src/user/guest_signal.rs`, syscalls in `src/user/syscall.rs`)
- **State:**
  - Process-wide: `sigactions[1..=64]` (handler, flags, mask).
  - Per thread: blocked mask, pending set, alternate stack. The thread layer swaps these into `Process` like the registers, and signals aimed at another thread wait in `thread_pending`.
- **Syscalls:**
  - `rt_sigaction` (the riscv64 `struct sigaction` has no restorer: 24 bytes);
  - `rt_sigprocmask` (block/unblock/setmask; SIGKILL/SIGSTOP stay unblockable);
  - `sigaltstack`;
  - `kill`/`tkill`/`tgkill`: to this process they raise a pending signal, and other pids go to the host `kill`;
  - `rt_sigreturn`.
- **Delivery** (`deliver`):
  - Checked after every syscall and slice, and on every synchronous fault.
  - The frame is 1,088 bytes, 16-aligned, below `sp` or at the top of the alternate stack (`SA_ONSTACK`):
    - `siginfo` (signo, code, then `si_addr` for faults or pid/uid for kills);
    - `ucontext`: `uc_stack`, `uc_sigmask` = the mask to restore, and `uc_mcontext` = {pc, x1..x31} + D-extension state {f0..f31, fcsr}.
  - Registers at entry: `a0` = signal, `a1` = &siginfo, `a2` = &ucontext (with `SA_SIGINFO`), `ra` = the trampoline, `sp` = the frame, `pc` = the handler.
  - The mask gains `sa_mask` plus the signal itself, unless `SA_NODEFER`.
- **Trampoline:** one read-execute page mapped just above the stack (`SIGTRAMP`), holding `li a7, 139; ecall`, like the kernel's vDSO `__vdso_rt_sigreturn`.
- **Faults → signals** (`fault_info`):
  - load/store/fetch page and access faults → SIGSEGV, with `SEGV_MAPERR` if the page is unmapped, else `SEGV_ACCERR`, and `si_addr` = the faulting address;
  - misaligned → SIGBUS;
  - illegal instruction → SIGILL;
  - EBREAK → SIGTRAP.

  A fault whose signal is blocked or ignored is fatal, as with the kernel's `force_sig`. The fault state is precise under every engine (D37/D48), so the handler sees the faulting pc.

## 3. Evidence
`guest/c/signals.c` (4 static builds + 1 dynamic), in `tests/user_programs.rs` under all nine configurations. Expected output, recorded from `qemu-riscv64`, exit status 134:
```
segv load: sig 11 code 1 addr 0x10
segv store to text: sig 11 code 2 addr is main 1
usr1: count 1 info 1 canary 1234567890 d 5.00
while blocked: count 1
after unblock: count 2
usr2: on altstack 1, reset to default 1
sigill: sig 4
ignored usr1
```

## 4. Limitations
- **Asynchronous host signals** (Ctrl-C, `alarm`/`setitimer` timers, `SIGCHLD`) are not forwarded to guest handlers. Neither are signals from other processes.
- **Syscall restart:** `SA_RESTART` and interrupted-syscall restart semantics are not modelled, since no syscall is interrupted.
- **Frame contents:** the frame always carries D-extension FP state, not the kernel's variable-size extension area, which Bridge-V does not need without V.
