Host: Intel(R) Xeon(R) Processor @ 2.10GHz; commit 57af1e0; QEMU emulator version 8.2.2 (Debian 1:8.2.2+ds-0ubuntu1.18); 5 runs after 1 warm-up, median (min–max).
Command: python3 tools/boot-bench.py --configs jit,jit-opensbi,jit-sv48,qemu --runs 5

| config | time to shell (s) | instructions (whole run) | MIPS |
|---|---:|---:|---:|
| jit | 1.37 (1.19–1.41) | 936,085,910 | 641 |
| jit-opensbi | 1.52 (1.43–1.55) | 952,263,410 | 585 |
| jit-sv48 | 1.38 (1.15–1.44) | 931,135,619 | 632 |
| qemu | 1.45 (1.24–1.49) | — | — |
