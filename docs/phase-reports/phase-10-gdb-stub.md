# Phase 10 mini-report: GDB stub

| Field | Value |
|---|---|
| Item | GDB stub (Phase 10 stretch goal 9, `docs/ROADMAP.md`) |
| Status | Complete (user mode, single-threaded guests) |
| Date | 2026-09-27 |
| Commit | the commit adding this report |

## 1. Summary
`bridgev run --gdb PORT prog` waits for a debugger on `127.0.0.1:PORT` and serves the GDB Remote Serial Protocol. Ubuntu's `gdb-multiarch` 15.1 debugs a guest through it:
```
$ bridgev run --gdb 23456 guest/build/fib-O2.elf 10 &
$ gdb-multiarch -q -batch -ex "set architecture riscv:rv64" -ex "file guest/build/fib-O2.elf" \
    -ex "target remote :23456" -ex "break main" -ex "continue" -ex "info registers pc sp a0" \
    -ex "stepi" -ex "stepi" -ex 'x/2i $pc' -ex "bt" -ex "delete" -ex "continue"
0x000000000001052c in _start ()
Breakpoint 1 at 0x1049c
Breakpoint 1, 0x000000000001049c in main ()
pc             0x1049c	0x1049c <main+8>
sp             0x3fffffc9f0	0x3fffffc9f0
a0             0x2	2
0x0000000000010508 in main ()
0x000000000001050a in main ()
=> 0x1050a <main+118>:	li	a2,10
   0x1050c <main+120>:	li	a1,0
#0  0x000000000001050a in main ()
[Inferior 1 (Remote target) exited normally]
```

## 2. What was built (`src/user/gdb.rs`)
- **Execution:** the debugger's view is the interpreter's. Every step executes a one-instruction block (`build_block_max(pc, 1)` + `exec_block`), whatever `--engine` says. Single steps are therefore exact, and software breakpoints need no code patching: `continue` steps until the pc is a breakpoint. It is slow compared with the JIT, but predictable.
- **Guest behaviour under the debugger:** syscalls, sleeps and signals behave as in a normal run (the same syscall layer and `guest_signal::deliver`). A fault stops with its signal (`S0b` for SIGSEGV); resuming delivers it to the guest's handler, or kills the process (`X0b`). Exit gives `Wxx`, and detach (`D`) runs the rest of the program in the stub.
- **Packets:**
  - execution and breakpoints: `?`, `c`, `s`, `Z0`/`z0`;
  - registers and memory: `g`/`G` (x0–x31, pc), `p`/`P` (0–31 x, 32 pc, 33–64 f0–f31), `m`/`M`;
  - session: `k`, `D`, `H*`;
  - queries: `qSupported` (`PacketSize`, `swbreak`, `qXfer:features:read`), `qAttached`, `qC`, `q[fs]ThreadInfo`, and `qXfer:features:read:target.xml`, which names `riscv:rv64`.

  Anything else gets the empty "unsupported" reply, and gdb falls back (e.g. from `vCont` to `c`/`s`).

## 3. Evidence
- `tests/gdb.rs` drives the stub with a minimal RSP client:
  - `qSupported`, `?`, `g` (pc = ELF entry, sp set), `target.xml`;
  - a breakpoint at `main`, then `c`: the program stops there;
  - `m` at `main` equals the ELF's bytes;
  - `s` moves the pc; `P` then `p` round-trips a register;
  - `z0` then `c`: `W00`, with the program's output intact.

  It passes, and runs in CI with no gdb needed.
- Manual session with `gdb-multiarch` 15.1 (above).

## 4. Limitations
- **Scope:** user mode only (no `bridgev boot --gdb`) and single-threaded guests (clone returns EAGAIN under the debugger).
- **Not supported:** hardware watchpoints (`Z2`–`Z4`), asynchronous interrupt (Ctrl-C while running), and `vCont`.
- **Speed:** everything runs in the interpreter, one instruction at a time.
