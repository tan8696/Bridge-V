Host: Intel(R) Xeon(R) Processor @ 2.10GHz (4 vCPU, pinned to CPU 2), kernel 6.18.44-fc-v37, rustc 1.94.1 (e408947bf 2026-03-25), qemu-riscv64 version 8.2.2 (Debian 1:8.2.2+ds-0ubuntu1.18). Commit `6b5a7c2` (dirty tree), 2026-09-27T05:56:36+00:00. 5 measured run(s) after 1 warm-up, median (min–max). Shared cloud VM: expect noise of several percent.

### coremark-mt4

| config | iterations/s | vs interp | vs native | guest MIPS | iterations per thread/run | valid |
|---|---:|---:|---:|---:|---:|---:|
| jit+linear | 12,189 (12,045–12,347) |  | 0.123 | 4,372 | 160,514 | 5/5 |
| qemu | 38,314 (37,722–38,637) |  | 0.385 | 54,989 (est.) | 481,229 | 5/5 |
| native | 99,439 (98,448–100,566) |  | 1.000 | — | 1,306,156 | 5/5 |

| config | translate time (share of run) | host bytes / guest insn | dispatcher entries per M insns |
|---|---:|---:|---:|
| jit+linear | 10.7 ms (0.020%) | 26.2 | 6,875 |
