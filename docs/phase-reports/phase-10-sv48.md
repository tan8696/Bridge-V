# Phase 10 mini-report: Sv48

| Field | Value |
|---|---|
| Item | Sv48 (Phase 10 stretch goal 2, `docs/ROADMAP.md`) |
| Status | Complete |
| Date | 2026-09-27 |
| Commit | `57af1e0` (code), plus the commit adding this report |

## 1. Summary
`bridgev boot --mmu sv48` offers Sv48 as well as Sv39, and Linux 6.8 then uses four-level page tables: `/proc/cpuinfo` reports `mmu: sv48`. The default machine (`--mmu sv39`) is unchanged, and its `satp` ignores mode 9 writes.

## 2. What was built
- **Walker** (`src/mem/mmu.rs::walk`): Sv48 is the Sv39 walk with one more level (`top_level = 3`) and 48-bit virtual addresses. Bits 63:48 must copy bit 47, otherwise it is a page fault (priv spec §12.5). Superpages grow to 512 GiB terapages, which the existing alignment check and `page_mask` handle generically.
- **`satp`** (`src/cpu/csr.rs`): accepts `MODE = 9` only when the machine sets `Csrs::sv48`. Otherwise the write has no effect (WARL, priv spec §12.1.11). Linux probes the paging mode by writing `satp` and reading it back, so this is how it discovers Sv48.
- **Devicetree:** `mmu-type = "riscv,sv48"` when offered.
- **Nothing else changed.** The TLB compares full 64-bit page tags, so the inline JIT fast path is mode-agnostic. The superpage range used by per-page SFENCE.VMA (D51) records terapages like any other superpage.

## 3. Evidence
| Test | What | Result |
|---|---|---|
| `mem::mmu::tests::sv48_walk` | `satp` WARL with and without the option; a 4 KiB page above the Sv39 range through four levels; a 512 GiB terapage leaf (`page_mask` = 2^39 − 1); a misaligned terapage faults; a non-canonical Sv48 address faults; the same address faults under Sv39 | pass |
| `linux_boot::linux_boots_with_sv48_jit` | Boot with `--mmu sv48`; `/proc/cpuinfo` must say `sv48`; shell commands and poweroff | pass (in CI) |

**Time to shell** (same batch as the OpenSBI report, `docs/bench/2026-09-27-57af1e0-boot/`): jit with Sv48 1.38 s (1.15–1.44) against 1.37 s (1.19–1.41) with Sv39. The extra walk level only matters on TLB misses, and those are rare (about 0.7 per 1,000 instructions, Phase 9 report §7).

## 4. Limitations
- **Sv57** (five levels) is not offered.
- **User mode** uses the flat or direct memory backends and is unaffected.

## 5. Reproduce
```sh
target/release/bridgev boot --mmu sv48 --kernel guest/build/linux/Image --initrd guest/build/linux/rootfs.cpio
cargo test --release --lib sv48 && cargo test --release --test linux_boot sv48 -- --ignored
```
