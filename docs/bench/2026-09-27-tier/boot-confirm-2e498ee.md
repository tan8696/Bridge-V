# Boot confirmation at `2e498ee` (default --tier 32)

GitHub Actions run 36351084311 (workflow `bench`, job "Linux boot, time to shell"), command:
`python3 tools/boot-bench.py --configs jit-tier0,jit-tier16,jit-tier32,jit-tier64,qemu --runs 7 --detail`, then `--configs jit-tier1000000000 --runs 1 --detail`.

Host: AMD EPYC 7763 64-Core Processor; commit 2e498ee; QEMU emulator version 8.2.2 (Debian 1:8.2.2+ds-0ubuntu1.18); 7 runs after 1 warm-up, median (min–max).

| config | time to shell (s) | instructions (whole run) | MIPS |
|---|---:|---:|---:|
| jit-tier0 | 1.30 (1.28–1.50) | 916,293,389 | 672 |
| jit-tier16 | 1.07 (1.05–1.08) | 921,278,943 | 819 |
| jit-tier32 | 1.06 (1.05–1.09) | 918,694,296 | 828 |
| jit-tier64 | 1.09 (1.07–1.10) | 923,660,861 | 814 |
| qemu | 1.41 (1.37–1.43) | — | — |

jit-tier0: bridgev: 1130 SBI calls; 5 WFI idles, 0.020 s idle; jit: 65634 TBs translated (411011 guest insns, 44501 KiB host code, 110.9 bytes/insn), 0 full + 800 code-change flushes, translate time 560.7 ms; 668503 dispatcher entries (732 per M guest insns); exits: none 279949, ecall 2422, exception 6, flush 400, host-fault 0, budget 16715, jump-cache miss 368492, fp-variant 0, mmu-fault 397, smc 117, wfi 5, straddle 211145; SMC: 117 code-page writes, 10611 TBs invalidated; chain: on, 46047 links, 5662 unlinks, 429060 jump-cache fills; JALR count needs --profile-jit; regalloc Linear (emitted code, all TBs): 89403 fills, 1189 spills, 136386 write-backs, 57902 moves, 0 retranslations

jit-tier16: bridgev: 1044 SBI calls; 3 WFI idles, 0.019 s idle; jit: 23163 TBs translated (146462 guest insns, 16219 KiB host code, 113.4 bytes/insn), 0 full + 800 code-change flushes, translate time 196.8 ms; 633550 dispatcher entries (691 per M guest insns); exits: none 280203, ecall 1397, exception 0, flush 384, host-fault 0, budget 16775, jump-cache miss 334546, fp-variant 0, mmu-fault 144, smc 101, wfi 0, straddle 210455; SMC: 116 code-page writes, 705 TBs invalidated; chain: on, 17830 links, 553 unlinks, 311567 jump-cache fills; JALR count needs --profile-jit; regalloc Linear (emitted code, all TBs): 36616 fills, 724 spills, 50314 write-backs, 21160 moves, 0 retranslations; tier 16: 525174 blocks (3266135 guest insns) interpreted, 23086 blocks then translated, 42162 still cold (by runs: 1: 20579, 2-3: 7969, 4-7: 5161, 8-15: 7931, 16: 522)

jit-tier32: bridgev: 1050 SBI calls; 5 WFI idles, 0.020 s idle; jit: 18023 TBs translated (115176 guest insns, 12631 KiB host code, 112.3 bytes/insn), 0 full + 800 code-change flushes, translate time 154.2 ms; 643518 dispatcher entries (702 per M guest insns); exits: none 292841, ecall 1299, exception 0, flush 368, host-fault 0, budget 16804, jump-cache miss 331991, fp-variant 0, mmu-fault 120, smc 95, wfi 0, straddle 210510; SMC: 116 code-page writes, 512 TBs invalidated; chain: on, 14008 links, 399 unlinks, 272262 jump-cache fills; JALR count needs --profile-jit; regalloc Linear (emitted code, all TBs): 29149 fills, 663 spills, 39737 write-backs, 16407 moves, 0 retranslations; tier 32: 850398 blocks (5330215 guest insns) interpreted, 17960 blocks then translated, 47217 still cold (by runs: 1: 20587, 2-3: 7766, 4-7: 5322, 8-15: 7925, 16-31: 5444, 32: 173)

jit-tier64: bridgev: 1056 SBI calls; 5 WFI idles, 0.019 s idle; jit: 13391 TBs translated (86546 guest insns, 9310 KiB host code, 110.2 bytes/insn), 0 full + 800 code-change flushes, translate time 118.9 ms; 672467 dispatcher entries (736 per M guest insns); exits: none 312595, ecall 1207, exception 0, flush 336, host-fault 0, budget 16792, jump-cache miss 341346, fp-variant 0, mmu-fault 103, smc 88, wfi 0, straddle 210522; SMC: 116 code-page writes, 394 TBs invalidated; chain: on, 10520 links, 308 unlinks, 232218 jump-cache fills; JALR count needs --profile-jit; regalloc Linear (emitted code, all TBs): 22290 fills, 596 spills, 30021 write-backs, 12238 moves, 0 retranslations; tier 64: 1342674 blocks (8491824 guest insns) interpreted, 13368 blocks then translated, 51767 still cold (by runs: 1: 20522, 2-3: 7861, 4-7: 5221, 8-15: 7938, 16-31: 5434, 32-63: 4739, 64: 52)

Host: AMD EPYC 7763 64-Core Processor; commit 2e498ee; QEMU emulator version 8.2.2 (Debian 1:8.2.2+ds-0ubuntu1.18); 1 runs after 1 warm-up, median (min–max).

| config | time to shell (s) | instructions (whole run) | MIPS |
|---|---:|---:|---:|
| jit-tier1000000000 | 13.52 (13.52–13.52) | 897,476,061 | 66 |

jit-tier1000000000: bridgev: 4652 SBI calls; 2 WFI idles, 0.018 s idle; jit: 0 TBs translated (0 guest insns, 0 KiB host code, 0.0 bytes/insn), 0 full + 802 code-change flushes, translate time 0.0 ms; 0 dispatcher entries (0 per M guest insns); exits: none 0, ecall 0, exception 0, flush 0, host-fault 0, budget 0, jump-cache miss 0, fp-variant 0, mmu-fault 0, smc 0, wfi 0, straddle 220538; SMC: 118 code-page writes, 0 TBs invalidated; chain: on, 0 links, 0 unlinks, 0 jump-cache fills; JALR count needs --profile-jit; regalloc Linear (emitted code, all TBs): 0 fills, 0 spills, 0 write-backs, 0 moves, 0 retranslations; tier 1000000000: 82569389 blocks (897250871 guest insns) interpreted, 0 blocks then translated, 65666 still cold (by runs: 1: 20566, 2-3: 8012, 4-7: 5344, 8-15: 7934, 16-31: 5375, 32-63: 4558, 64-127: 3093, 128-255: 2549, 256-511: 2508, 512-1023: 1926, 1024-2047: 1826, 2048-4095: 679, 4096-8191: 454, 8192-16383: 168, 16384-32767: 337, 32768-65535: 115, 65536-131071: 105, 131072-262143: 76, 262144-524287: 19, 524288-1048575: 12, 1048576-2097151: 8, 2097152-4194303: 1, 4194304-8388607: 1)
