# Phase 12: interpreter tier (D63) measurements

Measured on GitHub-hosted runners (`ubuntu-24.04`, AMD EPYC 7763, 4 vCPU, shared cloud VM, `taskset -c 2`) by the `bench` workflow (`.github/workflows/bench.yml`), because this phase had no Linux machine. These numbers compare configurations within one run only. They are not comparable with the Phase 11 numbers (Intel Xeon @ 2.10 GHz).

| File | Commit | GitHub Actions run | What |
|---|---|---|---|
| [`boot-sweep-53f9030.md`](boot-sweep-53f9030.md) | `53f9030` | 36350506481 | Linux boot, time to shell at `--tier` 0, 1, 4, 16, 64, 256 and QEMU (5 interleaved runs); a fully interpreted boot (`--tier 1000000000`) with the histogram of block run counts |
| [`boot-confirm-2e498ee.md`](boot-confirm-2e498ee.md) | `2e498ee` | 36351084311 | the same at `--tier` 0, 16, 32 (default), 64 and QEMU (7 interleaved runs), and the histogram again |
| [`user-2e498ee.md`](user-2e498ee.md) | `2e498ee` | 36351084311 | CoreMark, Dhrystone, fpbench: default (`jit+linear`, tier 32) against `jit-tier0` (3 runs each) |

Summary: time to shell 1.30 s → 1.06 s with the default tier (QEMU 1.41 s); 72% fewer blocks translated and 72% less host code; the user-mode scores are within ±1.2% (runner noise). Analysis: [`docs/phase-reports/phase-12-tiered-translation.md`](../../phase-reports/phase-12-tiered-translation.md) §7.
