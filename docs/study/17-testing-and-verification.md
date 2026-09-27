# 17 · Testing and verification: how we know it's correct

## What you will learn

- why a binary translator is unusually hard to test
- the layers of evidence: unit tests, golden tests, official test suites, differential testing, fuzzing, reference comparison
- how **lockstep** mode works, step by step
- how the random **fuzzers** work, and what "shrinking" means
- how to run the tests yourself

Folder: [`tests/`](../../tests/). The rule of the project (CLAUDE.md §21): *nothing is "done" without evidence*, and every bug fix adds a regression test.

---

## 1. Why this is hard

- **One wrong bit breaks everything, silently.** A wrong sign extension doesn't crash immediately; it corrupts a value that surfaces a million instructions later as a wrong answer.
- **Crashes have no error messages.** A bad x86 encoding is just garbage bytes; the process dies with "Segmentation fault" and no hint why.
- **Two whole ISAs** must be right in every detail.
- **Timing-dependent behaviour** (interrupts, timers) makes runs hard to reproduce.

So Bridge-V builds its confidence from many independent directions, each able to catch bugs the others miss.

---

## 2. The layers of evidence

| Layer | What it checks | Where |
|---|---|---|
| Unit tests | small functions: ALU edge cases, trap rules, TLB, cache lookup, chain-patch arithmetic | `#[cfg(test)] mod tests` inside many `src/` files |
| Decoder golden tests | the decoder and disassembler against LLVM's assembler | `tests/decoder_vectors.rs` |
| Encoder golden tests | every x86 instruction form against an independent disassembler | `tests/emitter_golden.rs` |
| Official test suite | 244 riscv-tests under every engine | `tests/riscv_tests.rs` |
| Reference comparison | real C programs: same output as QEMU | `tests/user_programs.rs` |
| IR property tests | lift + every optimizer pass preserve meaning | `tests/ir_passes.rs` |
| Differential fuzzing | random integer blocks, random FP blocks, random MMU accesses: JIT = interpreter | `tests/fuzz_blocks.rs`, `fuzz_fp.rs`, `softmmu.rs` |
| Lockstep | whole programs and a whole Linux boot, compared after every block | `--engine lockstep` |
| Integration | CLI behaviour, precise faults, no RWX memory, GDB stub, Linux boot | `tests/cli.rs`, `gdb.rs`, `linux_boot.rs` |

At the final commit: `cargo test` → **134 passed, 0 failed, 9 ignored** (the ignored ones are the Linux boot tests, which need downloaded images and run in a separate CI job).

---

## 3. Golden tests: compare with a trusted outsider

A **golden test** compares output against a known-correct reference that was produced independently.

**Decoder** ([`tests/decoder_vectors.rs`](../../tests/decoder_vectors.rs)): `tools/gen-decoder-vectors.py` asked LLVM's assembler (`llvm-mc`) to encode about 1,500 RISC-V instructions and saved the results in `tests/data/rv64_vectors.txt`. The test decodes each encoding and requires Bridge-V's disassembly to match LLVM's text **exactly**. LLVM's own compressed encodings also check the compressed-instruction expansion. The file is committed, so the test doesn't need LLVM installed.

**Encoder** ([`tests/emitter_golden.rs`](../../tests/emitter_golden.rs)): every emitter function is called with every register in every position, and with displacements at the boundaries (−129, −128, 127, 128, …). The bytes are decoded with the independent `iced-x86` library, and the decoded mnemonic, operands and length must match what was requested. Named tests pin the exact bytes of each gotcha (R12 needs SIB, R13 needs a displacement, SIL needs REX, …) and the interview examples in CLAUDE.md §28.5 (`interview_examples_28_5`).

---

## 4. The official riscv-tests

[riscv-tests](https://github.com/riscv-software-src/riscv-tests) is the official RISC-V test suite: hundreds of small bare-metal programs, each testing one instruction or feature. Each writes `1` to a special memory word called `tohost` on success, or `(n << 1) | 1` if test case `n` failed.

- Bridge-V builds **244** of them (`tools/build-riscv-tests.sh`): user-level integer, multiply, atomic, float, double and compressed (`rv64ui/um/ua/uf/ud/uc`) in both the `p` (physical memory) and `v` (virtual memory, with demand paging) environments, plus the machine- and supervisor-mode suites (`rv64mi`, `rv64si`).
- `bridgev run --mode bare` loads them at `0x8000_0000` in M-mode and watches `tohost` at every block boundary ([`src/system/bare.rs`](../../src/system/bare.rs)).
- [`tests/riscv_tests.rs`](../../tests/riscv_tests.rs) runs **all 244 under every engine** (interpreter, JIT, lockstep; direct and softmmu). All pass.

**Validating the tests themselves** (decision D20): before trusting a test binary, it was run under QEMU. Tests that QEMU itself gets wrong are listed in `tests/data/qemu-known-failures.txt`. So a failure is always Bridge-V's bug, never a broken test binary.

---

## 5. Reference comparison with QEMU

The guest C programs (`guest/c/`) were run once under `qemu-riscv64`, and their stdout and exit codes saved in `tests/data/expected/`. [`tests/user_programs.rs`](../../tests/user_programs.rs) runs every built variant (`-O0`/`-O2`, with and without compressed instructions, static and dynamic) under every engine and memory backend and requires **byte-identical** output. `tools/ref-check.sh` re-checks the references against QEMU in CI.

---

## 6. Lockstep: the JIT checked against the interpreter, block by block

`--engine lockstep` ([`src/jit/lockstep.rs`](../../src/jit/lockstep.rs)) is the project's most powerful bug-finder. For **every** translation block:

```
1. snapshot   = the architectural state (x, f, pc, icount, fflags, frm, privilege, reservation, CSRs)
2. turn on memory write logging and MMIO recording
3. REFERENCE: run the block's instructions in the interpreter (exec_block)
4. remember the interpreter's final state and the new value at every written address
5. UNDO: restore the snapshot, undo every memory write (from the log)
6. CHECKED: run the JIT's translation of the same block, with budget = exactly its length
7. COMPARE: registers, pc, icount, fcsr, privilege, reservation, CSRs, the exit kind,
            and memory at every address the interpreter wrote
8. if anything differs: print the block's guest disassembly, its x86 code and the
   differences, and stop.  Otherwise continue from the (identical) state.
```

Details that make it work:
- **Exactly one block per comparison.** The JIT runs with a budget equal to the block's length. If the block's exit is chained (or its `jalr` hits the jump cache), control reaches the next block's prologue, which finds the budget used up and exits before executing anything (D32). So chaining and the jump cache are exercised, yet each comparison covers exactly one block.
- **Deterministic time.** Lockstep makes the `time` CSR follow the instruction count (D29), so both runs read the same value; otherwise every `rdtime` would look like a difference.
- **Devices run once** (D52). A device read can change device state (reading the UART's receive register removes a byte). So the interpreter run performs its device accesses and **records** them; the JIT run **replays** the recorded values and only **compares** its own accesses (address, size, direction, value, count). Without this, a PLIC read-modify-write ran twice and diverged after 77.5 million identical blocks.
- **Snapshots skip non-architectural state.** The 64 KiB jump cache isn't architectural; copying it for every block would dominate the run time.

**Result:** CoreMark, Dhrystone, all test programs, and a complete Linux boot plus shell session (**84 million blocks**) run in lockstep with **no divergence**.

There is a hidden test flag, `--inject-bug`, which deliberately miscompiles `addi`, to prove that lockstep catches it.

---

## 7. Differential fuzzing

**Fuzzing** means testing with large amounts of random input. **Differential** means comparing two implementations that should agree. Bridge-V uses the `proptest` library.

### 7.1 Integer blocks ([`tests/fuzz_blocks.rs`](../../tests/fuzz_blocks.rs))

`tests/common/rvgen.rs` generates random straight-line RISC-V blocks: arithmetic of every kind, loads and stores confined to a scratch page, branches, and terminators that sometimes fault. Each block runs from random initial registers:
- once in the interpreter
- once as a JIT block under **several back-end configurations**: every `--regalloc` level, the linear allocator without pinned registers, and without BMI2 (to exercise the RCX-constrained shifts)
- twice per configuration: once as a single block, once with its self-loops linked and a budget of three iterations (to test chained loops)

Registers, FP registers, `pc`, `icount`, reservation, exit, and the scratch page must all be identical. It has been run for **1,000,000 blocks**.

### 7.2 FP blocks ([`tests/fuzz_fp.rs`](../../tests/fuzz_fp.rs))

Random FP instructions with operands biased towards special values (NaN payloads, signaling NaNs, ±0, ±∞, subnormals, conversion boundaries), random rounding modes and flags. The JIT must match SoftFloat **bit for bit**. It also forces the fast FP variant onto states where it doesn't apply, to check that the prologue guard exits correctly. **1,000,000 cases**; it found the FMA NV bug (file 14).

### 7.3 Softmmu ([`tests/softmmu.rs`](../../tests/softmmu.rs))

Random loads and stores through a random Sv39 layout with read-write, read-only, clean (D = 0), user, execute-only, unmapped and MMIO pages, misaligned and page-crossing accesses, and random SUM/MXR. Each block runs twice: once with a cold TLB (walks, fills, A/D updates) and once with the TLB it left (fast-path hits). It also includes a writable alias of the block's own code page, so it produces self-modifying stores.

### 7.4 Shrinking and regression files

When `proptest` finds a failing input, it **shrinks** it: repeatedly tries smaller, simpler versions until it finds a minimal block that still fails. That turns "a random 40-instruction block fails" into "these 2 instructions fail". Failing cases are saved in `tests/*.proptest-regressions` files and replayed first on every run, so a fixed bug stays fixed. CI uses a fixed random seed (`PROPTEST_RNG_SEED`) for reproducibility.

### 7.5 The IR evaluator as an oracle

[`tests/ir_passes.rs`](../../tests/ir_passes.rs) runs random blocks through the lifter and then through `ir::eval` (an IR interpreter), before and after each optimizer pass, and compares with the real interpreter. That catches front-end bugs (lifting, optimization) separately from back-end bugs (register allocation, encoding).

---

## 8. Continuous integration

`.github/workflows/ci.yml` runs on every push: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` (warnings are errors), building the guest programs, checking them against the QEMU references, building riscv-tests, and `cargo test`. A separate job boots Linux.

---

## 9. Running the tests yourself

On Linux x86-64 (or WSL2; see file 22), after `tools/setup.sh`, `tools/build-guests.sh` and `tools/build-riscv-tests.sh`:

```
cargo test                              # everything (a few minutes)
cargo test --test decoder_vectors       # one test file
cargo test division_edge_cases          # tests whose name contains this text
PROPTEST_CASES=100000 cargo test --release --test fuzz_blocks    # a longer fuzz run
cargo test --release --test linux_boot -- --ignored              # after tools/fetch-guest-images.sh
target/release/bridgev run --engine lockstep guest/build/fib-O2.elf 20   # lockstep by hand
```

---

## Check yourself

1. Why is a binary translator harder to test than most programs?
2. What is a golden test? Give the two golden tests in this project and their references.
3. How do riscv-tests report success or failure? How were the test binaries themselves validated?
4. Walk through the eight steps of lockstep for one block.
5. How does lockstep compare exactly one block even when blocks are chained?
6. Why must device accesses be recorded and replayed in lockstep?
7. What is differential fuzzing? What is shrinking, and why is it useful?
8. Which test would most likely catch: (a) a wrong REX byte, (b) a wrong `fold` rule, (c) a missing `movsxd` after `addw`, (d) a wrong TLB permission?
