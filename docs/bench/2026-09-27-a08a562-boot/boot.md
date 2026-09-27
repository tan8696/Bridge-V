Host: Intel(R) Xeon(R) Processor @ 2.10GHz; commit a08a562; QEMU emulator version 8.2.2 (Debian 1:8.2.2+ds-0ubuntu1.18); 5 runs after 1 warm-up, median (min–max).
Command: python3 tools/boot-bench.py --configs jit,interp,qemu,qemu-bvdtb --runs 5 (after D51, before WFI idle).

| config | time to shell (s) | instructions (whole run) | MIPS |
|---|---:|---:|---:|
| jit | 1.30 (1.09–1.48) | 952,868,894 | 690 |
| interp | 10.18 (9.77–10.44) | 896,778,049 | 87 |
| qemu | 1.48 (1.28–1.68) | — | — |
| qemu-bvdtb | 1.49 (1.27–1.79) | — | — |
