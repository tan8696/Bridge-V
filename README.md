# Bridge-V

**A dynamic binary translator that runs RISC-V (RV64GC) Linux programs on x86-64 by JIT-compiling guest basic blocks into native x86-64 machine code.**

It has four core subsystems:
- a hand-written x86-64 emitter, with W^X code memory
- direct block chaining by hot-patching jump targets
- guest registers pinned in R12–R15, plus a linear-scan allocator
- an SV39 software MMU with an inline software TLB

> **Status:** Phases 0–8 complete, including **Milestone A**: CoreMark runs at 13,498 iterations/s under the JIT, **26.5× the interpreter, 1.52× `qemu-riscv64` and 52% of native x86-64** on the same machine (Dhrystone: 39.6× the interpreter, 4.55× QEMU). Results and method: [`docs/BENCHMARKS.md`](docs/BENCHMARKS.md); reproduce with `tools/demo-milestone-a.sh`. Phase 6 inlines floating point (SSE2/FMA3, bit-exact against SoftFloat): 60× the helper path and 5.6× QEMU on an FP benchmark. Everything is checked in lockstep against the interpreter and by random-block fuzzers. Phase 7 adds M/S/U privilege and Sv39 virtual memory with an inline software TLB: all 244 riscv-tests pass (including the virtual-memory and machine/supervisor suites), and CoreMark keeps 54% of its speed with every access translated. Phase 8 handles self-modifying code: writes to pages holding translated code invalidate exactly those translations (x86-style patching without FENCE.I works too). Booting Linux (Phase 9, Milestone B) is next. See `docs/phase-reports/`.

| Document | What it covers |
|---|---|
| [`docs/PROJECT_EXPLAINED.md`](docs/PROJECT_EXPLAINED.md) | What Bridge-V is, how it works, what it's used for. Start here. |
| [`docs/ROADMAP.md`](docs/ROADMAP.md) | Detailed phase-by-phase build plan with acceptance criteria. |
| [`docs/phase-reports/`](docs/phase-reports/) | A detailed report for every completed phase. |
| [`docs/BENCHMARKS.md`](docs/BENCHMARKS.md) | Measured results from the benchmark harness. |
| [`CLAUDE.md`](CLAUDE.md) | The full engineering specification and design decision log. |

**Milestones**
- **A:** CoreMark and Dhrystone, with a JIT vs interpreter speedup report.
- **B (stretch):** boot Linux 6.6 to a BusyBox shell.
