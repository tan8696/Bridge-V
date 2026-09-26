# Bridge-V

**A dynamic binary translator that runs RISC-V (RV64GC) Linux programs on x86-64 by JIT-compiling guest basic blocks into native x86-64 machine code.**

It has four core subsystems:
- a hand-written x86-64 emitter, with W^X code memory
- direct block chaining by hot-patching jump targets
- guest registers pinned in R12–R15, plus a linear-scan allocator
- an SV39 software MMU with an inline software TLB

> **Status:** design complete; implementation starting at Phase 0.

| Document | What it covers |
|---|---|
| [`docs/PROJECT_EXPLAINED.md`](docs/PROJECT_EXPLAINED.md) | What Bridge-V is, how it works, what it's used for. Start here. |
| [`docs/ROADMAP.md`](docs/ROADMAP.md) | Detailed phase-by-phase build plan with acceptance criteria. |
| [`docs/phase-reports/`](docs/phase-reports/) | A detailed report for every completed phase. |
| [`CLAUDE.md`](CLAUDE.md) | The full engineering specification and design decision log. |

**Milestones**
- **A:** CoreMark and Dhrystone, with a JIT vs interpreter speedup report.
- **B (stretch):** boot Linux 6.6 to a BusyBox shell.
