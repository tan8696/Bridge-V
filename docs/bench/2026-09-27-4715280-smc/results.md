Host: Intel(R) Xeon(R) Processor @ 2.10GHz (4 vCPU, pinned to CPU 2), kernel 6.18.44-fc-v37, rustc 1.94.1 (e408947bf 2026-03-25), qemu-riscv64 version 8.2.2 (Debian 1:8.2.2+ds-0ubuntu1.18). Commit `4715280` (dirty tree), 2026-09-27T00:59:54+00:00. 5 measured run(s) after 1 warm-up, median (min–max). Shared cloud VM: expect noise of several percent.

### coremark

| config | iterations/s | vs interp | vs native | guest MIPS | iterations/run | valid |
|---|---:|---:|---:|---:|---:|---:|
| jit+linear | 13,923 (13,672–14,242) |  |  | 4,992 | 185,385 | 5/5 |
| softmmu | 7,750 (7,712–7,948) |  |  | 2,778 | 104,065 | 5/5 |

| config | translate time (share of run) | host bytes / guest insn | dispatcher entries per M insns |
|---|---:|---:|---:|
| jit+linear | 8.7 ms (0.065%) | 24.9 | 10 |

### dhrystone

| config | Dhrystones/s | vs interp | vs native | DMIPS | guest MIPS | runs/run | valid |
|---|---:|---:|---:|---:|---:|---:|---:|
| jit+linear | 21,777,166 (21,562,873–21,963,635) |  |  | 12,395 | 7,308 | 145,885,344 | 5/5 |
| softmmu | 10,343,653 (10,280,872–10,706,242) |  |  | 5,887 | 3,470 | 68,425,253 | 5/5 |

| config | translate time (share of run) | host bytes / guest insn | dispatcher entries per M insns |
|---|---:|---:|---:|
| jit+linear | 6.5 ms (0.097%) | 24.3 | 10 |
