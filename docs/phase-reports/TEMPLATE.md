# Phase NN report: <phase name>

> Copy this file to `docs/phase-reports/phase-NN-<short-name>.md` when a phase finishes. Fill **every** section. Write "n/a" and a reason if a section doesn't apply.
> Rules:
> - Facts only. Every number must come from a command whose output is included, or linked, below.
> - Keep it readable for the project owner. Explain *what* was done *and why*, not just list files.

| Field | Value |
|---|---|
| Phase | NN: <name> (see `docs/ROADMAP.md`) |
| Status | Complete / Complete with deferrals / Partial |
| Dates | <start> → <end> |
| Branch / final commit | `<branch>` @ `<sha>` |
| Sessions used | <n> (rough) |

## 1. Summary (5–10 lines)
What this phase set out to do, what was achieved, and the single most important result.

## 2. Planned vs delivered
| Task ID | Task | Status (done / deferred / dropped) | Notes |
|---|---|---|---|
| PN.1 | … | done | … |

## 3. What was built
For each component: purpose, key files (`path:line` where useful), and how it works in plain language. Include diagrams or code excerpts where they help.

## 4. Design decisions made
New or changed decisions (mirrored into CLAUDE.md §3 as D-numbers), with the alternatives considered and why this one won.

## 5. How it works: worked example
Trace one concrete example end to end (e.g. one guest instruction or block, from bytes to decoded form to IR to x86 bytes to result).

## 6. Tests and verification
- Test inventory: which suites were added, and how many tests.
- Commands run, with exact output summaries:
  ```
  $ cargo test ...
  test result: ok. N passed; 0 failed ...
  ```
- Acceptance criteria from the roadmap, each marked ✅/❌ with its evidence.

## 7. Performance (if applicable)
Measured numbers only: the table, methodology, host info, and the caveat that this is a noisy cloud VM.

## 8. Bugs found and fixed
| Symptom | Root cause | Fix (commit) | Regression test |
|---|---|---|---|

## 9. Deviations from the plan / spec
What differs from CLAUDE.md or ROADMAP.md, and why. (The spec files were updated in the same commit.)

## 10. Known limitations and technical debt
What is missing, fragile or deferred, and where it will be addressed.

## 11. How to reproduce
Exact commands from a clean checkout to rebuild and re-run everything in this report.

## 12. Next steps
What the next phase needs from this one, and the first task to start with.
