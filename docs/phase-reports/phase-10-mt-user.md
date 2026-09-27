# Phase 10 mini-report: multithreaded user mode

| Field | Value |
|---|---|
| Item | Multithreaded user mode (Phase 10 stretch goal 3, `docs/ROADMAP.md`) |
| Status | Done, with serialized execution: correct pthreads, no parallel speedup (D55) |
| Date | 2026-09-27 |
| Commit | the commit adding this report |

## 1. Summary
Guest programs can now create threads. `clone(CLONE_VM | CLONE_THREAD, ...)` starts a host thread with a copy of the caller's registers. Futexes, thread-local storage, `CLONE_CHILD_CLEARTID` (which `pthread_join` relies on) and thread exit all behave as on Linux.

Both test workloads work under every engine and memory backend, byte-identical to `qemu-riscv64`:
- a pthread test program (mutexes, condition variables, C11 atomics, compare-and-swap loops, TLS, a spin-wait that only ends if the spinning thread is preempted, sleeping threads);
- CoreMark's own 4-thread build.

Guest threads take turns under a fair lock rather than running in parallel. That keeps every atomic trivially atomic and needed no change to the memory system or the JIT, but 4 threads get no speedup: MT CoreMark runs at about 88% of the single-thread rate, while QEMU spreads it over 4 host cores (§3).

## 2. What was built (`src/user/thread.rs`, `src/user/syscall.rs`)
- **One host thread per guest thread**, each with its own `CpuState` and its own engine, so JIT threads have separate translation caches. `run()` starts the first guest thread and waits for the process result.
- **The GIL** is a FIFO ticket lock around the shared state (`Process`: guest memory, brk, fds via the host, signal actions; plus the syscall layer). A thread holds it for one engine slice or until its next syscall. The slice is unlimited when the thread is alone, and 10 M instructions (about 4 ms of JIT time) otherwise. While running, the thread's registers are swapped into `Process::cpu`, so the syscall layer is unchanged.
  - The first version used 100 k-instruction slices. Host-thread hand-over latency then dominated: MT CoreMark ran at 6,312 it/s, with the host idle 40% of the time.
- **Syscalls:**
  - `clone`: `CLONE_SETTLS` sets `tp`, and `PARENT_SETTID`/`CHILD_SETTID`/`CHILD_CLEARTID` are honoured. The child starts after the `ecall` with `a0 = 0` on its new stack. `clone3` returns `-ENOSYS`, and glibc falls back to `clone`. Fork-style clones are not supported.
  - `futex`: WAIT/WAIT_BITSET become host futex waits on the guest word's host address, run **without** the GIL. WAKE, WAKE_BITSET, REQUEUE and CMP_REQUEUE are host calls made under it. This works because guest memory *is* host memory and guest threads are host threads, so private futexes match.
  - `nanosleep` and `clock_nanosleep` (new) also run without the GIL.
  - `gettid` and `set_tid_address` report the per-thread id.
  - `exit` ends the thread: it zeroes and wakes the CLEARTID word, and the process ends with its last thread. `exit_group` ends the process at once.
- **Coherence of per-thread caches:**
  - **Code writes:** `DirectMem::smc_epoch` counts code-page writes. A thread whose engine did not see a write (another thread made it) flushes its translations before it runs again.
  - **Mappings:** with `--mem=softmmu`, changes to the mappings flush the other threads' TLBs.
  - **LR/SC:** a thread switch clears the LR reservation, so an SC may fail spuriously (allowed by the ISA).

## 3. Evidence
| Test | Result |
|---|---|
| `guest/c/threads.c` (4 static builds + 1 dynamic) in `tests/user_programs.rs`: 4 workers × 20,000 mutex-protected adds, atomic adds and CAS loops; TLS values per thread; distinct tids; pthread_join return values; 1,000-round condvar ping-pong; a busy-wait on an atomic flag set by a thread after `nanosleep` | byte-identical to qemu-riscv64 under interp, jit, lockstep, no-chain, 64 KiB code cache, mprotect W^X, regalloc none/pinned and `--mem=softmmu` |
| CoreMark `MULTITHREAD=4 USE_PTHREAD` (`tools/build-bench.sh`) | "Correct operation validated" under bridgev jit (§ below) |

MT CoreMark, `tools/bench.py --suite coremark-mt4 --configs jit+linear,qemu,native` (pinned to CPUs 0–3, 1 warm-up run, then 5 measured runs; Xeon @ 2.10 GHz, noisy VM):

| config | iterations/s (4 threads) | vs native | valid runs |
|---|---:|---:|---:|
| bridgev jit (serialized) | 12,189 (12,045–12,347) | 0.123 | 5/5 |
| qemu-riscv64 (threads in parallel) | 38,314 (37,722–38,637) | 0.385 | 5/5 |
| native x86-64 | 99,439 (98,448–100,566) | 1.000 | 5/5 |

- **Raw data:** `docs/bench/2026-09-27-mt-coremark/`. The header there says "pinned to CPU 2", but the MT workload is pinned to CPUs 0–3 (`CoreMarkMT.threads`).
- **Single-thread comparison:** single-thread CoreMark under the JIT ran at 13,923 it/s on this host (Phase 8 report), so running 4 threads one at a time keeps about 88% of that throughput.
- **QEMU:** qemu-user runs guest threads in parallel on the 4 cores and scales about 4.1× over its own single-thread rate (9,242 it/s in the Phase 7 batch).
- **Column caveat:** the harness's derived columns for bridgev (guest MIPS, dispatcher entries) come from the last thread's `--stats` plus the summed instruction count, so they are approximate for this workload.

## 4. Limitations
- **No parallel execution** (the GIL). Removing it needs:
  - a `DirectMem` shared without `&mut` (structural changes under a lock, raw accesses outside it);
  - host-atomic AMOs and LR/SC in the helpers;
  - a shared or per-thread translation cache with cross-thread invalidation;
  - `exit_group` interrupting threads blocked in host futexes (today the process exit kills them).
- **Memory:** each thread has its own translation cache (a lazily backed 256 MiB code buffer each).
- **Unsupported:** `fork`/`vfork` (clone without `CLONE_VM`), robust futex lists (`set_robust_list` accepted, not acted on), and PI futexes.
