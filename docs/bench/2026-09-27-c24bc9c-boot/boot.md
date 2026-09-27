Host: Intel(R) Xeon(R) Processor @ 2.10GHz; commit c24bc9c; QEMU emulator version 8.2.2 (Debian 1:8.2.2+ds-0ubuntu1.18); 5 runs after 1 warm-up, median (min–max).
Command: python3 tools/boot-bench.py --configs jit,interp,qemu,qemu-bvdtb --runs 5

| config | time to shell (s) | instructions (whole run) | MIPS |
|---|---:|---:|---:|
| jit | 1.25 (1.21–1.42) | 930,751,294 | 701 |
| interp | 9.99 (9.82–10.21) | 895,355,098 | 89 |
| qemu | 1.50 (1.36–1.56) | — | — |
| qemu-bvdtb | 1.62 (1.25–1.72) | — | — |
