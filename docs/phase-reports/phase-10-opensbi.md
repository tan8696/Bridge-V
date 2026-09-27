# Phase 10 mini-report: OpenSBI boot

| Field | Value |
|---|---|
| Item | OpenSBI boot (Phase 10 stretch goal 1, `docs/ROADMAP.md`) |
| Status | Complete |
| Date | 2026-09-27 |
| Commit | `57af1e0` (code), plus the commit adding this report |

## 1. Summary
`bridgev boot --firmware <fw_dynamic.bin>` runs real M-mode firmware instead of the built-in SBI (D15). The firmware is QEMU's own OpenSBI v1.3 (`/usr/share/qemu/opensbi-riscv64-generic-fw_dynamic.bin`, package `qemu-system-data`), unmodified.

What happens on a boot:
- OpenSBI starts in M-mode, discovers the machine from bridgev's devicetree, and sets up PMP, delegation and its console.
- It hands over to Linux in S-mode.
- Every SBI call Linux makes (21,773 per boot) is then an ECALL trapping to M-mode, served by guest firmware code under the JIT.

Linux reaches the BusyBox shell and powers off through OpenSBI's `sifive_test` driver. The whole run is clean in lockstep: 108.9 M translated blocks identical to the interpreter, M-mode included. This validates the privileged implementation against real firmware, not just against Bridge-V's own SBI.

## 2. What was built
- **Loading** (`src/system/machine.rs`): the firmware goes at the start of RAM (0x8000_0000, below the kernel at 0x8020_0000; overlap is an error).
- **`fw_dynamic_info`:** a struct (OpenSBI `include/sbi/fw_dynamic.h`, version 2: magic `OSBI`, version, `next_addr` = kernel entry, `next_mode` = S, options 0, `boot_hart` 0) is placed 1 MiB above the DTB.
- **Entry:** the hart starts in M-mode at the firmware with reset CSR values and `a0 = 0`, `a1 = DTB`, `a2 = &fw_dynamic_info`, which is what QEMU's reset vector does. `fw_jump` images ignore `a2`.
- **With firmware:**
  - `Env::sbi` is off, so S-mode ECALLs trap normally (the firmware sets `medeleg`/`mideleg`).
  - The machine loop no longer owns `mip.STIP`. The built-in SBI drives it from its own timer, but OpenSBI sets it from its M-mode timer handler, so the loop keeps the software value.
- **WFI idle vs poweroff.** OpenSBI's `sbi_system_reset` writes the test finisher, then parks the hart in `wfi` with no timer armed. WFI idle (D50) then slept "until input", and the machine never saw the finisher request. The idle loop now stops waiting as soon as a finish request is pending. This was the one bug of the bring-up.
- `--firmware` in `bridgev boot`; `boot-bench.py` configs `<engine>-opensbi`; CI installs `qemu-system-data`.

## 3. Evidence
OpenSBI's banner under bridgev (excerpt):
```
OpenSBI v1.3
Platform Name             : bridgev,virt
Platform IPI Device       : aclint-mswi
Platform Timer Device     : aclint-mtimer @ 10000000Hz
Platform Console Device   : uart8250
Platform Reboot Device    : sifive_test
Platform Shutdown Device  : sifive_test
Domain0 Next Address      : 0x0000000080200000
Domain0 Next Mode         : S-mode
Boot HART Base ISA        : rv64imafdc
Boot HART ISA Extensions  : time
Boot HART PMP Count       : 64
Boot HART PMP Granularity : 4
Boot HART PMP Address Bits: 54
Boot HART MIDELEG         : 0x0000000000000222
Boot HART MEDELEG         : 0x000000000000b109
[    0.000000] Linux version 6.8.0-60-generic ...
[    0.000000] SBI specification v1.0 detected
[    0.000000] SBI implementation ID=0x1 Version=0x10003
[    0.000000] OF: reserved mem: 0x0000000080000000..0x000000008003ffff (256 KiB) nomap non-reusable mmode_resv1@80000000
```
OpenSBI found everything through the devicetree alone: the CLINT (as ACLINT MSWI/MTIMER), the 16550, the test finisher, 64 PMP entries and the time CSR.

| Test (`tests/linux_boot.rs`, `--ignored`) | Result |
|---|---|
| `linux_boots_via_opensbi_jit` | pass: shell after 1.7 s, `0 SBI calls` (the built-in SBI is unused), 21,773 ECALL exits to M-mode |
| `linux_boots_via_opensbi_lockstep` | pass: 108,914,902 TBs identical, 1.07 G instructions |

Both run in CI (`linux-boot` job).

**Time to shell** (`tools/boot-bench.py`, Xeon @ 2.10 GHz, noisy VM, 5 runs, median (min–max); `docs/bench/2026-09-27-57af1e0-boot/`):

| config | time to shell (s) | MIPS |
|---|---:|---:|
| jit, built-in SBI | 1.37 (1.19–1.41) | 641 |
| jit, OpenSBI | 1.52 (1.43–1.55) | 585 |
| qemu-system-riscv64 (also OpenSBI) | 1.45 (1.24–1.49) | — |

OpenSBI costs about 0.15 s: each SBI call becomes a trap into M-mode, firmware code, and an MRET, instead of a Rust function call.

## 4. Limitations
- **Firmware formats:** only raw images (`fw_dynamic`/`fw_jump` `.bin`). ELF firmware is not loaded.
- **Hardware setup:** `mtime` is not writable. The firmware's PMP settings are stored but not enforced: physical permissions come from `DirectMem` (D48), so S-mode could access the firmware's memory. Linux doesn't, because it honours the reserved-memory nodes OpenSBI adds.
- **IPIs:** a CLINT `msip` write is noticed at the next slice boundary, not immediately. That is enough for one hart.

## 5. Reproduce
```sh
tools/setup.sh && tools/fetch-guest-images.sh
target/release/bridgev boot --firmware /usr/share/qemu/opensbi-riscv64-generic-fw_dynamic.bin \
    --kernel guest/build/linux/Image --initrd guest/build/linux/rootfs.cpio
cargo test --release --test linux_boot opensbi -- --ignored --test-threads 1
```
