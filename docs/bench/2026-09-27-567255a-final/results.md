Host: Intel(R) Xeon(R) Processor @ 2.10GHz (4 vCPU, pinned to CPU 2), kernel 6.18.44-fc-v37, rustc 1.94.1 (e408947bf 2026-03-25), qemu-riscv64 version 8.2.2 (Debian 1:8.2.2+ds-0ubuntu1.18). Commit `567255a`, 2026-09-27T11:03:54+00:00. 5 measured run(s) after 1 warm-up, median (min–max). Shared cloud VM: expect noise of several percent.

### coremark

| config | iterations/s | vs interp | vs native | guest MIPS | iterations/run | valid |
|---|---:|---:|---:|---:|---:|---:|
| interp | 454 (436–470) | 1.0× | 0.018 | 163 | 5,833 | 5/5 |
| jit+linear | 13,409 (12,854–13,746) | 29.5× | 0.525 | 4,807 | 176,764 | 5/5 |
| softmmu | 7,526 (7,303–7,620) | 16.6× | 0.295 | 2,697 | 98,205 | 5/5 |
| qemu | 8,988 (8,652–9,336) | 19.8× | 0.352 | 3,225 (est.) | 117,611 | 5/5 |
| native | 25,531 (25,354–25,617) | 56.2× | 1.000 | — | 336,755 | 5/5 |

| config | translate time (share of run) | host bytes / guest insn | dispatcher entries per M insns |
|---|---:|---:|---:|
| jit+linear | 8.6 ms (0.065%) | 24.9 | 10 |

### dhrystone

| config | Dhrystones/s | vs interp | vs native | DMIPS | guest MIPS | runs/run | valid |
|---|---:|---:|---:|---:|---:|---:|---:|
| interp | 472,238 (430,835–482,445) | 1.0× | 0.010 | 269 | 159 | 2,987,420 | 5/5 |
| jit+linear | 20,728,869 (20,184,104–22,565,587) | 43.9× | 0.455 | 11,798 | 6,955 | 133,687,860 | 5/5 |
| softmmu | 10,115,192 (10,095,604–10,370,023) | 21.4× | 0.222 | 5,757 | 3,393 | 67,049,138 | 5/5 |
| qemu | 4,758,387 (4,604,799–4,899,881) | 10.1× | 0.104 | 2,708 | 1,599 (est.) | 30,978,188 | 5/5 |
| native | 45,562,668 (44,432,669–47,145,198) | 96.5× | 1.000 | 25,932 | — | 304,819,353 | 5/5 |

| config | translate time (share of run) | host bytes / guest insn | dispatcher entries per M insns |
|---|---:|---:|---:|
| jit+linear | 6.4 ms (0.099%) | 24.3 | 10 |

### fpbench

| config | units/s | vs interp | vs native | guest MIPS | units/run | valid |
|---|---:|---:|---:|---:|---:|---:|
| interp | 118 (115–122) | 1.0× | 0.007 | 83 | 800 | 5/5 |
| jit+linear | 3,711 (3,659–3,873) | 31.5× | 0.224 | 2,600 | 24,001 | 5/5 |
| softmmu | 2,654 (2,527–2,752) | 22.5× | 0.160 | 1,859 | 17,438 | 5/5 |
| qemu | 636 (611–644) | 5.4× | 0.038 | 447 (est.) | 4,098 | 5/5 |
| native | 16,589 (16,278–17,379) | 140.9× | 1.000 | — | 115,373 | 5/5 |

| config | translate time (share of run) | host bytes / guest insn | dispatcher entries per M insns |
|---|---:|---:|---:|
| jit+linear | 7.5 ms (0.116%) | 27.2 | 10 |
