# Phase 07 report: Privileged architecture and SoftMMU (Sv39 + inline TLB)

| Field | Value |
|---|---|
| Phase | 7: Privileged architecture and SoftMMU (see `docs/ROADMAP.md`) |
| Status | Complete |
| Dates | 2026-09-26 → 2026-09-27 |
| Branch / final commit | `claude/compassionate-babbage-ul3prl` @ the commit that adds this report (code: `f190736`, `2da741e`, `6c95332`) |
| Sessions used | 1 |

## 1. Summary

Phase 7 adds virtual memory. Guest accesses can now be translated through an Sv39 page walker and a software TLB. That covers bare/system mode always, and Linux user mode with `--mem=softmmu`. Both engines use it:
- **Interpreter:** calls the MMU through `mem::mmu`.
- **JIT:** probes the TLB inline (9 instructions to the access). A miss goes to a cold per-access slow path. Faults there are made precise with the Phase 4 state maps.

The privileged CSR set, interrupt delivery, HTIF console output and a physical memory bus (RAM + MMIO) came with it. Most of M/S mode already existed from Phase 1.

**Results:**
- **All 244 riscv-tests pass:** `rv64u*-p`, `rv64u*-v` (virtual memory, demand paging), `rv64mi-p` and `rv64si-p`, under the interpreter, the JIT at every regalloc level, and lockstep.
- **Fuzzers:** a new fast-path vs slow-path fuzzer is clean at 1,000,000 cases. It found one real bug on the way.
- **TLB microbenchmark:** a hit costs 4.1 ns of load-to-use latency (vs 1.0 ns for a raw host load), and a miss with an Sv39 walk costs 28 ns.
- **CoreMark:** under `--mem=softmmu` it keeps 54% of direct-mode speed, at 2.7 billion guest instructions/s (§7).

## 2. Planned vs delivered

| Task ID | Task | Status | Notes |
|---|---|---|---|
| P7.1 | Complete CSR file | done | Table in §3.6. The only additions this phase: satp accepts Sv39, and the debug-trigger CSRs read as "no triggers" (rv64mi-p-breakpoint). WARL legalization and TVM/TW/TSR trapping already existed (rv64mi-p-illegal now passes because satp works). |
| P7.2 | Traps and interrupts | done | `CpuState::pending_interrupt` (priority MEI > MSI > MTI > SEI > SSI > STI, M/S enable rules, delegation). The dispatcher delivers interrupts between blocks in both engines. CSR instructions end blocks. WFI stays a legal no-op until Phase 9 adds a timer. |
| P7.3 | Physical memory bus | done | `mem/phys.rs` (`Mmio` trait) and `DirectMem::devices`. RAM is `DirectMem` mapped at its physical address; anything else is an access fault. |
| P7.4 | Sv39 walker | done | `mem/mmu.rs::walk`, per §14.3. The walker sets A/D itself. 9 unit tests (§6). Sv48 is left for Phase 10. |
| P7.5 | TLB | done | `mem/tlb.rs`: 256 direct-mapped 32-byte entries × 4 MMU indices at `CpuState` 0x10580, flag bits, flush rules (D48). |
| P7.6 | Inline fast path | done | `lower_ir.rs::tlb_probe` + `emit_soft_slow` + `helper_mmu_access`. The naive back end uses the interpreter helper for memory ops. |
| P7.7 | System-mode test runner | done | `--mode bare` runs everything with softmmu, plus HTIF `write` syscalls (printf from bare-metal programs). All `-v-` tests pass. |
| P7.8 | SoftMMU in user mode | done | `--mem=softmmu`: identity translation, guest mmap permissions as physical permissions, TLB flushed after brk/mmap/munmap/mremap/mprotect. It is in the benchmark matrix (`softmmu` column). |
| P7.9 | TLB microbenchmark | done | `guest/bench/tlb/tlbbench.c` (bare metal, Sv39) + `tlbuser.c` (user mode, direct vs softmmu), `tools/tlb-bench.py`. |

## 3. What was built

### 3.1 Two memory modes, one access API (`src/mem/mmu.rs`)
`CpuState.softmmu` (offset 0x116) selects the mode:
- 0: direct user-mode memory, as before.
- 1: system/bare softmmu.
- 2: user-mode softmmu, whose translation is flat. (Value 2 was added after the first measurements, §8.)

The interpreter's loads, stores, AMOs, FP loads/stores and instruction fetch all call `mmu::{load, store, load_for_amo, fetch16}`. Each starts with `if cpu.softmmu == 0 { direct }`, so direct mode pays one predictable branch.

With softmmu, an access works like this:
1. Pick the MMU index: data uses `data_idx` (MPRV → MPP, SUM → the S+SUM index), fetch uses `fetch_idx`.
2. Probe the TLB. On a miss, `fill` walks (Sv39, or identity for Bare/M-mode) and checks the physical page:
   - RAM uses `DirectMem`'s per-page permissions (RWX in system mode, the guest's mmap permissions in user mode).
   - Otherwise the address must belong to an MMIO device.
3. The fill folds permissions, SUM/MXR and the dirty bit into the three tags. A write tag is only valid once D = 1.
4. Access RAM through `DirectMem` or the device. Misaligned accesses that cross a page are split: a load byte by byte, a store only after both pages have translated, so a faulting store writes nothing.

A failed walk is a page fault (12/13/15). A failed physical check, or a PTE outside RAM, is an access fault (1/5/7).

### 3.2 The walker (`walk`)
It follows priv spec §12.3.2 step by step:
- **Address:** a non-canonical VA (bits 63:39 ≠ bit 38) is a page fault.
- **Reading PTEs:** each PTE read is a physical access, so a PTE outside RAM is an access fault.
- **PTE checks:** V = 0, W without R, or nonzero reserved/PBMT/N bits → page fault. A non-leaf PTE with A, D or U set is reserved → page fault (as Spike does). A pointer at level 0 → page fault. A misaligned superpage → page fault.
- **Permissions:** U pages are for U-mode, and for S-mode data accesses with SUM, never S-mode fetch. Loads need R, or X with MXR. Stores need W, fetches need X.
- **A/D:** A (and D for stores) are set with a physical store, as Svadu allows. The TLB's write tag stays invalid until D is set, so the first store to a clean page walks again.
- **Superpages:** they fill the TLB at 4 KiB granularity; the low VPN bits come from the VA.

### 3.3 The TLB (`src/mem/tlb.rs`, D48)
- **Layout:** `TlbEntry { addr_read, addr_write, addr_code, addend }`, 256 per index, 4 indices (U, S, S+SUM, M/Bare), 32 KiB at 0x10580.
- **Tags:** tag = virtual page | flags. `TLB_MMIO` (bit 3) forces device pages to the slow path; `TLB_CODE` (bit 4) is reserved for Phase 8. Invalid = all ones, whose low bits never match a compared value.
- **Addend:** host − virtual, the same arithmetic for RAM and MMIO. For MMIO, `va + addend − mem_base` is the physical address.
- **Why four indices:** QEMU-style, SUM gets its own index, because Linux toggles SUM around every user copy and a flush each time would be expensive. MPRV/MPP just select an index.
- **Flushes:** only satp writes, SFENCE.VMA and MXR changes flush all (`flush_all`). Every flush bumps `cpu.mmu_gen`.

### 3.4 Interpreter caches
Decoded blocks are keyed by (pc, fetch index) and dropped when `mmu_gen` changes. A virtual pc can map to different code after a satp/SFENCE.VMA, and a privilege change must re-check the fetch permission. A block whose first fetch faulted is not cached.

Block formation is stricter in softmmu mode: a 32-bit instruction straddling a page boundary is only allowed as a block's first instruction. In both modes, a block never continues onto another page after such an instruction. Before, direct mode kept going on the next page.

### 3.5 JIT (`src/jit/dispatch.rs`, `src/backend/x86/lower_ir.rs`)
- **TB key** (`cache.rs::TbKey`): pc, FP variant, flags = `0x80 | [0x40 flat] | fetch_idx | data_idx << 2`, and the physical page. The dispatcher (`Jit::select`) translates the pc first (`mmu::fetch_page`). A fetch fault is delivered directly; a page-straddling first instruction is interpreted (`interpret_one`) instead of translated, so a TB never depends on two pages.
- **Chaining:** only between TBs with equal flags, never out of a TB that ends in a CSR instruction (the CSR may change the flags its successor needs), and in system mode only within one virtual page (§13.3). User-mode softmmu (flat) chains freely.
- **Jump cache:** reset whenever (flags, `mmu_gen`) changes. That replaces a per-hit `addr_code` check.
- **Inline probe** (`tlb_probe`): see §5. The data MMU index is part of the TB key, so the TLB offset is a constant.
- **Cold slow path** (`emit_soft_slow`):
  1. Save RAX, RCX, RDX, RSI, RDI, R8 and R9 (budget) to `cpu.fault_regs`.
  2. Build the arguments from the saved copies, and refund the not-yet-retired budget (D30) so the helper can keep `icount` exact.
  3. `call [helper_mmu_access]`, then restore.
  4. If `exit_reason` is still 0, move the value into the destination and continue.
  5. Otherwise store R12–R15 and `fault_rip` (= the access instruction, via a new `lea r, [rip+label]` emitter form) and exit with reason 8 `MMU_FAULT`. The dispatcher applies that site's Phase 4 state map (D37): the dirty guest registers are written home from `fault_regs`, then pc and budget are fixed up. The trap is precise.
- **Naive back end:** loads/stores go through `helper_interp_one` (D14) with softmmu.

### 3.6 CSR coverage (`src/cpu/csr.rs`)

| CSR | Access | Behaviour |
|---|---|---|
| fflags, frm, fcsr | U | FS-gated (illegal with FS = Off); writes set FS = Dirty |
| cycle, time, instret, hpmcounter3–31 | U (counter-enable gated) | icount, 10 MHz host clock (icount/10 with `--deterministic`), icount; hpm = 0 |
| sstatus, sie, sip | S | views of mstatus/mie/mip through the S masks; only SSIP writable in sip |
| stvec, sscratch, sepc, scause, stval, scounteren, senvcfg | S | stvec WARL (direct/vectored), senvcfg reads 0 |
| satp | S (TVM-gated) | Bare and Sv39 accepted; other modes: the write has no effect; any write flushes the TLB |
| mstatus | M | SIE MIE SPIE MPIE SPP MPP FS MPRV SUM MXR TVM TW TSR writable; MPP = 2 legalizes to U; SD computed; UXL/SXL = 64; MXR changes flush the TLB |
| misa | M | RV64 ACDFIMSU, read-only (WARL) |
| medeleg, mideleg, mie, mip | M | delegable causes 0–9, 12, 13, 15; S interrupts only in mideleg; mip S bits writable |
| mtvec, mscratch, mepc, mcause, mtval, mcounteren, menvcfg, mcountinhibit | M | mtvec WARL; mepc/sepc bit 0 clear; menvcfg reads 0 |
| mcycle, minstret, mhpmcounter3–31, mhpmevent3–31 | M | writable (offset against icount); hpm read 0 |
| pmpcfg0–14 (even), pmpaddr0–63 | M | stored, not enforced (every access allowed) |
| mvendorid, marchid, mimpid, mconfigptr, mhartid | M, read-only | 0 |
| tselect, tdata1–3 | M | read 0 (no triggers), writes ignored (new) |
| anything else | | illegal instruction |

Privilege checks: csr[9:8] against the current level, read-only CSRs (csr[11:10] = 3) trap on writes, satp traps in S-mode with TVM. SRET traps with TSR, WFI in S with TW, SFENCE.VMA in S with TVM, and all of them in U.

## 4. Design decisions made

- **D48 (new, CLAUDE.md §3).** The softmmu design. It records:
  - the modes
  - the four MMU indices
  - the TLB layout and flush rules
  - A/D set by the walker
  - the TB key and chaining rules
  - jump-cache reset on (flags, `mmu_gen`)
  - straddling instructions interpreted
  - the slow-path protocol and exit reason 8
  - satp accepting Sv39 (superseding D24)
- **Alternatives considered:**
  - Three MMU indices plus a flush on SUM changes (rejected: Linux toggles SUM constantly).
  - A per-site stub that stores the dirty registers itself, as §14.4 planned. Rejected: the D37 state map already knows where every dirty value lives, so saving the caller-saved registers and resolving in Rust is simpler and just as precise.
  - Borrowing the load's destination register for the masked tag, as §14.4 sketched. Rejected: stores have no destination, so the probe recomputes the address from the (intact) base register instead.
  - One-instruction TBs for page-straddling instructions, as §13.2 planned. Rejected: they would need a two-page key; interpreting them is rare and simpler.
- **User-mode softmmu is "flat"** (`softmmu = 2`, `FLAG_FLAT`). It chains across pages like direct mode, because an identity translation only changes with a flush. Measured +37% on CoreMark (§7).
- **Lockstep and the TLB.** The TLB is not architectural, but it decides whether a walk (and an A/D write) happens. Lockstep used to copy the TLB per TB, which cost 216 s on the user-program suite. Now it flushes the TLB before the JIT run only if the reference run filled entries: the JIT then walks at least wherever the interpreter did, and walking a PTE whose A/D bits are already set writes nothing. That test now takes 25 s.

## 5. How it works: worked example

The first TB of the chase kernel in `tlbbench.c` (bare metal, S-mode, Sv39), from `--dump-x86` and `objdump -M intel`:

```
guest 0x80002004:  addi a5, zero, 0 ; lui a4, 0x40000 ; ld a4, 0(a4) ; addi a5, a5, 1 ; bne a1, a5, -4

   0: 49 83 e9 05              sub    r9, 0x5                          ; budget (D46)
   4: 0f 8c ..                 jl     budget_stub
   a: 41 bf 00 00 00 00        mov    r15d, 0x0                        ; a5 (pinned) = 0
  10: b8 00 00 00 40           mov    eax, 0x40000000                  ; a4 = lui (folded)
  15: 4c 8d 18                 lea    r11, [rax]                       ; va = a4 + 0
  18: 4d 89 da                 mov    r10, r11
  1b: 49 c1 ea 07              shr    r10, 0x7                         ; (va >> 12) << 5
  1f: 41 81 e2 e0 1f 00 00     and    r10d, 0x1fe0                     ; entry offset (256 entries)
  26: 49 81 e3 07 f0 ff ff     and    r11, 0xfffffffffffff007          ; page | misalignment bits
  2d: 4e 3b 9c 15 00 25 01 00  cmp    r11, [rbp+r10+0x12500]           ; addr_read, TLB of index S
  35: 0f 85 31 00 00 00        jne    slow (0x6c)
  3b: 4c 8d 18                 lea    r11, [rax]
  3e: 4e 03 9c 15 18 25 01 00  add    r11, [rbp+r10+0x12518]           ; + addend
  46: 49 8b 0b                 mov    rcx, [r11]                       ; the load (fault site)
  49: ...                                                              ; back from the slow path
  ... slow (0x6c):
  6c: 48 89 85 80 04 01 00     mov    [rbp+0x10480], rax               ; fault_regs[RAX] ... R9
  ...
  9d: 48 8b b5 80 04 01 00     mov    rsi, [rbp+0x10480]               ; va from the saved RAX
  a7: ba 08 01 00 00           mov    edx, 0x108                       ; size 8, load, MMU index 1
  ac: 4d 8d 51 03              lea    r10, [r9+0x3]                    ; refund the 3 unretired insns
  b0: 4c 89 95 88 00 00 00     mov    [rbp+0x88], r10                  ; -> cpu.budget
  b7: 48 8d 7d 80              lea    rdi, [rbp-0x80]                  ; CpuState*
  bb: ff 15 ..                 call   [helper_mmu_access]
  c1: 49 89 c3                 mov    r11, rax
  c4: 48 8b 85 80 04 01 00     mov    rax, [rbp+0x10480]               ; restore RAX ... R9
  ...
  f5: 83 bd 90 00 00 00 00     cmp    dword [rbp+0x90], 0x0            ; exit_reason
  fc: 0f 85 08 00 00 00        jne    fault
 102: 4c 89 d9                 mov    rcx, r11                         ; the loaded value
 105: e9 3f ff ff ff           jmp    0x49
 10a: 4c 89 a5 e0 04 01 00     mov    [rbp+0x104e0], r12               ; fault: R12..R15, fault_rip,
 ...                                                                   ; exit with MMU_FAULT
```
`0x12500 = 0x10580 (TLB) + 1 × 0x2000 (index S) − 0x80 (RBP bias)`.

**On a hit:**
- It costs 9 instructions to the loaded value.
- There are two TLB-entry loads, the tag and the addend, which are independent of each other.
- No stores or calls.

**On a miss:**
- The walk reads the root, L1 and L0 PTEs of `root[1] → l1[0] → l0[0]`, maps VA 0x4000_0000 to the physical page of `data[0]`, and fills entry 0 of the S TLB.
- The helper returns the value, and the stub continues at 0x49.

## 6. Tests and verification

| Suite | What | Result |
|---|---|---|
| `tests/riscv_tests.rs` | now **all 244** riscv-tests (asserts the count): rv64ui/um/ua/uc/uf/ud × p/v, rv64mi-p, rv64si-p; under interp, jit, lockstep, no-chain, slice = 1, mprotect W^X with small blocks, every regalloc level, linear unpinned | 8/8 tests pass |
| `mem::mmu::tests` (new, 9) | walker matrix: 4K/2M/1G pages; misaligned 2M and 1G superpages; U/S/SUM/MXR × load/store/fetch; M-mode and Bare identity; MPRV with MPP = U and S; A set by loads, D by stores and the write tag only after D; invalid PTEs (V = 0, W without R, reserved bit 54, PBMT, N, a pointer at level 0, non-leaf with A/D/U); non-canonical VAs; a root table outside RAM (access fault, per access type); MMIO pages (read/write through the device, fetch → access fault) and unbacked physical addresses; page-crossing loads and a page-crossing store that faults on its second page without writing | 9/9 |
| `cpu::trap::tests` (new, 3) | interrupt priority order; M/S enable rules at every privilege × MIE/SIE combination, delegated and not; M-level beating a delegated one; trap entry to S with vectoring, exceptions never vectored, medeleg ignored in M-mode | 3/3 |
| `tests/softmmu.rs` (new) | random load/store/ALU blocks off four base registers over 8 data pages, each randomly RW, RW-clean (D = 0), read-only, user, execute-only, unmapped or MMIO (a logging device whose reads depend on the access history), with offsets biased to page ends; random SUM/MXR; S-mode Sv39; JIT (linear, pinned, none) vs interpreter, cold then warm TLB; compares registers, pc, icount, exit/exception, every data page, the page tables (A/D) and the device log | **1,000,000 cases clean** (450 s) |
| `tests/user_programs.rs` | every guest program under `--mem=softmmu` with interp, jit, lockstep and `--regalloc none`, byte-exact against the QEMU references | pass |
| `tests/cli.rs::bare_metal_htif_printf` (new) | riscv-tests' `qsort` benchmark in bare mode (HTIF printf) under interp, jit, lockstep | pass |
| `tests/emitter_golden.rs::lea_rip_relative_label` (new) | `lea r, [rip+label]` for all 16 registers, forward and backward | pass |

```
$ PROPTEST_CASES=1000000 cargo test --release --test softmmu
test result: ok. 1 passed; 0 failed; ... finished in 450.26s
$ BRIDGEV_REQUIRE_GUESTS=1 cargo test --release
(all 14 test binaries) test result: ok ... 0 failed
```
`cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` (with and without `--features disasm`) are clean.

**Acceptance criteria:**
- ✅ `rv64mi/si-p-*` and `rv64u*-v-*` pass (interp, jit, lockstep): all 244, above.
- ✅ Walker unit tests pass: 9/9.
- ✅ Recorded: the TLB microbenchmark (§7.2) and softmmu vs direct CoreMark (§7.1).

## 7. Performance

Host: Intel Xeon @ 2.10 GHz (nominal), 4 vCPU, pinned to CPU 2, kernel 6.18.44, rustc 1.94.1. Shared cloud VM: expect several percent of noise.

### 7.1 Direct vs softmmu (user mode, `tools/bench.py`)

Two batches of 5 runs after 1 warm-up, median (min–max):
- **Batch 1:** commit `2da741e`, together with interp and qemu.
- **Batch 2:** commit `6c95332`, after the flat-chaining fix of §4, same batch for direct and softmmu. Its JSON says "dirty tree" only because this report was being written; the binary was built from `6c95332`.

Raw data: [`../bench/2026-09-27-2da741e-softmmu/`](../bench/2026-09-27-2da741e-softmmu/), [`../bench/2026-09-27-6c95332-softmmu/`](../bench/2026-09-27-6c95332-softmmu/).

| workload | config | score | vs direct | guest MIPS | dispatcher entries / M insns | host bytes / guest insn |
|---|---|---:|---:|---:|---:|---:|
| CoreMark (it/s) | interp (batch 1) | 520 (486–529) | | 186 | | |
| | qemu-riscv64 (batch 1) | 9,242 (9,059–10,032) | | 3,316 (est.) | | |
| | direct, `jit+linear` (batch 1) | 14,144 (13,651–14,431) | 1.00 | 5,071 | 10 | 24.9 |
| | softmmu, same-page chaining (batch 1) | 6,318 (6,168–6,787) | 0.45 | 2,264 | 2,366 | 107.3 |
| | direct, `jit+linear` (batch 2) | 13,789 (13,512–14,074) | 1.00 | 4,944 | 10 | 24.9 |
| | **softmmu, flat chaining (batch 2)** | **7,498 (7,440–7,574)** | **0.54** | **2,688** | 10 | |
| Dhrystone (runs/s) | interp (batch 1) | 517,037 | | 174 | | |
| | qemu-riscv64 (batch 1) | 5,142,221 | | 1,728 (est.) | | |
| | direct (batch 1) | 21,478,937 | 1.00 | 7,208 | 10 | 24.3 |
| | softmmu, same-page chaining (batch 1) | 7,705,864 | 0.36 | 2,585 | 2,977 | 117.7 |
| | direct (batch 2) | 21,803,943 (21,598,316–22,869,068) | 1.00 | 7,317 | 10 | 24.3 |
| | **softmmu, flat chaining (batch 2)** | **10,856,731 (10,704,928–11,454,179)** | **0.50** | **3,642** | 10 | |

**Findings:**
- **Cost of the TLB:** with every load and store going through the inline TLB, CoreMark keeps 54% and Dhrystone 50% of direct-mode speed.
- **Absolute speed:** both still run at 2.7–3.6 billion guest instructions/s, far above the ≥ 120 MIPS softmmu target of §22.
- **Against QEMU** (`qemu-riscv64` maps guest memory directly, like our direct mode): softmmu is 0.81× QEMU on CoreMark and 2.1× on Dhrystone.
- **The chaining fix (§4):** it took dispatcher entries from 2,366 to 10 per M instructions, and gained +19% CoreMark and +41% Dhrystone (softmmu, batch 1 → batch 2).
- **What remains** is the probe itself: 9 instructions and two TLB-entry loads per access, plus the larger code.

### 7.2 TLB microbenchmark (P7.9, `tools/tlb-bench.py`)

**Method:**
- **Workload:** `tlbbench.c` builds Sv39 tables (4096 data pages at VA 0x4000_0000 plus an identity 1 GiB page for code, stack and `tohost`), `mret`s into S-mode, and times two kernels with `rdtime` (10 MHz):
  - **chase:** 4 M dependent loads around a ring with one node per page, so latency matters.
  - **stream:** 4 M independent loads, one per page, walking the pages in order, so throughput matters.
- **Working sets:** 8 pages (all TLB hits after warm-up) and 4096 pages (a 256-entry direct-mapped TLB misses on every access).
- **Baseline:** each kernel has one with the same loop minus the load, which is subtracted.
- **User-mode twin:** `tlbuser.c` runs the same kernels under `--mem=direct` (raw host loads) and `--mem=softmmu` (TLB, identity fill, no walk), to separate the TLB machinery from the host's own load latency.
- **Runs:** 5 runs after 1 warm-up, median (min–max).
- **Cycle column:** cycles at the 2.1 GHz nominal TSC frequency. The core may run faster (the raw dependent load measures 1.0 ns, about 4 cycles at a plausible 4 GHz), so treat nanoseconds as the primary unit.

| config | kernel | ns/access | cycles @ 2.1 GHz |
|---|---|---:|---:|
| sv39 (JIT, S-mode) | chase W=8 (hit) | 4.15 (3.98–4.20) | 8.7 |
| sv39 | stream W=8 (hit) | 0.34 (0.26–0.49) | 0.7 |
| sv39 | chase W=4096 (miss + walk) | 28.09 (27.60–34.98) | 59.0 |
| sv39 | stream W=4096 (miss + walk) | 28.10 (27.18–29.07) | 59.0 |
| direct (user) | chase W=8 | 1.01 (0.78–1.02) | 2.1 |
| direct | stream W=8 | 0.01 (−0.00–0.04) | 0.0 |
| direct | chase W=4096 | 16.31 (14.68–20.49) | 34.3 |
| direct | stream W=4096 | 2.56 (2.41–3.37) | 5.4 |
| softmmu (user) | chase W=8 | 3.27 (3.24–4.18) | 6.9 |
| softmmu | stream W=8 | 0.32 (0.31–0.54) | 0.7 |
| softmmu | chase W=4096 (miss, identity fill) | 17.07 (15.86–21.02) | 35.8 |
| softmmu | stream W=4096 (miss, identity fill) | 13.76 (13.29–17.33) | 28.9 |

Raw output: [`../bench/2026-09-27-2da741e-softmmu/tlb.md`](../bench/2026-09-27-2da741e-softmmu/tlb.md).

**Reading it:**
- **A TLB hit** adds about 2.3–3.1 ns of load-to-use latency to a dependent load (3.3–4.1 vs 1.0 ns raw), roughly 5–7 nominal cycles. That is the dependent chain `lea → mov → shr → and → cmp [tag]`, then `add [addend]` and the load.
- **In throughput** (independent accesses), a hit costs about 0.3 ns (under 1 nominal cycle) per access, because out-of-order execution overlaps the probe.
- **An Sv39 miss** (helper call + 3-level walk + fill) costs about 28 ns per access, about 59 nominal cycles, i.e. 24 ns more than a hit.
- **An identity-fill miss** (user-mode softmmu) costs 14–17 ns, part of it the host's own TLB misses on 4096 pages (visible in the direct numbers: 16 ns for the dependent chain).
- **The "45 → 4 cycles" bullet (§28.1)** should say what was measured: a hit costs < 1 cycle per access in throughput (about 5–7 cycles of added latency); a miss with a full Sv39 walk costs about 59 nominal cycles.
- **No QEMU comparison:** QEMU's `-M spike` HTIF only proxies single-character console writes, so `tlbbench` can't print under it.

### 7.3 Code size
Softmmu TBs are about 4.5× larger: 107 host bytes per guest instruction on CoreMark vs 25 in direct mode. Almost all of it is the cold slow-path stub per access (7 saves, 7 restores, argument setup, call, fault exit: about 200 bytes). It sits at the TB tail, out of the hot path, but it costs I-cache/TLB footprint and translation time (12.1 vs 8.6 ms). A shared, per-register-set out-of-line thunk would shrink it (§10).

## 8. Bugs found and fixed

| Symptom | Root cause | Fix (commit) | Regression test |
|---|---|---|---|
| softmmu fuzzer: `x11: jit 0x0, interp 0x1` after a store page fault | `mv a1, a0` left a1 dirty as a copy of pinned a0 (R14); the slow stub saved only caller-saved registers to `fault_regs`, so the state map read a stale R14 slot | the fault path also stores R12–R15 (`2da741e`) | `tests/softmmu.proptest-regressions` |
| `rv64mi-p-breakpoint` failed test 2 | `tselect`/`tdata*` were illegal CSRs | they read as "no trigger" (`f190736`) | riscv-tests |
| user-program suite failed after adding a bare-metal ELF | it runs every `guest/build/*.elf` as a Linux program | bare-metal builds go to `guest/build/bare/` (`2da741e`) | the suite itself |
| lockstep + softmmu 9× slower than needed (216 s) | a 32 KiB TLB copy per TB | flush only when the reference run filled (`2da741e`) | runtime of `user_programs` |
| softmmu CoreMark: 2,366 dispatcher entries per M instructions (vs 10) | the system-mode same-page chaining rule applied to user-mode softmmu | `softmmu = 2` (flat) chains across pages (`6c95332`) | benchmark table (§7.1) |
| direct mode: a block could continue onto the next page after a page-straddling instruction | the stop condition only checked `next & 0xfff == 0` | stop when the next pc leaves the block's first page (`f190736`) | riscv-tests, fuzzers |

## 9. Deviations from the plan / spec

- **Four MMU indices** (S+SUM separate) instead of three plus MPRV variants (D48; CLAUDE.md §8.1/§14.4 updated).
- **Slow-path state:** the slow path saves the caller-saved registers and reuses the D37 state map, instead of storing the dirty guest registers itself (§4). CLAUDE.md §14.4 now describes the emitted sequence.
- **Page-straddling instructions** are interpreted, not translated as one-instruction TBs with two pages (§13.2).
- **No `addr_code` check per jump-cache hit:** the cache is reset whenever flags or `mmu_gen` change instead (roadmap P7.6 annotated).
- **The TLB size is fixed at 256** (`tlb::TLB_BITS`); there is no `--tlb-size` flag yet.
- **WFI is still a no-op** (no timer until Phase 9).
- **TLB statistics** count fills (misses), not hits; there is no hit counter on the hot path.

## 10. Known limitations and technical debt

- **Slow-path code size** (§7.3): a shared thunk per (size, signedness, direction) with the register saves inside would cut TB size about 3×.
- **SFENCE.VMA flushes everything**, with no per-address or per-ASID flush. ASIDs are stored but unused. That matters for Linux (Phase 9).
- **The jump cache is cleared (64 KiB) on every flags change,** i.e. on every privilege transition in system mode. Fine for the tests; Linux syscalls will pay it. A fill-list clear or flag-tagged entries would be cheaper.
- **PMP registers are stored but never enforced.**
- **No Sv48** (Phase 10).
- **Hit counts are not measured.**
- **Interrupts:** only software-set `mip` bits can be pending. Timers and external interrupts come with the CLINT/PLIC (Phase 9).

## 11. How to reproduce

```
tools/setup.sh && cargo build --release
tools/build-riscv-tests.sh && tools/build-guests.sh && tools/build-bench.sh
BRIDGEV_REQUIRE_GUESTS=1 cargo test --release          # all suites incl. 244 riscv-tests × 8 configs
PROPTEST_CASES=1000000 cargo test --release --test softmmu
python3 tools/tlb-bench.py                             # §7.2
python3 tools/bench.py --suite coremark,dhrystone --configs jit+linear,softmmu   # §7.1
target/release/bridgev run --mode bare --engine jit --dump-x86 /tmp/x86 --max-insns 3000000 \
    guest/build/bench/tlbbench.riscv                   # §5 (tb_0000000080002004)
target/release/bridgev run --mode bare --engine jit guest/build/bare/qsort.elf   # HTIF printf
```

## 12. Next steps

Phase 8 (self-modifying code) builds on this phase:
- the `TLB_CODE` flag bit reserved here
- the physical-page TB keys
- the per-page permission table in `DirectMem`

The plan is a code-page mark in `DirectMem`: a host write-protect in direct mode, `TLB_CODE` in softmmu. On a write, both engines leave right after the store, and the dispatcher invalidates the page's TBs, which makes FENCE.I cheap.
