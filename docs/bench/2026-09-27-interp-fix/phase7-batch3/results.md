Host: Intel(R) Xeon(R) Processor @ 2.10GHz (4 vCPU, pinned to CPU 2), kernel 6.18.44-fc-v37, rustc 1.94.1 (e408947bf 2026-03-25), qemu-riscv64 version 8.2.2 (Debian 1:8.2.2+ds-0ubuntu1.18). Commit `3869c44` (dirty tree); bridgev binary /tmp/claude-0/-home-user-Bridge-V/14a52129-467f-5a9b-929c-ff913a68b0d0/scratchpad/bv-p7, 2026-09-27T18:36:10+00:00. 5 measured run(s) after 1 warm-up, median (min–max). Shared cloud VM: expect noise of several percent.

### coremark

| config | iterations/s | vs interp | vs native | guest MIPS | iterations/run | valid |
|---|---:|---:|---:|---:|---:|---:|
| interp | 485 (483–496) | 1.0× |  | 174 | 6,474 | 5/5 |

### dhrystone

| config | Dhrystones/s | vs interp | vs native | DMIPS | guest MIPS | runs/run | valid |
|---|---:|---:|---:|---:|---:|---:|---:|
| interp | 507,877 (480,341–516,820) | 1.0× |  | 289 | 171 | 3,382,867 | 5/5 |

### fpbench

| config | units/s | vs interp | vs native | guest MIPS | units/run | valid |
|---|---:|---:|---:|---:|---:|---:|
| interp | 124 (123–137) | 1.0× |  | 87 | 771 | 5/5 |
