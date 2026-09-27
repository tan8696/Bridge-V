Host: Intel(R) Xeon(R) Processor @ 2.10GHz (4 vCPU, pinned to CPU 2), kernel 6.18.44-fc-v37, rustc 1.94.1 (e408947bf 2026-03-25), qemu-riscv64 version 8.2.2 (Debian 1:8.2.2+ds-0ubuntu1.18). Commit `47fd507` (dirty tree); bridgev binary /tmp/claude-0/-home-user-Bridge-V/14a52129-467f-5a9b-929c-ff913a68b0d0/scratchpad/bv-HEAD, 2026-09-27T12:01:37+00:00. 5 measured run(s) after 1 warm-up, median (min–max). Shared cloud VM: expect noise of several percent.

### coremark

| config | iterations/s | vs interp | vs native | guest MIPS | iterations/run | valid |
|---|---:|---:|---:|---:|---:|---:|
| interp | 453 (420–455) | 1.0× |  | 162 | 5,691 | 5/5 |

### dhrystone

| config | Dhrystones/s | vs interp | vs native | DMIPS | guest MIPS | runs/run | valid |
|---|---:|---:|---:|---:|---:|---:|---:|
| interp | 429,099 (425,166–431,447) | 1.0× |  | 244 | 144 | 2,749,903 | 5/5 |

### fpbench

| config | units/s | vs interp | vs native | guest MIPS | units/run | valid |
|---|---:|---:|---:|---:|---:|---:|
| interp | 117 (108–124) | 1.0× |  | 82 | 671 | 5/5 |
