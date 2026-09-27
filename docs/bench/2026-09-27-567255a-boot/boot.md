Host: Intel(R) Xeon(R) Processor @ 2.10GHz; commit af6fcae; QEMU emulator version 8.2.2 (Debian 1:8.2.2+ds-0ubuntu1.18); 5 runs after 1 warm-up, median (min–max).

| config | time to shell (s) | instructions (whole run) | MIPS |
|---|---:|---:|---:|
| jit | 1.48 (1.34–1.50) | 926,085,442 | 592 |
| interp | 10.49 (9.74–10.82) | 895,416,385 | 85 |
| qemu | 1.54 (1.40–1.93) | — | — |
