Host: Intel(R) Xeon(R) Processor @ 2.10GHz; commit ee9f7a6; QEMU emulator version 8.2.2 (Debian 1:8.2.2+ds-0ubuntu1.18); 3 runs after 1 warm-up, median (min–max).
Command: python3 tools/boot-bench.py --configs jit,qemu,qemu-bvdtb --runs 3 (the first Phase 9 commit, before D51).

| config | time to shell (s) | instructions (whole run) | MIPS |
|---|---:|---:|---:|
| jit | 1.80 (1.76–1.82) | 935,057,964 | 490 |
| qemu | 1.48 (1.40–1.59) | — | — |
| qemu-bvdtb | 1.73 (1.50–1.93) | — | — |

`--stats` of one jit boot at ee9f7a6 (same session):
bridgev: 933049909 guest instructions in 1.984 s (470.2 MIPS)
bridgev: 1319 SBI calls; jit: 65598 TBs translated (410741 guest insns, 43296 KiB host code, 107.9 bytes/insn), 0 full + 800 code-change flushes, translate time 597.2 ms; 6496688 dispatcher entries (6965 per M guest insns); exits: none 6062423, ecall 2175, exception 6, flush 400, host-fault 0, budget 14989, jump-cache miss 416183, fp-variant 0, mmu-fault 395, smc 117, straddle 221941; SMC: 117 code-page writes, 10612 TBs invalidated; chain: on, 45627 links, 5650 unlinks, 801415 jump-cache fills
softmmu: 1438070 TLB fills
