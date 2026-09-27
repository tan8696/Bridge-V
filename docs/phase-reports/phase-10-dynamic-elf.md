# Phase 10 mini-report: dynamic ELF and PIE

| Field | Value |
|---|---|
| Item | Dynamic ELF (Phase 10 stretch goal 4, `docs/ROADMAP.md`) |
| Status | Complete |
| Date | 2026-09-27 |
| Commit | the commit adding this report |

## 1. Summary
`bridgev run` now runs what `riscv64-linux-gnu-gcc` produces by default: dynamically linked, position-independent executables. The kernel's part of `execve` is emulated:
- the program goes to Linux's `ELF_ET_DYN_BASE`;
- its `PT_INTERP` (`/lib/ld-linux-riscv64-lp64d.so.1`) is loaded from a sysroot;
- the auxiliary vector points `ld.so` at the program.

`ld.so` then does its own work (opening and mapping `libc.so.6`, relocations, IRELATIVE/ifunc resolution, TLS, RELRO) through ordinary syscalls. Every C test program also runs as a dynamic PIE build, byte-identical to `qemu-riscv64 -L /usr/riscv64-linux-gnu`.

## 2. What was built
- **Loader** (`src/user/loader.rs`):
  - `map_segments(elf, bias)` maps the PT_LOAD segments at `vaddr + bias`.
  - ET_DYN programs get `bias = ET_DYN_BASE` (0x2a_aaaa_a000, which is TASK_SIZE/3·2 for Sv39); static ET_EXEC programs are unchanged.
  - The interpreter, itself an ET_DYN, goes to `INTERP_BASE` (0x3f_8000_0000, between the top-down mmap area and the stack), and execution starts at its entry.
  - The auxv carries `AT_BASE` = interpreter base, `AT_PHDR` = program headers + bias and `AT_ENTRY` = program entry + bias. `brk` starts after the biased image.
- **Sysroot** (`--sysroot`/`-L`, default `/usr/riscv64-linux-gnu` when the program has an interpreter): the interpreter path, and absolute paths passed to `openat`, `faccessat`, `newfstatat`, `readlinkat` and `statx`, are looked up in the sysroot first, then on the host (qemu's `-L` rule). The host's `/etc/ld.so.cache` is x86-64-only, so riscv `ld.so` skips its entries and searches its default directories, which resolve inside the sysroot.
- **Syscalls:**
  - A file-backed `mmap` of a read-only or executable mapping used to copy the file through a host page that was not writable. That crashed on the first `ld.so` mapping. The copy now goes through a writable mapping, which then gets the requested protection.
  - `pread64`/`pwrite64` added.

## 3. Evidence
- `tools/build-guests.sh` builds `<name>-O2-dyn.elf` (default dynamic PIE) for every C test program. `tools/ref-check.sh` checks them under `qemu-riscv64` with `QEMU_LD_PREFIX`, and `tests/user_programs.rs` runs them in all nine configurations: interp, jit, lockstep, no-chain, small code cache, mprotect W^X, regalloc levels and softmmu.
- The covered programs include `threads` (pthreads through `libc.so`), `signals`, `smc` (JIT-in-guest with `mmap` + `__builtin___clear_cache`), `printf_float`, `qsort`, `malloc`, `setjmp`, `stat` and `strings`.

## 4. Limitations
- **Loading:** no `execve` of other programs, and no `dlopen` beyond what `ld.so` does at start-up (`dlopen` at run time uses the same syscalls and should work, but is untested). `MAP_SHARED` file mappings are copies (writes are not written back).
- **Address layout:** the interpreter base is fixed, with no ASLR, like qemu-user by default.
