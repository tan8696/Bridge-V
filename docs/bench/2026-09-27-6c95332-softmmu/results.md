Host: Intel(R) Xeon(R) Processor @ 2.10GHz (4 vCPU, pinned to CPU 2), kernel 6.18.44-fc-v37, rustc 1.94.1 (e408947bf 2026-03-25), qemu-riscv64 version 8.2.2 (Debian 1:8.2.2+ds-0ubuntu1.18). Commit `6c95332` (dirty tree), 2026-09-27T00:34:14+00:00. 5 measured run(s) after 1 warm-up, median (min–max). Shared cloud VM: expect noise of several percent.

### coremark

| config | iterations/s | vs interp | vs native | guest MIPS | iterations/run | valid |
|---|---:|---:|---:|---:|---:|---:|
| jit+linear | 13,789 (13,512–14,074) |  |  | 4,944 | 177,387 | 5/5 |
| softmmu | 7,498 (7,440–7,574) |  |  | 2,688 | 95,934 | 5/5 |

| config | translate time (share of run) | host bytes / guest insn | dispatcher entries per M insns |
|---|---:|---:|---:|
| jit+linear | 8.5 ms (0.066%) | 24.9 | 10 |

### dhrystone

| config | Dhrystones/s | vs interp | vs native | DMIPS | guest MIPS | runs/run | valid |
|---|---:|---:|---:|---:|---:|---:|---:|
| jit+linear | 21,803,943 (21,598,316–22,869,068) |  |  | 12,410 | 7,317 | 140,653,162 | 5/5 |
| softmmu | 10,856,731 (10,704,928–11,454,179) |  |  | 6,179 | 3,642 | 73,537,399 | 5/5 |

| config | translate time (share of run) | host bytes / guest insn | dispatcher entries per M insns |
|---|---:|---:|---:|
| jit+linear | 5.8 ms (0.090%) | 24.3 | 10 |
