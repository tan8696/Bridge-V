# Design stage report: project definition and planning (pre-Phase 0)

| Field | Value |
|---|---|
| Phase | Design stage, before Phase 0 (see `docs/ROADMAP.md`) |
| Status | Complete |
| Dates | 2026-09-26 |
| Branch | `claude/compassionate-babbage-ul3prl` |
| Sessions used | 1 |

## 1. Summary
No code was written, on purpose. This stage turned the project brief ("Build a RISC-V 64-bit → x86-64 JIT dynamic binary translator") into four documents:
- A complete engineering specification (`CLAUDE.md`).
- A detailed phase-by-phase build plan (`docs/ROADMAP.md`).
- A plain-language explanation of the project (`docs/PROJECT_EXPLAINED.md`).
- A reporting template for every future phase (`docs/phase-reports/TEMPLATE.md`).

It also verified what the development container offers, so that later phases start on facts rather than assumptions.

## 2. Planned vs delivered
| Item | Status | Notes |
|---|---|---|
| Project specification (`CLAUDE.md`) | done | 29 sections: goals, decisions D1–D18, architecture, ISA and x86 references, subsystem designs, verification, benchmarks, roadmap summary, conventions, interview prep |
| Detailed roadmap (`docs/ROADMAP.md`) | done | Phases 0–11, each with task IDs, deliverables, tests, acceptance criteria, risks and required report contents |
| Project explainer (`docs/PROJECT_EXPLAINED.md`) | done | For non-specialists as well as interviewers |
| Phase-report template | done | Mandatory at the end of every phase |
| `README.md` | done | Short landing page for the GitHub repo |
| Push to GitHub | done | Blocked at first (§10); resolved once the Claude GitHub App was installed |

## 3. What was produced
- **`CLAUDE.md`**, the single source of truth. The design choices it records:
  - The language is Rust.
  - A hand-written x86-64 emitter, with no assembler libraries at runtime.
  - Dual-mapped W^X code memory.
  - Block chaining by patching the rel32 of `jmp`/`jcc` instructions.
  - A jump cache for indirect jumps.
  - Four guest registers pinned in R12–R15, plus a linear-scan allocator.
  - A direct memory backend for user mode, and a SoftMMU with an inline, direct-mapped TLB for system mode.
  - Eager invalidation of translated code on writes (SMC).
  - SoftFloat for exact floating point.
  - A built-in SBI and a QEMU-`virt`-compatible machine for booting Linux.
- **`docs/ROADMAP.md`**: 12 phases (0–11). Milestone A (CoreMark/Dhrystone speedup) is marked **must-have**, and the Linux boot (Milestone B) is marked a stretch, because of the ~$100 credit budget.
- **`docs/PROJECT_EXPLAINED.md`**: what a dynamic binary translator is, where they are used, how Bridge-V works step by step (with a worked example), and which skills it demonstrates.
- **`docs/phase-reports/TEMPLATE.md`**: 12 required sections per phase report.

## 4. Design decisions made
All 18 decisions are recorded in CLAUDE.md §3 (D1–D18). The most consequential:
- **D1, Rust instead of C++.** Memory safety for the large "ordinary" parts (decoder, loader, MMU, syscalls), with `unsafe` confined to JIT and memory-mapping code.
- **D9, two memory backends.** Direct (fast) for user programs, and SoftMMU (SV39 + TLB) for full-system mode. Comparing the two is itself a talking point.
- **D13, SoftFloat first.** Floating-point bit-exactness is notoriously hard. Starting from the same library the reference simulator (Spike) uses removes a whole class of bugs.
- **D15, built-in SBI before OpenSBI.** This shortens the Linux bring-up path considerably.

## 5. Verification performed
The environment was checked in the container on 2026-09-26:
- Host: Intel Xeon 2.8 GHz, 4 vCPU, 15 GiB RAM. The CPU flags include `bmi2 fma avx2 avx512f`.
- Installed: `rustc`/`cargo` 1.94.1, `gcc` 13.3, `clang` 18 with the riscv64 target, `ld.lld`, `llvm-mc`, `llvm-objdump`, `gdb`.
- Available through apt (not yet installed): `gcc-riscv64-linux-gnu`, `qemu-user` 8.2, `qemu-system-misc`.
- The x86 encodings quoted in the interview section were assembled with `llvm-mc` and match exactly:
  - `cmp r14, rsi` = `49 39 F6`
  - `jne` at 0x10 → 0x200 = `0F 85 EA 01 00 00`
  - `jmp` at 0x1040 → 0x2000 = `E9 BB 0F 00 00`
  - The TLB fast-path instructions, `mfence` and `roundsd` also match.

## 6. Tests
n/a. There is no code yet.

## 7. Performance
n/a. The targets in CLAUDE.md §22 are hypotheses, and will be replaced by measurements.

## 8. Bugs found
None.

## 9. Deviations
None.

## 10. Known issues
- **GitHub push is blocked.** `git push` returns HTTP 403 ("Claude doesn't have GitHub access to tan8696/Bridge-V"). The GitHub connector can read the repository, but writing returns 403 ("Resource not accessible by integration").
  - **Fix (repository owner):** install or configure the Claude GitHub App for the `tan8696` account at <https://github.com/apps/claude/installations/select_target>. Give it access to `Bridge-V` with read and write permission on repository contents. Then reconnect at <https://claude.ai/connect-github> if needed.
  - **Resolved 2026-09-26:** the owner installed the Claude GitHub App, and the branch `claude/compassionate-babbage-ul3prl` was pushed. On the way, a pending collaborator invite to a GitHub *user* named "Claude" was found to be the wrong mechanism, and it was flagged for removal.
- The four open questions in `docs/ROADMAP.md` §18 are awaiting the owner's answers: license, whether to commit to Milestone B, where to store kernel images, and the CI budget.

## 11. How to reproduce
Nothing to build yet. Read `CLAUDE.md`, then `docs/ROADMAP.md`.

## 12. Next steps
Start **Phase 0, task P0.1** (repository hygiene), then P0.2 (Cargo skeleton) and P0.3 (`tools/setup.sh`).
