# 18 · Performance and benchmarks

## What you will learn

- the benchmarks used (CoreMark, Dhrystone, the FP benchmark, the Linux boot) and what they measure
- how measurements were taken so they can be trusted
- the final results against the interpreter, QEMU and native code
- how much each JIT technique contributed, and **why** each one is faster
- the Phase 12 result: the interpreter tier that made the Linux boot 18% faster
- known issues and honest caveats

Sources: [`docs/BENCHMARKS.md`](../BENCHMARKS.md) (every number here comes from it or a phase report), [`tools/bench.py`](../../tools/bench.py), [`tools/boot-bench.py`](../../tools/boot-bench.py).

---

## 1. Units

- **MIPS**: millions of *guest* (RISC-V) instructions completed per second of real time. "4,807 MIPS" = 4.8 billion guest instructions per second.
- **CoreMark iterations/s**: how many times per second the CoreMark workload completes. Higher is better.
- **DMIPS**: Dhrystone MIPS = Dhrystones per second ÷ 1,757 (a historical normalization).
- **Speedup**: time(A) ÷ time(B) on the same workload and machine. "1.49× QEMU" means Bridge-V does 1.49 times as much work per second as QEMU.
- **% of native**: the same C source compiled directly for x86-64 and run on the same machine is 100%.

---

## 2. The workloads

| Benchmark | What it is | What it stresses |
|---|---|---|
| **CoreMark** (EEMBC) | the industry-standard CPU benchmark: linked lists, matrix math, a state machine, CRC | integer code, branches, memory, function calls |
| **Dhrystone** | a classic synthetic integer benchmark (the riscv-tests version, unmodified, plus a small Linux shim) | calls, string ops, very short blocks |
| **fpbench** ([`guest/bench/fp/fpbench.c`](../../guest/bench/fp/fpbench.c)) | an n-body simulation (double), a 24×24 single-precision matrix multiply, 4,096 int↔FP conversions | floating point |
| **Linux boot** | time from start to the BusyBox `/ #` prompt | everything in system mode: MMU, TLB, traps, interrupts, devices |

Every run is **validated**: CoreMark's own checksums must pass ("Correct operation validated"), Dhrystone's self-check must pass, fpbench compares against reference values. An invalid run doesn't count.

---

## 3. How measurements were taken (decision D43)

The harness is [`tools/bench.py`](../../tools/bench.py):
- each configuration is **calibrated** so a run lasts long enough (about 10 s for CoreMark) to make timer noise negligible
- the process is pinned to one CPU core with `taskset -c 2`
- **1 warm-up run, then 5 measured runs**; the **median** is reported, with the min–max range
- the host CPU model, kernel, compiler versions, commit hash and date are recorded with every result
- the host was a **shared cloud VM** (Intel Xeon @ 2.10 GHz, 4 vCPUs), so a few percent of noise is expected. The project says so next to every result.
- native builds use the same source and flags (host gcc 13.3, same as the cross gcc 13.3)

The project's rule: **no number is reported unless the harness produced it.** Earlier targets like "120 MIPS" and "45 → 4 cycles" were hypotheses, replaced by measurements.

---

## 4. Final results (Phase 11, commit `567255a`)

### 4.1 User mode

| workload | interpreter | **Bridge-V JIT** | JIT, softmmu | qemu-riscv64 8.2 | native x86-64 |
|---|---:|---:|---:|---:|---:|
| CoreMark (it/s) | 454 | **13,409** | 7,526 | 8,988 | 25,531 |
| Dhrystone (per s) | 472,238 | **20,728,869** (11,798 DMIPS) | 10,115,192 | 4,758,387 | 45,562,668 |
| fpbench (units/s) | 118 | **3,711** | 2,654 | 636 | 16,589 |

| workload | JIT guest MIPS | JIT vs QEMU | JIT vs native | JIT vs interpreter |
|---|---:|---:|---:|---:|
| CoreMark | 4,807 | **1.49×** | 52.5% | 29.5× |
| Dhrystone | 6,955 | **4.36×** | 45.5% | 43.9× |
| fpbench | 2,600 | **5.83×** | 22.4% | 31.5× |

### 4.2 System mode: Linux boot to a shell

| config | time to shell | guest instructions | MIPS |
|---|---:|---:|---:|
| **Bridge-V JIT** | **1.48 s** | 926 M | 592 |
| Bridge-V interpreter | 10.49 s | 895 M | 85 |
| qemu-system-riscv64 | 1.54 s | — | — |

---

## 5. Where the speed comes from (Phase 5, commit `668723f`)

Each JIT level adds one technique. CoreMark iterations per second:

| config | it/s | vs interpreter | what was added |
|---|---:|---:|---|
| interpreter | 510 | 1.0× | — |
| `jit-naive` (`--regalloc none --no-chain`) | 791 | 1.6× | translate to x86, but every block returns to the dispatcher and every guest register lives in memory |
| `jit+chain` | 7,024 | 13.8× | **block chaining + jump cache** |
| `jit+pinned` | 10,168 | 19.9× | **4 guest registers pinned** in R12–R15 |
| `jit+linear` | 13,498 | 26.5× | **IR optimizer + linear-scan allocator with lazy write-back** |
| qemu | 8,898 | 17.5× | |
| native | 25,962 | 50.9× | |

### 5.1 Why each step helps

**Naive JIT (1.6×).** Removes the interpreter's per-instruction `match` and decode checks, but every block still makes a round trip through `exit_jit` → Rust → hash lookup → `enter_jit`, and every guest register access is a memory load or store. The dispatcher is entered **180,762 times per million guest instructions**, so the round-trip overhead dominates.

**Chaining (8.9× more).** Blocks jump straight into each other, and returns go through the inline jump cache. Dispatcher entries fall to **10 per million** instructions. The CPU now runs long stretches of direct jumps that its branch predictor handles perfectly, instead of one heavily shared indirect jump that it keeps mispredicting. (File 10.)

**Pinning (+45%).** `sp`, `ra`, `a0` and `a5` cross nearly every block boundary. With them pinned in callee-saved registers, they never touch memory at block edges or around helper calls. (File 08.)

**Optimizer + linear scan (+33%).** Within a block: registers are read from memory once and reused (forwarding), constants are folded, overwritten writes vanish, and dirty registers are written back once, at the exit. Generated code shrank from 28.0 to 25.0 host bytes per guest instruction. (Files 07, 08.)

**Budget in a register (D46).** Keeping the budget counter in R9 instead of memory removed a store→load dependency chain through every block: CoreMark +10.5%, Dhrystone +22.4% in same-batch A/B runs.

**Inline FP (Phase 6, 60×).** On fpbench, sending FP instructions through the SoftFloat helper gave 58 units/s; inline SSE with fix-ups gave 3,508. (File 14.)

### 5.2 Translation cost is tiny

| config | translate time for all of CoreMark | share of the run | host bytes per guest instruction |
|---|---:|---:|---:|
| jit-naive | 2.7 ms | 0.022% | 31.3 |
| jit+linear | 9.2 ms | 0.071% | 25.0 |

The optimizing JIT takes about 3.4× longer to translate than the naive one, but that is still less than a tenth of a percent of the run: a good trade.

**But not for a Linux boot.** A boot runs a huge amount of code only once, so there translation was about 40% of the time. Phase 12 fixed that with an interpreter tier (§8 below and file 23).

### 5.3 Why Dhrystone and fpbench beat QEMU by more

- **Dhrystone** has very short blocks and many calls/returns, exactly where chaining, the jump cache and pinned `sp`/`ra` help most.
- **fpbench**: QEMU computes RISC-V floating point largely in software (its softfloat library) to get exact results; Bridge-V runs most FP as native SSE instructions and fixes up only the rare special cases.

---

## 6. The software TLB (Phase 7, `tools/tlb-bench.py`)

| | cost |
|---|---|
| TLB hit (throughput) | ~0.34 ns per access |
| TLB hit (added latency) | ~3 ns |
| TLB miss: page walk + fill | ~28 ns (~59 cycles) |

CoreMark with every access through the TLB (`--mem=softmmu`) keeps **56%** of direct-mode speed (7,526 vs 13,409 it/s), still 0.84× QEMU's user mode, which uses direct memory.

## 7. System-mode improvements (Phase 9, D51)

For the Linux boot, letting direct exits that leave a page look up the jump cache inline, tagging jump-cache entries with privilege flags, and flushing only one page on `sfence.vma addr`:
- dispatcher entries: 6.5 M → 0.7 M
- TLB fills: 1.44 M → 0.6 M
- time to shell: 1.80 s → 1.30 s (measured then; 1.48 s on the final, slower host)

---

## 8. Tiered translation (Phase 12, D63): don't translate what barely runs

The full story is in **file 23**. In short: a JIT block now runs in the interpreter until it has run 32 times (`--tier 32`, the default), and only then is it translated. Code that runs only a few times never pays the ~8 µs translation cost.

Measured on a GitHub-hosted runner (AMD EPYC 7763, a different machine from the tables above, so compare only within this table), with 7 boots per configuration taking turns:

| config | time to shell | blocks translated | host code |
|---|---:|---:|---:|
| `--tier 0` (every block translated, as before) | 1.30 s | 65,634 | 43.5 MiB |
| **`--tier 32` (default)** | **1.06 s** | **18,023** | **12.3 MiB** |
| qemu-system-riscv64 | 1.41 s | | |

- The boot is **18% faster**, and Bridge-V goes from 1.08× to **1.33× QEMU** on that machine.
- CoreMark, Dhrystone and fpbench did not change beyond noise (+0.1%, +1.1%, −1.2%). Their hot loops are still translated, just 32 runs later.
- Why it works: in a boot, **31% of all blocks run exactly once** and 64% run fewer than 16 times.

---

## 9. Honest caveats (useful in interviews)

- **Noise.** A shared cloud VM varies by several percent between sessions. The same code measured 1.25 s and 1.48 s for the Linux boot on different days; a same-batch A/B showed identical speed, so the difference was the host.
- **The interpreter got slower** during Phases 8–10 (CoreMark 520 → ~454 it/s in the final matrix), which flatters "29.5× over the interpreter". After Phase 11 the cause was found (two self-modifying-code checks on the interpreter's store path) and fixed (D61); the fixed interpreter is back at Phase 7 speed. Phase 5's **26.5×** (CoreMark) and **39.6×** (Dhrystone) remain the conservative figures.
- **Two machines.** The Phase 12 tiered-translation numbers come from a GitHub runner (AMD EPYC), not the Xeon used for everything else. Never mix the two in one comparison.
- **Threads don't run in parallel.** Multi-threaded CoreMark runs at ~88% of single-thread speed in Bridge-V, while QEMU runs threads in parallel (38,314 it/s on 4 cores).
- **Only one host.** Numbers on your machine will differ. Re-measure before quoting them on other hardware.
- **Rejected ideas** were measured too: loop-resident registers doubled a toy loop but didn't move CoreMark or Dhrystone, so they were reverted (D45); a different pinned-register set wasn't a clear win, so the default stayed (D42).

---

## 10. Seeing the numbers yourself

```
bridgev run --engine jit --stats guest/build/bench/coremark-rv64.elf 0x0 0x0 0x66 150000
tools/demo-milestone-a.sh                # CoreMark: interpreter vs JIT, prints the speedup (~2 min)
python3 tools/bench.py --quick           # a short version of the whole matrix
bridgev run --engine jit --profile-tbs --stats prog.elf   # which translated blocks are hottest
```

`--stats` prints guest MIPS, TBs translated, code size, dispatcher entries, exits by reason, chain links, jump-cache fills and allocator activity. `--profile-jit` adds the jump-cache hit rate (it changes the generated code, so don't time with it).

---

## Check yourself

1. What is MIPS? Why does it count guest instructions, not host ones?
2. List three things the benchmark harness does to make its numbers trustworthy.
3. Recite the headline results: CoreMark vs QEMU and vs native, and the Linux boot time.
4. Which single technique gave the biggest speed-up, and why?
5. Why does pinning four registers help so much when there are 32?
6. Why is the optimizer worth its extra translation time?
7. Why is "29.5× over the interpreter" not the fairest figure?
8. What would you say if an interviewer asked "Is Bridge-V faster than QEMU?"
9. Why was translation cost negligible for CoreMark but about 40% of the Linux boot? What did Phase 12 change?
