# Phase 10 mini-report: SMP guest

| Field | Value |
|---|---|
| Item | SMP guest (Phase 10 stretch goal 10, `docs/ROADMAP.md`) |
| Status | Done, with harts run one at a time (round-robin per slice) |
| Date | 2026-09-27 |
| Commit | the commit adding this report |

## 1. Summary
`bridgev boot --smp N` (N ≤ 8) gives the machine N harts. Linux 6.8 brings them all up, both with the built-in SBI (HSM `hart_start`) and through OpenSBI (all harts enter the firmware, which releases the secondaries):
```
[    0.000000] rcu: 	RCU restricting CPUs from NR_CPUS=32 to nr_cpu_ids=4.
[    0.200607] smp: Brought up 1 node, 4 CPUs
/ # cat /proc/cpuinfo   → processor 0 … processor 3
```
With OpenSBI v1.3: `Platform HART Count : 4`, `Domain0 HARTs : 0*,1*,2*,3*`, and the same 4 CPUs in Linux.

The harts run one at a time in the machine thread, round-robin per slice (`--slice`, default 100 k instructions). This is the same trade-off as the user-mode threads (D55): the guest sees a real SMP machine (IPIs, remote fences, per-hart timers and interrupt contexts, CPU hotplug calls), but there is no parallel speedup.

## 2. What was built
- **Harts** (`src/system/machine.rs`, `struct Hart`): each has its own `CpuState` (registers, CSRs with its `mhartid`, TLBs, jump cache) and its own engine (translation cache), an HSM status, and a WFI "waiting" flag.
  - All harts share hart 0's `time` origin, so `time` agrees across harts.
  - A switch to a different hart clears its LR reservation.
- **Scheduling:** the loop computes every hart's `mip` from the devices, then runs the next started, non-waiting hart for one slice.
  - A hart that executes WFI with nothing pending waits until an interrupt is pending in its `mip & mie`.
  - When every started hart waits, the host sleeps until the earliest CLINT/SBI deadline, console input, a disk request or a finish request.
- **Devices:**
  - CLINT: `msip[h]` at 4·h and `mtimecmp[h]` at 0x4000 + 8·h.
  - PLIC: 2 contexts per hart (M = 2h, S = 2h + 1; enable, threshold and claim per context), `outputs(h)`.
  - Devicetree: `cpu@0..N-1`, each with its own `riscv,cpu-intc`; CLINT and PLIC `interrupts-extended` list every hart.
- **Built-in SBI:** calls that affect other harts return actions the machine applies to every hart in the mask, the caller included:
  - IPI → SSIP;
  - remote FENCE.I → that hart's engine `fence_i`;
  - remote SFENCE.VMA → that hart's TLB flush;
  - HSM `hart_start` → a fresh S-mode register state at the start address with `a0` = hart id and `a1` = opaque;
  - `hart_stop`; `hart_get_status` reports STARTED/STOPPED/START_PENDING.
- **Self-modifying code across harts:** `DirectMem::smc_log` records every code-page write. Before a hart runs, its engine is given the pages written since it last ran (as `smc_pages`), so it invalidates exactly those translations (D49), not its whole cache.
- **CLI:** `--smp N`.

## 3. Evidence
| Test (`tests/linux_boot.rs`, `--ignored`, in CI) | Result |
|---|---|
| `linux_boots_with_4_harts_jit`: built-in SBI; `/proc/cpuinfo` must list processor 3; shell commands; poweroff | pass |
| `linux_boots_with_4_harts_via_opensbi_jit`: the same through OpenSBI | pass |
| `system::plic::tests::claim_complete_priority_threshold` (extended): hart 2's S context has its own enables and claim | pass |
| Manual: `--smp 2` jit boot | "Brought up 1 node, 2 CPUs"; 708 M / 307 M instructions on harts 0/1 |

**Lockstep:** a lockstep boot with `--smp 2` ran for 400 s of wall-clock time without a divergence. It reached only 5.8 s of kernel time, short of the shell. Under lockstep, time follows each hart's own instruction count and WFI is a no-op (D50), so idle harts spin, and every TB also runs twice. SMP lockstep is therefore impractically slow and is not in the test suite; single-hart lockstep boots (with and without OpenSBI) are.

**Cost:** a whole session (boot, three shell commands, poweroff; one run each, built-in SBI, jit) takes 1.52 s with 1 hart, 1.88 s with 2 and 2.82 s with 4. The harts' boot work is serialized, and every hart does its own per-CPU initialisation. Parallel execution is the natural next step (§4).

## 4. Limitations
- **No parallel execution** (see D55's list for what it would take).
- **Deterministic time** (`--deterministic`, lockstep) counts each hart's own instructions, so harts' clocks drift apart there.
- **HSM** `hart_suspend` is not supported (Linux only uses it with idle states in the devicetree).
- **Legacy SBI v0.1** IPI/fence calls act on all harts (their masks are virtual addresses); Linux uses the v0.2 extensions.
