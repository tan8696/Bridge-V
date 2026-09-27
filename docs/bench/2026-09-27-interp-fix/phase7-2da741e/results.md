Host: Intel(R) Xeon(R) Processor @ 2.10GHz (4 vCPU, pinned to CPU 2), kernel 6.18.44-fc-v37, rustc 1.94.1 (e408947bf 2026-03-25), qemu-riscv64 version 8.2.2 (Debian 1:8.2.2+ds-0ubuntu1.18). Commit `47fd507` (dirty tree); bridgev binary /tmp/claude-0/-home-user-Bridge-V/14a52129-467f-5a9b-929c-ff913a68b0d0/scratchpad/bv-p7, 2026-09-27T12:20:56+00:00. 5 measured run(s) after 1 warm-up, median (min–max). Shared cloud VM: expect noise of several percent.

### coremark

| config | iterations/s | vs interp | vs native | guest MIPS | iterations/run | valid |
|---|---:|---:|---:|---:|---:|---:|
| interp | 473 (458–492) | 1.0× |  | 170 | 5,970 | 5/5 |

### dhrystone

| config | Dhrystones/s | vs interp | vs native | DMIPS | guest MIPS | runs/run | valid |
|---|---:|---:|---:|---:|---:|---:|---:|
| interp | 496,761 (479,890–508,115) | 1.0× |  | 283 | 167 | 3,150,609 | 5/5 |

### fpbench

| config | units/s | vs interp | vs native | guest MIPS | units/run | valid |
|---|---:|---:|---:|---:|---:|---:|
| interp | 123 (121–132) | 1.0× |  | 86 | 804 | 5/5 |
