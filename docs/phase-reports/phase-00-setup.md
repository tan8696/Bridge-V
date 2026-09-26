# Phase 00 report: Project setup and toolchain

| Field | Value |
|---|---|
| Phase | 00: Project setup and toolchain (see `docs/ROADMAP.md`) |
| Status | **Complete with one deferral** (LICENSE: awaiting the owner's choice) |
| Dates | 2026-09-26 → 2026-09-26 |
| Branch / commits | `claude/compassionate-babbage-ul3prl`: `2119c1d` (P0.1–P0.3), `46fda7a` (P0.4, P0.6), `d2e2993` (P0.5), `7ff02f2` (P0.8, P0.9), plus the commit containing this report |
| CI | Run #1 on `7ff02f2`: **success** (both jobs), <https://github.com/tan8696/Bridge-V/actions/runs/36238859655> |
| Sessions used | 1 |

## 1. Summary
Phase 0 turned an empty repository into a working development environment. What now exists:
- A Rust crate (`bridge-v`, binary `bridgev`) with the full module tree from CLAUDE.md §6, a stub CLI, and lint and test gates.
- A one-command host setup script: RISC-V cross-compiler, QEMU reference emulators, devicetree compiler.
- 26 RISC-V guest test programs, with reference outputs recorded from QEMU.
- The official riscv-tests (244 in-scope test binaries), validated under QEMU.
- CI on GitHub Actions running all of it.

**The most important result:** every input Bridge-V will be tested with in Phase 1 has already been proven valid against an independent reference emulator:
- 26/26 guest programs match byte-exact under `qemu-riscv64`.
- 243/244 riscv-tests pass under `qemu-system-riscv64 -M spike`. The single failure is a documented QEMU bug.

So from Phase 1 on, a test failure means a Bridge-V bug, not a broken test.

## 2. Planned vs delivered
| Task | Status | Notes |
|---|---|---|
| P0.1 Repository hygiene | done (1 deferral) | `.gitignore`, `rust-toolchain.toml` (1.94), `rustfmt.toml`, `.editorconfig`. `clippy.toml` wasn't needed. **LICENSE deferred** (ROADMAP §18, owner decision). |
| P0.2 Cargo skeleton | done | Deps per D18; 53 source files matching §6; CLI stubs; 3 CLI tests |
| P0.3 `tools/setup.sh` | done | Idempotent: the second run reports "all packages already installed" |
| P0.4 Guest build infrastructure | done | 2 asm + 6 C programs → 26 ELFs |
| P0.5 Third-party sources | done | 3 shallow submodules; 244 riscv-tests; 3 toolchain problems fixed (§8) |
| P0.6 Reference emulator sanity | done | `ref-run.sh`, `ref-check.sh`: 26/26 byte-exact |
| P0.7 Test harness scaffolding | done | `tests/common/mod.rs` (`guest_elf`, `run_bridgev`) |
| P0.8 Continuous integration | done | 2 jobs, green on the first run |
| P0.9 Documentation scaffolding | done | `docs/BENCHMARKS.md` placeholder; README links verified |

## 3. What was built

### 3.1 Rust crate skeleton (`Cargo.toml`, `src/`)
- **Crate `bridge-v`**: library `bridgev` plus binary `bridgev`, edition 2024, Rust pinned to 1.94 via `rust-toolchain.toml`. rustup installed it automatically on first use.
- **Dependencies (D18):**
  - Runtime: `anyhow` 1.0.104, `clap` 4.6.7 (derive), `libc` 0.2.189, `rustc-hash` 2.1.3.
  - Dev: `iced-x86` 1.21.0 (decoder + Intel formatter only), `proptest` 1.11.0.
  - The runtime deps are declared now and used from Phase 1.
- **Profiles:**
  - `dev` uses `opt-level = 1`, so the interpreter's test suites run at usable speed.
  - `release` uses `panic = "abort"`, because helpers must never unwind through JIT frames (§8.4), plus thin LTO, `codegen-units = 1` and `debug = 1` (symbols for profiling).
- **Module tree:** 53 files (including `main.rs` and `lib.rs`) exactly as laid out in CLAUDE.md §6. Each starts with a `//!` doc comment stating its responsibility and the roadmap task that implements it (e.g. `src/jit/code_mem.rs`: "W^X code buffer … Phase 2 (P2.3)"). This makes the tree a navigable map of the architecture before any code exists.
- **CLI (`src/main.rs`):** subcommands `run <elf> [args…]`, `boot --kernel`, `disasm <elf>`, `bench <suite>`. Each one prints `not implemented yet (Phase N, see docs/ROADMAP.md)` and exits with status 2, so scripts can't mistake a stub for success.

### 3.2 Host setup (`tools/setup.sh`)
- It installs 15 apt packages (§4 of CLAUDE.md), but only the ones that are missing, and then prints the version of every tool.
- It tolerates `apt-get update` warnings, because the container's third-party PPAs are blocked by the network policy. It fails with a pointer to the proxy status endpoint only if `apt-get install` itself fails.

### 3.3 Guest programs (`guest/`, `tools/build-guests.sh`)

| Program | What it exercises |
|---|---|
| `asm/hello.S` | The absolute minimum: `write` + `exit` syscalls via `ecall`, PC-relative address, no stack, no libc. The first program Bridge-V will run. |
| `asm/loop.S` | 10⁸ iterations of `addi`/`bne` = 2×10⁸ guest instructions. It exits with 225, so the loop count is verified. This is the MIPS workload for later phases. |
| `c/hello.c` | glibc startup, `printf` |
| `c/printf_float.c` | F/D arithmetic, a subnormal (1e-310), NaN compare, −0.0, `sqrt`, `%a` hex-float formatting |
| `c/malloc.c` | brk heap, a 4 MiB allocation (above glibc's mmap threshold, so it exercises `mmap`), `realloc`, `free` |
| `c/qsort.c` | 10 000-element sort through a function pointer (indirect calls, i.e. JALR) |
| `c/setjmp.c` | `setjmp`/`longjmp` across recursion; non-zero exit status (7) |
| `c/strings.c` | glibc's optimized string routines, `snprintf`, `argv` handling (uses `strings.args`) |

- Each C program is built 4 ways: `-O0`/`-O2` × `rv64gc`/`rv64imafd_zicsr_zifencei` (`-nc` = no compressed instructions in the program's own code). That is 24 C ELFs plus 2 asm, 26 in total and about 13 MB.
- Caveat: the static glibc linked into `-nc` builds is itself rv64gc, so `-nc` binaries still contain compressed instructions inside libc. This is recorded in the script header.

### 3.4 Reference outputs (`tools/ref-run.sh`, `tools/ref-check.sh`, `tests/data/expected/`)
- `ref-check.sh --update` records each program's **exact stdout bytes** (`<name>.out`) and **exit status** (`<name>.code`) under `qemu-riscv64` 8.2.2.
- `ref-check.sh` re-runs all 26 variants and compares them with `cmp`.
- Phase 1's integration tests will compare `bridgev` against these same files.

### 3.5 riscv-tests (`third_party/riscv-tests`, `tools/build-riscv-tests.sh`, `tools/ref-riscv-tests.sh`)
- Built **out of tree** into `guest/build/riscv-tests-obj/`, so the submodule checkout stays clean.
- Only the in-scope suites are collected:

  | Suite | `-p` | `-v` |
  |---|---|---|
  | rv64ui | 54 | 54 |
  | rv64um | 13 | 13 |
  | rv64ua | 19 | 19 |
  | rv64uf | 11 | 11 |
  | rv64ud | 12 | 12 |
  | rv64uc | 1 | 1 |
  | rv64si | 7 | — |
  | rv64mi | 17 | — |
  | **Total** | | **244** |

  - `-p` = bare physical-memory environment (Phase 1 target).
  - `-v` = the Sv39 virtual-memory environment (Phase 7 target).
- A missing in-scope test fails the script. Out-of-scope suites (Zb*, Zfh, hypervisor, …) may fail to build and are ignored (CLAUDE.md §2).
- `ref-riscv-tests.sh` runs each test under `qemu-system-riscv64 -M spike`, whose HTIF device implements the `tohost` pass/fail protocol, with a 20 s timeout per test.

### 3.6 Submodules (pinned)
| Path | Commit | Date |
|---|---|---|
| `third_party/riscv-tests` | `bcffa2b3188b040c611f90dc0b6e422f54775a09` | 2026-09-25 |
| `third_party/riscv-tests/env` | `6de71edb142be36319e380ce782c3d1830c65d68` | 2025-04-01 |
| `third_party/coremark` | `1f483d5b8316753a742cbf5590caf5bd0a4e4777` | 2025-05-01 |
| `third_party/berkeley-softfloat-3` | `a0c6494cdc11865811dec815d5c0049fba9d82a8` | 2025-03-07 |

All three are marked `shallow = true` in `.gitmodules`.

### 3.7 CI (`.github/workflows/ci.yml`)
- **`build, lint, test`:**
  1. checkout with submodules
  2. `tools/setup.sh`
  3. Rust 1.94
  4. cargo cache
  5. `cargo fmt --check`
  6. clippy with `-D warnings`
  7. build guests
  8. `ref-check.sh`
  9. `cargo test` with `BRIDGEV_REQUIRE_GUESTS=1`, so missing guests fail instead of being skipped
- **`riscv-tests`:** setup, a cache keyed on the riscv-tests commit plus the build script hash, a build if not cached, then `ref-riscv-tests.sh`.

## 4. Design decisions made
Both were appended to CLAUDE.md §3.
- **D20, reference validation.**
  - Every test input is validated against QEMU before Bridge-V ever runs it: byte-exact stdout and exit code for user programs, and HTIF pass/fail for riscv-tests.
  - Tests that QEMU itself gets wrong live in `tests/data/qemu-known-failures.txt` and count as XFAIL. If one unexpectedly passes (XPASS), that is an error, so the list can't rot.
  - Alternative considered: Spike (the official RISC-V ISA simulator). It isn't packaged for Ubuntu and would have to be built from source. QEMU's `spike` machine gives the same HTIF protocol for free.
- **D21, riscv-tests scope and build flags.** Only the RV64GC and privileged suites are built, and three toolchain workarounds are documented in §8. Alternative considered: installing a bare-metal `riscv64-unknown-elf` toolchain. That isn't in the Ubuntu archive, and building it from source would cost far more than three flags.

## 5. How it works: worked example
Trace `guest/asm/hello.S` from source to verified reference output.

1. **Build.** `clang --target=riscv64-unknown-linux-gnu -march=rv64gc -mabi=lp64d -nostdlib -static -fuse-ld=lld` produces a static ELF.

   `readelf` output:
   ```
   Class: ELF64   Type: EXEC   Machine: RISC-V   Flags: 0x5, RVC, double-float ABI
   LOAD 0x010000 R      (rodata: "hello\n")
   LOAD 0x0111ae R E    (text)
   ```
2. **Machine code** (`objdump -d`, after switching to `lla`; see §8.7):
   ```
   111ae: 4505          li    a0,1          # compressed (C.LI), 2 bytes
   111b0: fffff597      auipc a1,0xfffff    # a1 = pc - 0x1000 ...
   111b4: ff858593      addi  a1,a1,-8      #      ... = address of "hello\n"
          4619          li    a2,6          # compressed
          04000893      li    a7,64         # __NR_write
          00000073      ecall
   ```
   This small program already mixes 16-bit and 32-bit instructions and uses PC-relative addressing (`auipc`+`addi`). Both are things the Phase 1 decoder must handle, and `auipc` is exactly the case the Phase 4 optimizer constant-folds (CLAUDE.md §7.5).
3. **Reference run.** `tools/ref-run.sh guest/build/hello.elf` prints `hello\n` and exits 0. `ref-check.sh --update` stores those 6 bytes in `tests/data/expected/asm-hello.out` and `0` in `asm-hello.code`.
4. **Check.** `ref-check.sh` re-runs it and compares with `cmp`. In Phase 1, `bridgev run guest/build/hello.elf` must produce the identical 6 bytes and exit code.

## 6. Tests and verification
All of these were run from a **clean state** (`rm -rf target guest/build`) on 2026-09-26:
```
$ ./tools/setup.sh                      (second run)
setup: all packages already installed
$ cargo build && cargo fmt --check && cargo clippy --all-targets -- -D warnings
LINT_OK                                 (clean build + lint: 13.6 s)
$ ./tools/build-guests.sh
build-guests: built 26 programs into guest/build
$ ./tools/ref-check.sh
ref-check: 26 passed, 0 failed
$ BRIDGEV_REQUIRE_GUESTS=1 cargo test
test guest_helper_skips_or_finds_hello ... ok
test version_exits_zero ... ok
test unimplemented_subcommands_exit_two ... ok
test result: ok. 3 passed; 0 failed
$ ./tools/build-riscv-tests.sh
build-riscv-tests: 244 in-scope test ELFs in guest/build/riscv-tests (0 missing)
$ ./tools/ref-riscv-tests.sh
ref-riscv-tests: 243 passed, 1 expected failures (QEMU limitations), 0 failed
```
**Negative test of the checker:** I set `tests/data/expected/qsort.code` to a wrong value on purpose. `ref-check.sh` then reported `22 passed, 4 failed` with exit status 1, which proves it detects mismatches in all four variants. The file was restored afterwards.

**Acceptance criteria** (ROADMAP Phase 0):
| Criterion | Result | Evidence |
|---|---|---|
| `cargo build`, `cargo test`, `cargo fmt --check`, clippy clean | ✅ | Output above; CI steps 5–9 |
| `tools/setup.sh` succeeds | ✅ | Local (first and second run) and CI |
| `tools/build-guests.sh` produces all guest ELFs | ✅ | 26 built |
| riscv-tests built | ✅ | 244 in-scope, 0 missing |
| qemu reference outputs match | ✅ | 26/26 byte-exact; riscv-tests 243/244 plus 1 documented QEMU XFAIL |
| CI green | ✅ | Run #1, both jobs `success` |

## 7. Performance
n/a. Nothing executes guest code yet. The only timings recorded:
- Clean build + clippy: 13.6 s.
- riscv-tests build: ~5 s.
- All 244 riscv-tests under QEMU: 5.3 s.
- CI run: ~1.5 min.

## 8. Bugs and problems found and fixed
| # | Symptom | Root cause | Fix | Regression guard |
|---|---|---|---|---|
| 1 | `apt-get update` shows 403 for two PPAs | Container network policy blocks `ppa.launchpadcontent.net` | `setup.sh` tolerates `update` warnings; the Ubuntu archive is sufficient | CI runs `setup.sh` |
| 2 | `setjmp.c` failed to build: `-Werror=infinite-recursion` | GCC 13 flags a recursive function with no normal return path (it always `longjmp`s) | Added a never-taken `return 0` path | `-Wall -Werror` in `build-guests.sh` |
| 3 | `ref-check.sh` ignored trailing newlines | Shell command substitution strips them | Capture stdout to a temp file and compare with `cmp` | Negative test above |
| 4 | riscv-tests `-v` link error: `dangerous relocation: The addend isn't allowed for R_RISCV_GOT_HI20` | Ubuntu's cross gcc defaults to PIC, so `la sym+off` becomes a GOT load, which can't carry an addend | `-no-pie -fno-pic` | Build fails loudly if a test is missing |
| 5 | Every riscv-test hung under QEMU: illegal instruction at 0x80000000 with `tval = 0x4` | Ubuntu's gcc passes `--build-id`. The `.note.gnu.build-id` section was placed at 0x80000000, pushing `_start` to 0x80000040, and the reset vector jumped into the note (its first word, `namesz = 4`) | `-Wl,--build-id=none` (`_start` is back at 0x80000000) | `ref-riscv-tests.sh` in CI |
| 6 | The whole rv64ua suite was silently skipped | Upstream compiles rv64ua with `-march=rv64g_zacas_zabha`, and binutils 2.42 rejects `zacas`. The Makefile's compiler-support probe then drops the suite | The script builds the 19 classic A tests itself with `-march=rv64g` (same commands as the upstream template) and excludes `amocas_*` (Zacas, outside RV64GC) | The missing-test check covers rv64ua |
| 7 | `hello.S` used a GOT load for `la a1, msg` | clang's linux-gnu target also defaults to PIC | `lla` (pure PC-relative) keeps the first program minimal | Worked example in §5 |
| 8 | `rv64mi-p-instret_overflow` fails under QEMU (test 2), also with `-icount` | QEMU 8.2 doesn't give a `minstret` CSR write precedence over the writing instruction's own retirement increment | Not ours: added to `qemu-known-failures.txt` as XFAIL. **Bridge-V must pass it in Phase 7.** | XPASS detection |

## 9. Deviations from the plan / spec
- **Extra tools beyond the roadmap:** `tools/build-riscv-tests.sh` (the roadmap mentioned it only inline), `tools/ref-check.sh` and `tools/ref-riscv-tests.sh`. CLAUDE.md §6 has been updated.
- **riscv-tests validation:** the roadmap suggested `spike` or `qemu -M virt`. We used `qemu -M spike` instead, because `virt` has no HTIF (D20).
- **rv64ua built outside the upstream Makefile**, and `amocas_*` excluded (D21).
- **Expected outputs** are generated from the `-O2 rv64gc` build, and all four variants are checked against them.

## 10. Known limitations and technical debt
- **LICENSE not chosen.** This is an owner decision (ROADMAP §18). The other §18 questions are also still open.
- **`-nc` builds still contain compressed instructions from libc.** A truly C-free libc would need a rebuilt glibc or musl, which is not planned.
- **The container is ephemeral.** `tools/setup.sh` has to be re-run in each new session. Wiring it into the environment's setup script, or a SessionStart hook, would automate that.
- **The riscv-tests CI cache** is keyed on the submodule commit and the build-script hash. Changing `ref-riscv-tests.sh` alone doesn't invalidate it, and doesn't need to.

## 11. How to reproduce
```
git clone --recurse-submodules https://github.com/tan8696/Bridge-V && cd Bridge-V
git checkout claude/compassionate-babbage-ul3prl
./tools/setup.sh                      # apt packages (sudo if not root)
cargo fmt --check && cargo clippy --all-targets -- -D warnings
./tools/build-guests.sh && ./tools/ref-check.sh
BRIDGEV_REQUIRE_GUESTS=1 cargo test
./tools/build-riscv-tests.sh && ./tools/ref-riscv-tests.sh
```

## 12. Next steps
Phase 1 (front end + reference interpreter), starting with **P1.1 core types** and **P1.2 ELF loader**:
- The ELF loader can be tested immediately against the 26 guest ELFs and 244 riscv-tests.
- `hello.elf` (§5) is the first end-to-end target: `bridgev run guest/build/hello.elf` must print `hello` and exit 0.
- Phase 1's acceptance suite (all `rv64u{i,m,a,f,d,c}-p-*`, 110 tests) and the 24 C variants are already built and validated.
