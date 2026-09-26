# Bridge-V

**A dynamic binary translator that runs RISC-V (RV64GC) Linux programs on x86-64 by JIT-compiling guest basic blocks into native x86-64 machine code.**

It has four core subsystems:
- a hand-written x86-64 emitter, with W^X code memory
- direct block chaining by hot-patching jump targets
- guest registers pinned in R12–R15, plus a linear-scan allocator
- an SV39 software MMU with an inline software TLB

> **Status:** Phases 0–4 complete: interpreter, JIT with a hand-written x86-64 encoder, W^X code buffer, block chaining, an inline jump cache, and an IR with constant folding and a register allocator (guest registers pinned in R12–R15 plus linear scan), all checked in lockstep against the interpreter and by a random-block fuzzer (10⁶ blocks). CoreMark runs at 12,754 iterations/s, 23.6× the interpreter and 1.32× QEMU. Benchmarks and Milestone A (Phase 5) are next. See `docs/phase-reports/`.

| Document | What it covers |
|---|---|
| [`docs/PROJECT_EXPLAINED.md`](docs/PROJECT_EXPLAINED.md) | What Bridge-V is, how it works, what it's used for. Start here. |
| [`docs/ROADMAP.md`](docs/ROADMAP.md) | Detailed phase-by-phase build plan with acceptance criteria. |
| [`docs/phase-reports/`](docs/phase-reports/) | A detailed report for every completed phase. |
| [`CLAUDE.md`](CLAUDE.md) | The full engineering specification and design decision log. |

**Milestones**
- **A:** CoreMark and Dhrystone, with a JIT vs interpreter speedup report.
- **B (stretch):** boot Linux 6.6 to a BusyBox shell.
