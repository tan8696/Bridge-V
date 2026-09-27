# User mode at `2e498ee`: default tier (jit+linear = --tier 32) against --tier 0

GitHub Actions run 36351084311 (workflow `bench`, job "CoreMark, Dhrystone, fpbench"), command:
`python3 tools/bench.py --configs jit+linear,jit-tier0 --runs 3 --out bench-out`. Per-run lines, then the harness tables:

```
coremark  jit+linear warm-up   n=149713 score=11771.74084 wall=12.72s ok
coremark  jit+linear run 1/3   n=153033 score=11794.450867 wall=12.98s ok
coremark  jit+linear run 2/3   n=153033 score=11847.410389 wall=12.92s ok
coremark  jit+linear run 3/3   n=153033 score=11858.426966 wall=12.91s ok
coremark  jit-tier0  warm-up   n=153167 score=11830.30818 wall=12.96s ok
coremark  jit-tier0  run 1/3   n=153795 score=11831.294715 wall=13.01s ok
coremark  jit-tier0  run 2/3   n=153795 score=11833.115334 wall=13.01s ok
coremark  jit-tier0  run 3/3   n=153795 score=11843.138765 wall=13.00s ok
dhrystone jit+linear warm-up   n=108547712 score=16683182.0 wall=6.51s ok
dhrystone jit+linear run 1/3   n=108440683 score=16710005.0 wall=6.50s ok
dhrystone jit+linear run 2/3   n=108440683 score=16723168.0 wall=6.49s ok
dhrystone jit+linear run 3/3   n=108440683 score=16675336.0 wall=6.51s ok
dhrystone jit-tier0  warm-up   n=106989896 score=16561885.0 wall=6.47s ok
dhrystone jit-tier0  run 1/3   n=107652253 score=16543770.0 wall=6.52s ok
dhrystone jit-tier0  run 2/3   n=107652253 score=16524448.0 wall=6.53s ok
dhrystone jit-tier0  run 3/3   n=107652253 score=16242056.0 wall=6.64s ok
fpbench   jit+linear warm-up   n=13660 score=2102.69 wall=6.50s ok
fpbench   jit+linear run 1/3   n=13668 score=2071.567 wall=6.61s ok
fpbench   jit+linear run 2/3   n=13668 score=2078.75 wall=6.58s ok
fpbench   jit+linear run 3/3   n=13668 score=2100.836 wall=6.51s ok
fpbench   jit-tier0  warm-up   n=13604 score=2101.634 wall=6.49s ok
fpbench   jit-tier0  run 1/3   n=13661 score=2104.727 wall=6.50s ok
fpbench   jit-tier0  run 2/3   n=13661 score=2104.649 wall=6.51s ok
fpbench   jit-tier0  run 3/3   n=13661 score=2105.115 wall=6.50s ok
```

Host: AMD EPYC 7763 64-Core Processor (4 vCPU, pinned to CPU 2), kernel 6.17.0-1022-azure, rustc 1.94.1 (e408947bf 2026-03-25), qemu-riscv64 version 8.2.2 (Debian 1:8.2.2+ds-0ubuntu1.18). Commit `2e498ee`, 2026-09-27T21:17:06+00:00. 3 measured run(s) after 1 warm-up, median (min–max). Shared cloud VM: expect noise of several percent.

### coremark

| config | iterations/s | vs interp | vs native | guest MIPS | iterations/run | valid |
|---|---:|---:|---:|---:|---:|---:|
| jit+linear | 11,847 (11,794–11,858) |  |  | 4,250 | 153,033 | 3/3 |
| jit-tier0 | 11,833 (11,831–11,843) |  |  | 4,242 | 153,795 | 3/3 |

| config | translate time (share of run) | host bytes / guest insn | dispatcher entries per M insns |
|---|---:|---:|---:|
| jit+linear | 2.1 ms (0.016%) | 24.4 | 10 |
| jit-tier0 | 10.3 ms (0.079%) | 25.0 | 10 |

### dhrystone

| config | Dhrystones/s | vs interp | vs native | DMIPS | guest MIPS | runs/run | valid |
|---|---:|---:|---:|---:|---:|---:|---:|
| jit+linear | 16,710,005 (16,675,336–16,723,168) |  |  | 9,511 | 5,610 | 108,440,683 | 3/3 |
| jit-tier0 | 16,524,448 (16,242,056–16,543,770) |  |  | 9,405 | 5,542 | 107,652,253 | 3/3 |

| config | translate time (share of run) | host bytes / guest insn | dispatcher entries per M insns |
|---|---:|---:|---:|
| jit+linear | 1.2 ms (0.018%) | 24.9 | 10 |
| jit-tier0 | 8.5 ms (0.130%) | 24.4 | 10 |

### fpbench

| config | units/s | vs interp | vs native | guest MIPS | units/run | valid |
|---|---:|---:|---:|---:|---:|---:|
| jit+linear | 2,079 (2,072–2,101) |  |  | 1,458 | 13,668 | 3/3 |
| jit-tier0 | 2,105 (2,105–2,105) |  |  | 1,474 | 13,661 | 3/3 |

| config | translate time (share of run) | host bytes / guest insn | dispatcher entries per M insns |
|---|---:|---:|---:|
| jit+linear | 1.4 ms (0.021%) | 40.6 | 10 |
| jit-tier0 | 9.0 ms (0.138%) | 27.1 | 10 |
