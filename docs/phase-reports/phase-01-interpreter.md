# Phase 01 report: Front end and reference interpreter

| Field | Value |
|---|---|
| Phase | 01: Front end and reference interpreter (see `docs/ROADMAP.md`) |
| Status | **Complete** (deviations noted in §9) |
| Dates | 2026-09-26 → 2026-09-26 |
| Branch / commits | `claude/compassionate-babbage-ul3prl`: `79f0d8b` (decoder), `1f5a2ca` + `31eb791` (ELF), `6484663` (interpreter core), `034dac0` (FP), `26bb8ce` (user mode), plus the commit containing this report |
| CI | Run #8 on `26bb8ce`: **success**, both jobs, <https://github.com/tan8696/Bridge-V/actions/runs/36243149882> |
| Sessions used | 1 |

## 1. Summary
Bridge-V can now **run RISC-V programs**. This phase built the whole front end and a reference interpreter:
- an ELF loader;
- a decoder for every RV64GC instruction, including compressed ones;
- a disassembler that matches LLVM's output;
- a CPU model with machine- and supervisor-mode CSRs and traps;
- floating point through the same SoftFloat library the official RISC-V simulator (Spike) uses;
- Linux user-mode emulation with about 45 system calls.

**Headline results, all measured:**
- **All 110** official riscv-tests of the Phase 1 suites pass (`rv64u{i,m,a,f,d,c}-p-*`).
- **All 30** guest programs produce byte-identical output and exit status to `qemu-riscv64`. That includes static glibc programs with `printf("%f")`, heap and mmap, `qsort`, `setjmp`/`longjmp` and `stat`.
- **CoreMark** runs correctly and validates with the same CRCs as QEMU.
- The interpreter's baseline speed is **127.4 MIPS** on a tight loop and **175.9 MIPS** on CoreMark. These are the numbers the JIT must beat in Phase 2.

## 2. Planned vs delivered
| Task | Status | Notes |
|---|---|---|
| P1.1 Core types | done (deviation) | `GuestVirt`/`GuestPhys`, `Exception`, privilege constants, `Stop` (exit reasons). Register fields are plain `u8`, not newtypes (§9). |
| P1.2 ELF64 loader | done | 3 unit + 3 integration tests; 270 real ELFs parse; proptest fuzzing |
| P1.3 Instruction model | done | `Inst`: 26 variants covering RV64IMAFD + Zicsr + Zifencei + privileged |
| P1.4 32-bit decoder | done | 1494 llvm-mc golden vectors + an illegal-encoding unit test |
| P1.5 RVC expansion | done | 624 compressed vectors (36/37 RVC instructions; `c.nop` unit-tested) + 10 reserved encodings |
| P1.6 Disassembler | done | Matches llvm-mc `-M no-aliases` exactly for all 1494 vectors; `bridgev disasm` |
| P1.7 CPU state v1 | done | `#[repr(C, align(64))]` with 14 compile-time `offset_of!` asserts |
| P1.8 Direct memory v1 | done | 2³⁸-byte reservation + 4 GiB guards, two-level permission table, 4 unit tests |
| P1.9 Interpreter core | done | Pre-decoded block cache, 4 named semantic-gotcha test groups |
| P1.10 A extension | done | LR/SC reservation, all AMOs W/D, alignment exceptions |
| P1.11 F/D via SoftFloat | done | SoftFloat 3e RISC-V specialization via `build.rs` |
| P1.12 CSRs + minimal M-mode | done (exceeded) | Full M-mode and most of S-mode, WARL legalization, delegation |
| P1.13 riscv-tests runner | done | `bridgev run --mode bare` + `tests/riscv_tests.rs` |
| P1.14 Linux loader | done | Stack/auxv test parses the stack back and checks every pointer |
| P1.15 Syscalls | done | ~45 syscalls; `stat` conversion checked by a dedicated guest program |
| P1.16 CLI, trace, stats | done | `--stats`, `--trace insn`, `--strace`, `--max-insns` |

## 3. What was built

### 3.1 Decoder, RVC and disassembler (`src/isa/`)
- **`Inst`** (`isa/inst.rs`) is a compact enum of 16 bytes per instruction:
  - Immediates are stored fully sign-extended.
  - ALU operations share one `AluOp` between register and immediate forms, so the interpreter, and later the JIT, have a single semantic function per operation.
  - `Decoded { inst, len, raw }` keeps the length (2 or 4, for link addresses) and the raw bits (for `mtval` on illegal instructions).
- **`decode.rs`** covers every major opcode of CLAUDE.md §7.3.
  - Reserved encodings are rejected: wrong funct7/funct3, `slliw` with bit 25 set, `ecall` with rd≠0, `lr` with rs2≠0, and the unsupported H/Q FP formats.
  - All of them become `Inst::Illegal(raw)`. The decoder never panics.
- **`rvc.rs`** expands all RV64C encodings to their 32-bit `Inst`.
  - HINT encodings (`c.li x0, …`, `c.slli x0, …`) become writes to `x0`, which are no-ops.
  - Reserved encodings become illegal: `nzimm = 0` forms, `c.addiw x0`, `c.jr x0`, `c.lwsp x0`, the funct3 = 100 slot, and the all-zero halfword.
- **`disasm.rs`** reproduces LLVM's canonical syntax exactly: ABI register names, CSR names (decimal for unnamed CSRs), rounding-mode suffixes and fence sets.
- **Golden vectors.** `tools/gen-decoder-vectors.py` generates about 1500 assembly lines. They rotate through all 32 registers in every field and use edge immediates. They also include one shape per compressible format, which makes LLVM emit the compressed forms. Each line is assembled by `llvm-mc` both without and with the C extension, and `tests/data/rv64_vectors.txt` (committed) records:
  - the 32-bit encoding
  - the compressed encoding, if any
  - LLVM's canonical text

### 3.2 ELF loader (`src/elf.rs`)
- Every read is bounds-checked (`slice`, `u16_at`, …). It validates:
  - magic, class, endianness, version, machine (243), type
  - program-header bounds, `p_filesz ≤ p_memsz`, segment contents inside the file
  - no overlapping `PT_LOAD`s
- It extracts `PT_INTERP` and `.symtab` (for `tohost`/`fromhost`), and computes `AT_PHDR`.

### 3.3 CPU state, CSRs and traps (`src/cpu/`)
- **`state.rs`:** the JIT-visible layout of CLAUDE.md §8.1, pinned by compile-time asserts. `x` is at 0x000, `pc` at 0x100, …, `f` at 0x140, `fflags`/`frm` at 0x240/0x241. The Rust-only CSR block comes last.
- **`csr.rs`:** CSR semantics:
  - privilege checks from `csr[9:8]`; read-only CSRs from `csr[11:10] = 3`;
  - `mcounteren`/`scounteren` gating of `cycle`/`time`/`instret`;
  - `mstatus.FS` gating of `fflags`/`frm`/`fcsr`;
  - WARL legalization: `MPP = 2` becomes U; reserved `xtvec` modes become direct; `xepc[0] = 0`; `medeleg`/`mideleg` masks;
  - `sstatus`, `sie` and `sip` implemented as views of the M-mode registers.

  **`minstret`/`mcycle` writes take precedence over the writing instruction's own retirement**, implemented as an offset from `icount + 1`. Thanks to this, `rv64mi-p-instret_overflow` passes, even though QEMU 8.2 fails it (Phase 0 report, §8 #8).
- **`trap.rs`:**
  - Trap entry with `medeleg`/`mideleg` delegation to S or M, vectored interrupt entry, and MRET/SRET status-stack semantics (CLAUDE.md §15).
  - A trap also clears the LR reservation.

### 3.4 Guest memory (`src/mem/direct.rs`)
- The whole SV39 user half (2³⁸ bytes) is reserved at startup as `PROT_NONE` + `MAP_NORESERVE`, with 4 GiB guards on each side. Guest address `g` always lives at host `base + g`. That is the layout the Phase 2 JIT will use (`[rbx + g]`).
- Guest permissions live in a lazily allocated two-level table: 16 384 chunks × 4096 pages, one byte per page (R/W/X plus a MAPPED marker).
  - Every interpreter access is checked against it, including accesses that cross a page. **A bad guest access becomes a guest exception; it can never fault the host.**
  - Host pages are always RW; host-level protection that mirrors the guest's is introduced with the JIT (D23).

### 3.5 Interpreter (`src/interp/mod.rs`)
- **Blocks** are decoded once and cached by PC in an `FxHashMap`. A block ends at control flow, system or CSR instructions, a page boundary, or 64 instructions. A fetch fault while decoding is recorded and raised only when execution actually reaches that address.
- **`alu`/`aluw`** hold the integer semantics, including every CLAUDE.md §7.5 gotcha:
  - division never traps (`x/0 = −1`, `MIN/−1 = MIN`, remainders `x` / 0);
  - W-ops sign-extend;
  - shift amounts are masked to 6 or 5 bits;
  - MULHSU is computed with i128.
- **JALR** reads `rs1` before writing `rd`. Loads to `x0` still access memory.
- **AMOs:** LR records a reservation (address and value). SC succeeds only on a matching reservation and always clears it. The other AMOs read and write in one step and require natural alignment.
- **Environments:** in bare mode, exceptions and ECALL trap into guest M/S code. In user mode, they return `Stop::Ecall`/`Stop::Fault` to the Linux layer.

### 3.6 Floating point (`src/cpu/fp.rs`, `build.rs`, `src/cpu/softfloat_shim.c`)
- **Build.** `build.rs` compiles Berkeley SoftFloat 3e with `SPECIALIZE_TYPE=RISCV`. It takes the object lists straight from SoftFloat's own Linux-x86_64-GCC Makefile, so the configuration matches upstream exactly.
  - The state is compiled with `THREAD_LOCAL=_Thread_local`, because `cargo test` runs tests on several threads.
  - A 2-function C shim exposes the thread-local rounding mode and flags, which Rust cannot reference directly.
- **Why no translation code is needed.** SoftFloat's rounding-mode numbers and exception-flag bits are identical to RISC-V's `frm` and `fflags`. The RISC-V specialization already provides canonical NaNs, tininess detected after rounding, and RISC-V's saturating float→int conversions.
- **What `fp.rs` adds:**
  - NaN-boxing: an improperly boxed single reads as the canonical NaN;
  - the dynamic rounding mode, with reserved values → illegal-instruction;
  - FS gating, plus setting FS to Dirty;
  - FMA variants via exact sign flips;
  - FMIN/FMAX: NaN handling, sNaN → NV, −0 < +0;
  - FCLASS;
  - sign injection, and raw FMV moves;
  - sign-extension of FCVT.W/WU results.

### 3.7 Linux user mode (`src/user/`)
- **Loader** (`loader.rs`):
  - Maps `PT_LOAD` segments. Pages shared by two segments get the union of their permissions, and runs of equal pages are mapped together.
  - Sets up the brk heap and an 8 MiB stack at `0x3F_FFFF_F000`.
  - Builds `argc`, `argv`, `envp` and 16 auxv entries: PHDR, PHENT, PHNUM, PAGESZ, ENTRY, HWCAP = IMAFDC = `0x112d`, RANDOM (16 host-random bytes), EXECFN, UID/GID, …
- **Syscalls** (`syscall.rs`):
  - About 45 calls. Where layouts are identical, they pass through to the host with translated pointers. That covers errno values, `open` flags, `mmap` flags and prot bits, `timespec`, `iovec`, `statx`, `rlimit` and `utsname`.
  - **`struct stat` is converted** (riscv64 128 bytes → x86-64 144 bytes).
  - `mmap`/`munmap`/`mprotect`/`brk` are implemented on the direct backend (top-down placement below `0x3F_0000_0000`).
  - `/proc/self/exe` resolves to the guest executable, and `uname` reports `machine = riscv64`.
  - `riscv_flush_icache` flushes the block cache. Unknown syscalls are logged once and return `-ENOSYS`.
- **Run loop** (`mod.rs`): a fatal guest exception exits with `128 + signal` (SIGSEGV, SIGILL, SIGBUS or SIGTRAP), as a shell would report it.

### 3.8 CLI (`src/main.rs`)
```
bridgev run [--mode user|bare] [--engine interp] [--stats] [--trace insn] [--strace] [--max-insns N] <elf> [args…]
bridgev disasm <elf>
```

## 4. Design decisions made (appended to CLAUDE.md §3)
- **D22, decoder oracle = llvm-mc.**
  - Golden vectors are generated by `llvm-mc -M no-aliases` and committed, so the tests don't need LLVM installed.
  - The disassembler matches LLVM's text exactly, so one test verifies decode *and* disassembly.
  - The compressed encodings LLVM chooses cross-check the RVC expansion.
  - Alternative considered: hand-written expected values. Rejected as error-prone, and not independent.
- **D23, software-enforced guest permissions (Phase 1).**
  - Host pages stay RW, and guest R/W/X live in a two-level table that is checked on every interpreter access.
  - The interpreter is therefore memory-safe even for hostile guests.
  - Host-level `mprotect` mirroring the guest's permissions arrives with the JIT's unchecked `[rbx + g]` accesses in Phase 2, and SMC write-protection in Phase 8.
- **D24, Sv39 gated off until Phase 7.** `satp` accepts only Bare mode (`SV39_SUPPORTED = false` in `csr.rs`). A guest can't enable translation that isn't implemented yet and have it silently ignored.

## 5. How it works: worked example
`bridgev run --trace insn guest/build/hello.elf`, as actually captured:
```
00000000000111ae: 00004505  addi a0, zero, 1      ← 16-bit 0x4505 = c.li a0, 1, expanded by rvc.rs
00000000000111b0: fffff597  auipc a1, 1048575     ← a1 = pc + sext(0xfffff << 12) = 0x101b0
00000000000111b4: ff858593  addi a1, a1, -8       ← a1 = 0x101a8 = address of "hello\n"
00000000000111b8: 00004619  addi a2, zero, 6      ← c.li a2, 6
00000000000111ba: 04000893  addi a7, zero, 64     ← syscall number 64 = write
00000000000111be: 00000073  ecall                 ← Stop::Ecall → Syscalls::dispatch
hello                                             ← host write(1, base + 0x101a8, 6)
00000000000111c2: 00004501  addi a0, zero, 0
00000000000111c4: 05d00893  addi a7, zero, 93     ← exit
00000000000111c8: 00000073  ecall                 ← SysOut::Exit(0) → process exit status 0
```
The path of the `write`, step by step:
1. `build_block(0x111ae)` fetches halfwords through `DirectMem::fetch16`, which checks the X permission.
2. `decode_parts` sees that `0x4505`'s low bits aren't `0b11`, so it calls `rvc::expand` (16-bit). It fetches the second halfword only for 32-bit instructions.
3. The block ends at the first `ecall` (`Inst::ends_block`).
4. `exec` returns `Flow::Ecall`, and the interpreter returns `Stop::Ecall` with `pc` at the ecall.
5. The syscall layer reads a7 = 64, and translates the guest buffer `a1 = 0x101a8` (6 bytes, R permission checked) into a host pointer.
6. It calls the host `write`, puts the result (6) in a0, and advances pc by 4.

## 6. Tests and verification
**Test inventory** (32 test functions; several each check hundreds of cases):
| Suite | What it checks |
|---|---|
| `tests/decoder_vectors.rs` (2) | 1494 disassemblies identical to llvm-mc; 624 compressed expansions |
| `src/isa/*` unit tests (5) | immediates, lengths, 17 illegal 32-bit encodings, 10 reserved RVC encodings, HINTs |
| `src/elf.rs` (3) + `tests/elf.rs` (3) | synthetic ELF, 9 malformed variants + overlap, 270 real ELFs, proptest (random/corrupted bytes never panic) |
| `src/mem/direct.rs` (4) | map/access/unmap, page-crossing fault address, out-of-space, mapped-without-permission |
| `src/interp` (4) | division edge cases, MULH variants, W-op sign extension and shift masking, SLT/SLTIU |
| `src/cpu/fp.rs` (3) | NaN-boxing, FCLASS bits, SoftFloat linkage + rounding + reserved rm |
| `tests/riscv_tests.rs` (1) | **110** official tests: rv64ui 54, um 13, ua 19, uc 1, uf 11, ud 12 |
| `tests/user_programs.rs` (2) | **30** guest programs vs qemu (byte-exact stdout + exit code); initial stack layout |
| `tests/cli.rs` (5) | version, stubs, missing-file error, `disasm` output, guest helper |

**Commands and results** (2026-09-26):
```
$ BRIDGEV_REQUIRE_GUESTS=1 cargo test          → 32 passed; 0 failed
$ cargo test --release --test riscv_tests -- --nocapture
110 riscv-tests run, 0 failed
$ cargo test --test user_programs -- --nocapture
26 guest programs run, 0 failed      (before stat.c was added; 30 after: ref-check 30/30)
$ cargo clippy --all-targets -- -D warnings    → exit 0
```

**Informational** (Phase 7 scope, not required now): 19 of the 24 `rv64mi`/`rv64si` `-p` tests already pass. The 5 failures need things not built yet:
- `mi-breakpoint`: debug triggers
- `mi-csr`: case 12
- `mi-illegal`: interrupts/WFI
- `si-dirty`, `si-icache-alias`: Sv39

**Acceptance criteria:**
| Criterion | Result | Evidence |
|---|---|---|
| All `rv64u{i,m,a,f,d,c}-p-*` pass under the interpreter | ✅ | 110/110 |
| All C guest programs (O0/O2, rv64gc/rv64imafd) match qemu | ✅ | 30/30 byte-exact, incl. asm programs |
| Decoder vector tests pass (count recorded) | ✅ | 1494 vectors / 624 compressed |
| First interpreter MIPS on `loop.elf` and CoreMark | ✅ | §7 |

## 7. Performance (baseline, not a Phase 5 measurement)
Host: Intel Xeon @ 2.80 GHz, 4 vCPU, a shared cloud VM (noisy). Release build of `26bb8ce`, a single run each. This is not the Phase 5 protocol (no `taskset`, no median of 5), so read these as indicative.

| Workload | Guest instructions | Time | Guest MIPS |
|---|---|---|---|
| `loop.elf` (10⁸ × `addi`+`bne`), release | 200 000 007 | 1.570 s | **127.4** |
| `loop.elf`, test profile (opt-level 1) | 200 000 007 | 1.924 s | 104.0 |
| CoreMark (11 000 iterations, auto-calibrated) | 4 345 420 831 | 24.701 s wall | **175.9** |

| CoreMark score (iterations/s) | Value |
|---|---|
| Bridge-V interpreter | **489.7** (CRCs `0xe714/0x1fd7/0x8e3a/0x33ff`, "Correct operation validated") |
| `qemu-riscv64` 8.2.2 (a JIT) | 8 519.2, identical CRCs |

QEMU is currently about 17× faster. That gap is exactly what the JIT phases (2–4) are for.

CoreMark build command, a Phase 1 ad-hoc build (the official build comes in P5.1):
```
riscv64-linux-gnu-gcc -O2 -static -march=rv64gc -Ithird_party/coremark -Ithird_party/coremark/posix \
  -DPERFORMANCE_RUN=1 '-DFLAGS_STR="-O2 -static -march=rv64gc"' third_party/coremark/core_*.c \
  third_party/coremark/posix/core_portme.c -o coremark.elf
```
The iteration count was auto-calibrated by CoreMark: `get_seed_32(4)` in `core_main.c:137`, so a `-DITERATIONS` define isn't consumed. P5.1 will pass iterations explicitly.

## 8. Bugs found and fixed
| # | Symptom | Root cause | Fix | Guard |
|---|---|---|---|---|
| 1 | CI run #4 failed on clippy after a push | My shell chain piped clippy into `head`, masking its exit status, so the commit went through | Fixed in `31eb791`; clippy's `$?` is now checked explicitly before every commit | CI clippy step |
| 2 | CI runs #6 and #7 failed | The build-test job didn't build riscv-tests, but `tests/riscv_tests.rs` requires them when `BRIDGEV_REQUIRE_GUESTS=1` | CI step "Build riscv-tests" (run #8 green) | CI |
| 3 | 1 of 624 compressed vectors mismatched | Not a decoder bug: LLVM compresses `addiw rd, zero, imm` to `c.li rd, imm`, which is equivalent | Normalization in the test (`canon`) | The vector test |
| 4 | Memory test failure | Wrong expectation in my test (little-endian byte order across a page) | Fixed the expected byte (`0xad`) | — |
| 5 | `stat.c` failed to build, **and** `ref-check.sh --update` then recorded "expected output" for a nonexistent ELF | Missing `<stdlib.h>`; the script didn't check that the ELF exists | Include added; `ref-check.sh` now fails on a missing ELF | `ref-check.sh` |
| 6 | Stack-layout test fault | The test helper read a fixed 64-byte window past the end of the stack mapping | Read strings byte by byte | — |
| 7 | Compile errors while developing | A borrow conflict in `csr_write`, and an `fs()` name clash between the FS field and FP register reads | Restructured / renamed (`freg_s`) | Compiler |
| 8 | Found during self-review before the first test run: `DirectMem::map` didn't set the MAPPED marker, so pages mapped with no permissions would look unmapped | Oversight | Marker set in `map`; `mapped_without_permissions` test added | Unit test |

## 9. Deviations from the plan / spec
- **Register newtypes (P1.1).** `Inst` uses plain `u8` register indices, which are always < 32 because they come from 5-bit fields. `GuestVirt` is used at mapping and slice APIs, where host pointers and guest addresses meet (CLAUDE.md §25). Per-access `load`/`store` take `u64`, because they never hand out host pointers.
- **Exceeded P1.12:** most of S-mode is already implemented (CSR views, delegation, SRET, TVM/TW/TSR checks).
- **New files not in the original §6 layout:** `src/system/bare.rs` (bare-metal test harness), `src/cpu/softfloat_shim.c`, `tools/gen-decoder-vectors.py`, `tests/{decoder_vectors,elf,riscv_tests,user_programs}.rs`, `guest/c/stat.c`. CLAUDE.md §6 has been updated.
- **P1.14 "`env.c`":** `argv` is covered by `strings.c` (it runs with arguments) and `envp` by the stack-layout test. No separate `env.c` was added.
- **P1.7 "runtime layout print":** the compile-time `offset_of!` asserts make it redundant, so it wasn't added.

## 10. Known limitations and technical debt
- **No interrupts:** WFI is a no-op, and there are no timers or devices (Phase 7/9).
- **`satp` Sv39 writes are ignored** (D24, Phase 7).
- **Signals are accepted but never delivered.** `rt_sigaction`/`rt_sigprocmask` succeed and handlers never run. A fatal fault terminates the process (Phase 10).
- **Single-threaded only:** `clone` is unsupported, and `futex` WAIT returns `-EAGAIN`.
- **File `mmap` copies the contents** (MAP_PRIVATE semantics), so `MAP_SHARED` writes aren't written back.
- **Dynamically linked and PIE executables are rejected** with a clear message (Phase 10).
- **`--max-insns` is checked at block boundaries**, so it can overshoot by up to 63 instructions.
- **The block cache doesn't detect code modification without FENCE.I.** The RISC-V spec allows this. Eager SMC handling comes in Phase 8.
- **Interpreter speed is untuned** (a `match` per instruction, `Rc` per block). It's the golden model, and the speed work belongs to the JIT.

## 11. How to reproduce
```
git checkout claude/compassionate-babbage-ul3prl
./tools/setup.sh
./tools/build-guests.sh && ./tools/build-riscv-tests.sh
cargo fmt --check && cargo clippy --all-targets -- -D warnings
BRIDGEV_REQUIRE_GUESTS=1 cargo test
cargo build --release
./target/release/bridgev run --stats guest/build/loop.elf        # exit 225, ~127 MIPS
./target/release/bridgev run --mode bare guest/build/riscv-tests/rv64ud-p-fadd   # PASS
./target/release/bridgev run --trace insn guest/build/hello.elf  # the §5 trace
python3 tools/gen-decoder-vectors.py                             # regenerates the golden vectors
```

## 12. Next steps
Phase 2 (naive JIT) builds on:
- the fixed `CpuState` offsets;
- the direct backend's `base()`, which becomes RBX;
- `interp::alu`/`aluw`, the reference semantics;
- `helper_interp_one`, which the JIT will use as its fallback (D14) by reusing `interp::exec`.

Start with **P2.1 (the x86-64 emitter and its iced-x86 golden tests)**, which is independent of everything else. Then P2.3 "hello JIT" (dual-mapped code memory) and P2.4 (the trampolines).
