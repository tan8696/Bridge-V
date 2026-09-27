Same-batch A/B of the Linux boot (Phase 11, 2026-09-27): Phase 9 build `c24bc9c` against HEAD (`af6fcae`, same code as `567255a`), alternating, 5 runs each, `taskset -c 2`.
Time to the first `/ # ` prompt as seen by a Python driver that polls the console every 10 ms
(so about 0.1 s above `tools/boot-bench.py`'s numbers).

| build | runs (s) | median |
|---|---|---:|
| c24bc9c (Phase 9) | 1.45 1.53 1.57 1.58 1.62 | 1.57 |
| HEAD | 1.43 1.52 1.54 1.62 1.62 | 1.54 |

Conclusion: the Phase 10 code boots as fast as the Phase 9 code. The difference between
`boot.md` (jit 1.48 s) and the Phase 9 measurement (1.25 s) is the host, not the code; QEMU on this
host also measured 1.54 s against 1.50 s then.

Same-machine `--stats` for one HEAD run: 926,257,566 guest instructions, 66,001 TBs translated
(translate time 590 ms), 698,317 dispatcher entries, 642,031 TLB fills; Phase 9
(`../2026-09-27-c24bc9c-boot/stats.txt`): 928,557,222, 66,669 TBs (552 ms), 703,121, 673,941.
