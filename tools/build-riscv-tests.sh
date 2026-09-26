#!/usr/bin/env bash
# Build the official riscv-tests ISA suites (P0.5) out of tree, so the submodule stays clean.
# Output: guest/build/riscv-tests/<test> ELFs for the in-scope suites only (RV64GC + priv):
#   rv64ui rv64um rv64ua rv64uf rv64ud rv64uc rv64si rv64mi, in the -p (physical) and -v (Sv39)
#   environments. Out-of-scope suites (Zb*, Zfh, hypervisor, ...; CLAUDE.md §2) may fail to
#   build with the lp64d-only cross glibc; that is ignored, but a missing in-scope test is fatal.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
SRC="$ROOT/third_party/riscv-tests"
OBJ="$ROOT/guest/build/riscv-tests-obj"
OUT="$ROOT/guest/build/riscv-tests"
PREFIX=${RISCV_PREFIX:-riscv64-linux-gnu-}

[[ -f "$SRC/env/p/link.ld" ]] || { echo "missing submodules: git submodule update --init --recursive" >&2; exit 1; }
mkdir -p "$OBJ" "$OUT"
cd "$OBJ"
[[ -f Makefile ]] || "$SRC/configure" --with-xlen=64 > configure.log 2>&1
# -no-pie -fno-pic: Ubuntu's cross gcc defaults to PIE/PIC, which turns `la` into GOT loads;
# the tests are linked at fixed addresses (0x80000000). --build-id=none: the default
# .note.gnu.build-id would be placed at 0x80000000, ahead of _start in .text.init.
make -k -j"$(nproc)" isa RISCV_PREFIX="$PREFIX" \
  RISCV_GCC_OPTS="-static -no-pie -fno-pic -mcmodel=medany -fvisibility=hidden -nostdlib -nostartfiles -Wl,--no-warn-rwx-segments -Wl,--build-id=none" \
  > make.log 2>&1 || true

# Upstream compiles rv64ua with -march=rv64g_zacas_zabha, which binutils 2.42 (Ubuntu 24.04)
# rejects, so the whole suite is skipped. The classic A tests only use RV64GC instructions:
# build them here with -march=rv64g, using the same commands as isa/Makefile's compile_template.
# amocas_* (Zacas) is outside RV64GC and is excluded.
GCC="${PREFIX}gcc"
OPTS=(-static -no-pie -fno-pic -mcmodel=medany -fvisibility=hidden -nostdlib -nostartfiles -Wl,--no-warn-rwx-segments -Wl,--build-id=none)
for src in "$SRC"/isa/rv64ua/*.S; do
  t=$(basename "$src" .S); [[ $t == amocas_* ]] && continue
  "$GCC" -march=rv64g -mabi=lp64d "${OPTS[@]}" -I"$SRC/env/p" -I"$SRC/isa/macros/scalar" \
    -T"$SRC/env/p/link.ld" "$src" -o "isa/rv64ua-p-$t"
  "$GCC" -march=rv64g -mabi=lp64d "${OPTS[@]}" -DENTROPY=0x"$(echo "rv64ua-v-$t" | md5sum | cut -c 1-7)" \
    -std=gnu99 -O2 -I"$SRC/env/v" -I"$SRC/isa/macros/scalar" -T"$SRC/env/v/link.ld" \
    "$SRC/env/v/entry.S" "$SRC"/env/v/*.c "$src" -o "isa/rv64ua-v-$t"
done

SUITES=(rv64ui rv64um rv64ua rv64uf rv64ud rv64uc rv64si rv64mi)
missing=0
for suite in "${SUITES[@]}"; do
  frag="$SRC/isa/$suite/Makefrag"
  # Test names listed in the suite's Makefrag, e.g. "rv64ui_sc_tests = add addi ...".
  names=$(sed -n '/_sc_tests *=/,/^$/p' "$frag" | tr -d '\\' | sed 's/.*= *//' | tr -s ' \t\n' ' ')
  envs=(p); grep -q "_v_tests *=" "$frag" && envs+=(v)
  for env in "${envs[@]}"; do
    for t in $names; do
      [[ $suite == rv64ua && $t == amocas_* ]] && continue
      elf="isa/$suite-$env-$t"
      if [[ -x "$elf" ]]; then cp -p "$elf" "$OUT/"; else echo "MISSING: $elf" >&2; missing=$((missing + 1)); fi
    done
  done
done
echo "build-riscv-tests: $(find "$OUT" -type f | wc -l) in-scope test ELFs in ${OUT#"$ROOT"/} ($missing missing)"
((missing == 0))
