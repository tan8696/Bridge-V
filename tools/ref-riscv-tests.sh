#!/usr/bin/env bash
# Run the built riscv-tests under the reference emulator: QEMU's `spike` machine implements the
# HTIF tohost/fromhost protocol the tests use to report pass (exit 0) or fail (exit = test no.).
# Confirms the test ELFs themselves are valid before bridgev runs them (P0.5).
# Tests listed in tests/data/qemu-known-failures.txt are expected to fail under QEMU (XFAIL);
# an unexpected pass of such a test is reported too, so the list stays accurate.
#   tools/ref-riscv-tests.sh [glob]     default glob: rv64*
set -uo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
DIR="$ROOT/guest/build/riscv-tests"
KNOWN=$(grep -v '^#' "$ROOT/tests/data/qemu-known-failures.txt" | grep -v '^$' || true)
pass=0; fail=0; xfail=0
for elf in "$DIR"/${1:-rv64*}; do
  name=$(basename "$elf")
  timeout 20 qemu-system-riscv64 -M spike -nographic -bios "$elf" >/dev/null 2>&1
  code=$?
  if grep -qx "$name" <<< "$KNOWN"; then
    if ((code == 0)); then fail=$((fail + 1)); echo "XPASS (remove from known-failures): $name"
    else xfail=$((xfail + 1)); fi
  elif ((code == 0)); then pass=$((pass + 1))
  else fail=$((fail + 1)); echo "FAIL($code): $name"; fi
done
echo "ref-riscv-tests: $pass passed, $xfail expected failures (QEMU limitations), $fail failed"
((fail == 0))
