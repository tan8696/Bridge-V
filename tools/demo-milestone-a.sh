#!/usr/bin/env bash
# Milestone A demo (P5.4): build Bridge-V and the benchmarks, run CoreMark under the reference
# interpreter and under the JIT (valid >= 10 s runs, calibrated by tools/bench.py), and print
# the speedup. Needs the packages from tools/setup.sh. About 2 minutes after the build.
#   tools/demo-milestone-a.sh           CoreMark, interpreter vs JIT
#   tools/demo-milestone-a.sh --all     also Dhrystone, every JIT level, qemu-riscv64 and native
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"
git submodule update --init third_party/coremark third_party/riscv-tests >/dev/null
cargo build --release
tools/build-bench.sh
OUT=target/demo-milestone-a
if [[ ${1:-} == --all ]]; then
  python3 tools/bench.py --runs 1 --warmup 0 --out "$OUT"
else
  python3 tools/bench.py --suite coremark --configs interp,jit+linear --runs 1 --warmup 0 --out "$OUT"
fi
python3 - "$OUT/results.json" <<'PY'
import json, sys
r = json.load(open(sys.argv[1]))["results"]["coremark"]
i, j = r["interp"], r["jit+linear"]
print(f"\nCoreMark (validated): interpreter {i['median']:,.1f} it/s, JIT {j['median']:,.1f} it/s"
      f" -> JIT speedup {j['median'] / i['median']:.1f}x over the interpreter")
PY
