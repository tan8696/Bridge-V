Host: Intel(R) Xeon(R) Processor @ 2.10GHz (4 vCPU, pinned to CPU 2), kernel 6.18.44-fc-v37, rustc 1.94.1 (e408947bf 2026-03-25), qemu-riscv64 version 8.2.2 (Debian 1:8.2.2+ds-0ubuntu1.18). Commit `f7f41aa`, 2026-09-27T12:05:25+00:00. 5 measured run(s) after 1 warm-up, median (min–max). Shared cloud VM: expect noise of several percent.

### coremark

| config | iterations/s | vs interp | vs native | guest MIPS | iterations/run | valid |
|---|---:|---:|---:|---:|---:|---:|
| interp | 487 (452–507) | 1.0× |  | 175 | 6,228 | 5/5 |

### dhrystone

| config | Dhrystones/s | vs interp | vs native | DMIPS | guest MIPS | runs/run | valid |
|---|---:|---:|---:|---:|---:|---:|---:|
| interp | 501,304 (486,392–520,319) | 1.0× |  | 285 | 168 | 3,467,237 | 5/5 |
