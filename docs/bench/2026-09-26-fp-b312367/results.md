Host: Intel(R) Xeon(R) Processor @ 2.10GHz (4 vCPU, pinned to CPU 2), kernel 6.18.44-fc-v37, rustc 1.94.1 (e408947bf 2026-03-25), qemu-riscv64 version 8.2.2 (Debian 1:8.2.2+ds-0ubuntu1.18). Commit `b312367` (dirty tree), 2026-09-26T19:53:01+00:00. 5 measured run(s) after 1 warm-up, median (min–max). Shared cloud VM: expect noise of several percent.

### fpbench

| config | units/s | vs interp | vs native | guest MIPS | units/run | valid |
|---|---:|---:|---:|---:|---:|---:|
| interp | 130 (124–134) | 1.0× | 0.008 | 91 | 863 | 5/5 |
| jit-naive | 55 (54–58) | 0.4× | 0.003 | 38 | 356 | 5/5 |
| jit+chain | 58 (56–61) | 0.4× | 0.003 | 41 | 368 | 5/5 |
| jit+pinned | 3,686 (3,629–3,919) | 28.4× | 0.215 | 2,584 | 23,804 | 5/5 |
| jit+linear | 3,508 (3,469–3,654) | 27.1× | 0.205 | 2,458 | 22,531 | 5/5 |
| jit-helper-fp | 58 (55–59) | 0.4× | 0.003 | 40 | 376 | 5/5 |
| qemu | 629 (606–687) | 4.9× | 0.037 | 442 (est.) | 4,175 | 5/5 |
| native | 17,107 (16,920–17,700) | 132.0× | 1.000 | — | 112,757 | 5/5 |
| softmmu | n/a: Phase 7 (--mem=softmmu) |  |  |  |  |  |

| config | translate time (share of run) | host bytes / guest insn | dispatcher entries per M insns |
|---|---:|---:|---:|
| jit-naive | 2.2 ms (0.034%) | 32.2 | 68,668 |
| jit+chain | 2.3 ms (0.036%) | 32.2 | 16 |
| jit+pinned | 5.0 ms (0.077%) | 30.0 | 10 |
| jit+linear | 8.4 ms (0.131%) | 27.2 | 10 |
| jit-helper-fp | 7.0 ms (0.107%) | 28.6 | 16 |
