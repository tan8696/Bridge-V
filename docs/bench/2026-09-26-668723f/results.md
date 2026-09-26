Host: Intel(R) Xeon(R) Processor @ 2.10GHz (4 vCPU, pinned to CPU 2), kernel 6.18.44-fc-v37, rustc 1.94.1 (e408947bf 2026-03-25), qemu-riscv64 version 8.2.2 (Debian 1:8.2.2+ds-0ubuntu1.18). Commit `668723f`, 2026-09-26T18:43:22+00:00. 5 measured run(s) after 1 warm-up, median (min–max). Shared cloud VM: expect noise of several percent.

### coremark

| config | iterations/s | vs interp | vs native | guest MIPS | iterations/run | valid |
|---|---:|---:|---:|---:|---:|---:|
| interp | 510 (508–520) | 1.0× | 0.020 | 183 | 6,732 | 5/5 |
| jit-naive | 791 (752–817) | 1.6× | 0.030 | 284 | 9,857 | 5/5 |
| jit+chain | 7,024 (6,674–7,251) | 13.8× | 0.271 | 2,519 | 86,775 | 5/5 |
| jit+pinned | 10,168 (9,987–10,242) | 19.9× | 0.392 | 3,646 | 122,604 | 5/5 |
| jit+linear | 13,498 (13,445–13,872) | 26.5× | 0.520 | 4,839 | 174,070 | 5/5 |
| qemu | 8,898 (8,736–8,936) | 17.5× | 0.343 | 3,193 (est.) | 117,476 | 5/5 |
| native | 25,962 (24,956–26,570) | 50.9× | 1.000 | — | 341,472 | 5/5 |
| softmmu | n/a: Phase 7 (--mem=softmmu) |  |  |  |  |  |

| config | translate time (share of run) | host bytes / guest insn | dispatcher entries per M insns |
|---|---:|---:|---:|
| jit-naive | 2.7 ms (0.022%) | 31.3 | 180,762 |
| jit+chain | 2.5 ms (0.020%) | 31.3 | 10 |
| jit+pinned | 5.3 ms (0.044%) | 28.0 | 10 |
| jit+linear | 9.2 ms (0.071%) | 25.0 | 10 |

### dhrystone

| config | Dhrystones/s | vs interp | vs native | DMIPS | guest MIPS | runs/run | valid |
|---|---:|---:|---:|---:|---:|---:|---:|
| interp | 552,303 (549,409–569,102) | 1.0× | 0.012 | 314 | 186 | 3,805,165 | 5/5 |
| jit-naive | 856,901 (809,934–865,922) | 1.6× | 0.019 | 488 | 288 | 5,812,469 | 5/5 |
| jit+chain | 10,796,150 (10,763,270–11,083,702) | 19.5× | 0.241 | 6,145 | 3,625 | 70,384,470 | 5/5 |
| jit+pinned | 16,382,707 (15,676,967–17,150,622) | 29.7× | 0.365 | 9,324 | 5,499 | 105,228,260 | 5/5 |
| jit+linear | 21,861,687 (21,627,933–22,678,936) | 39.6× | 0.487 | 12,443 | 7,337 | 135,786,502 | 5/5 |
| qemu | 4,803,021 (4,612,027–4,855,974) | 8.7× | 0.107 | 2,734 | 1,614 (est.) | 32,575,524 | 5/5 |
| native | 44,867,922 (41,737,267–47,248,605) | 81.2× | 1.000 | 25,537 | — | 296,767,166 | 5/5 |
| softmmu | n/a: Phase 7 (--mem=softmmu) |  |  |  |  |  |  |

| config | translate time (share of run) | host bytes / guest insn | dispatcher entries per M insns |
|---|---:|---:|---:|
| jit-naive | 1.8 ms (0.027%) | 30.9 | 175,606 |
| jit+chain | 1.8 ms (0.028%) | 30.8 | 10 |
| jit+pinned | 3.8 ms (0.059%) | 27.3 | 10 |
| jit+linear | 5.5 ms (0.088%) | 24.4 | 10 |
