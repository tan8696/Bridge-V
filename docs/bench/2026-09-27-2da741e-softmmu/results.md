Host: Intel(R) Xeon(R) Processor @ 2.10GHz (4 vCPU, pinned to CPU 2), kernel 6.18.44-fc-v37, rustc 1.94.1 (e408947bf 2026-03-25), qemu-riscv64 version 8.2.2 (Debian 1:8.2.2+ds-0ubuntu1.18). Commit `2da741e`, 2026-09-27T00:23:21+00:00. 5 measured run(s) after 1 warm-up, median (min–max). Shared cloud VM: expect noise of several percent.

### coremark

| config | iterations/s | vs interp | vs native | guest MIPS | iterations/run | valid |
|---|---:|---:|---:|---:|---:|---:|
| interp | 520 (486–529) | 1.0× |  | 186 | 6,469 | 5/5 |
| jit+linear | 14,144 (13,651–14,431) | 27.2× |  | 5,071 | 179,461 | 5/5 |
| softmmu | 6,318 (6,168–6,787) | 12.2× |  | 2,264 | 82,579 | 5/5 |
| qemu | 9,242 (9,059–10,032) | 17.8× |  | 3,316 (est.) | 121,094 | 5/5 |

| config | translate time (share of run) | host bytes / guest insn | dispatcher entries per M insns |
|---|---:|---:|---:|
| jit+linear | 8.6 ms (0.068%) | 24.9 | 10 |

### dhrystone

| config | Dhrystones/s | vs interp | vs native | DMIPS | guest MIPS | runs/run | valid |
|---|---:|---:|---:|---:|---:|---:|---:|
| interp | 517,037 (508,203–579,977) | 1.0× |  | 294 | 174 | 3,425,052 | 5/5 |
| jit+linear | 21,478,937 (21,242,989–21,987,036) | 41.5× |  | 12,225 | 7,208 | 142,310,695 | 5/5 |
| softmmu | 7,705,864 (7,483,189–8,010,739) | 14.9× |  | 4,386 | 2,585 | 48,926,267 | 5/5 |
| qemu | 5,142,221 (4,723,464–5,649,993) | 9.9× |  | 2,927 | 1,728 (est.) | 32,951,997 | 5/5 |

| config | translate time (share of run) | host bytes / guest insn | dispatcher entries per M insns |
|---|---:|---:|---:|
| jit+linear | 5.7 ms (0.086%) | 24.3 | 10 |
