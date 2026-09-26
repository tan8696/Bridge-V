#!/usr/bin/env bash
# Build the Phase 5 benchmarks (P5.1) into guest/build/bench/, for the RISC-V guest and for the
# x86-64 host (the "native" column of the benchmark matrix), from the same sources and flags:
#   coremark-rv64.elf / coremark-native   EEMBC CoreMark, `make PORT_DIR=linux`, -O2, static,
#                                         PERFORMANCE_RUN=1; iterations are passed at run time
#                                         (argv: 0x0 0x0 0x66 <iterations>).
#   dhrystone-rv64.elf / dhrystone-native Dhrystone 2.1 from third_party/riscv-tests (unmodified)
#                                         + guest/bench/dhrystone shim; argv[1] = runs.
#   fpbench-rv64.elf / fpbench-native     guest/bench/fp/fpbench.c (P6.6: nbody, sgemm, int<->FP
#                                         conversions, self-validating); argv[1] = units.
# Writes guest/build/bench/BUILDINFO.txt with compiler versions, flags, source commits and hashes.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
OUT="$ROOT/guest/build/bench"
RV_CC=${RISCV_CC:-riscv64-linux-gnu-gcc}
HOST_CC=${HOST_CC:-gcc}
RV_ARCH="-march=rv64gc -mabi=lp64d"
mkdir -p "$OUT"

[[ -f "$ROOT/third_party/coremark/core_main.c" ]] || git -C "$ROOT" submodule update --init third_party/coremark
[[ -f "$ROOT/third_party/riscv-tests/benchmarks/dhrystone/dhrystone.c" ]] ||
  git -C "$ROOT" submodule update --init third_party/riscv-tests

coremark() { # <cc> <extra flags> <output name>
  local obj="$OUT/obj-$3/"
  rm -rf "$obj"
  make -s -C "$ROOT/third_party/coremark" PORT_DIR=linux CC="$1" \
    XCFLAGS="$2 -static -DPERFORMANCE_RUN=1" OPATH="$obj" link >/dev/null
  mv "$obj/coremark.exe" "$OUT/$3"
  rm -rf "$obj"
}
coremark "$RV_CC" "$RV_ARCH" coremark-rv64.elf
coremark "$HOST_CC" "" coremark-native

DHRY="$ROOT/third_party/riscv-tests/benchmarks/dhrystone"
# K&R-style C (implicit int): gnu89. -O2 like CoreMark; the sources' own
# `#pragma GCC optimize ("no-inline")` keeps procedures out of line (Dhrystone ground rules).
# dhrystone_main.c's debug_printf (the final-value report) is renamed to the shim's printf;
# riscv-tests' empty debug_printf in dhrystone.c stays unused.
DHRY_FLAGS="-O2 -static -std=gnu89 -w -I$ROOT/guest/bench/dhrystone -I$DHRY"
dhrystone() { # <cc> <arch flags> <output name>
  local obj="$OUT/obj-$3"
  mkdir -p "$obj"
  # shellcheck disable=SC2086
  "$1" $2 $DHRY_FLAGS -Ddebug_printf=bridgev_dhry_printf -c "$DHRY/dhrystone_main.c" -o "$obj/main.o"
  # shellcheck disable=SC2086
  # PASS2: dhrystone.h defines `struct tms time_info` (native times() path) in every file
  # unless PASS2 is set; modern GCC's -fno-common rejects the duplicate.
  "$1" $2 $DHRY_FLAGS -DPASS2 -c "$DHRY/dhrystone.c" -o "$obj/dhry.o"
  # shellcheck disable=SC2086
  "$1" $2 $DHRY_FLAGS -c "$ROOT/guest/bench/dhrystone/shim.c" -o "$obj/shim.o"
  # shellcheck disable=SC2086
  "$1" $2 -static "$obj/main.o" "$obj/dhry.o" "$obj/shim.o" -o "$OUT/$3"
  rm -rf "$obj"
}
dhrystone "$RV_CC" "$RV_ARCH" dhrystone-rv64.elf
dhrystone "$HOST_CC" "" dhrystone-native

FP_FLAGS="-O2 -static"
# shellcheck disable=SC2086
"$RV_CC" $RV_ARCH $FP_FLAGS "$ROOT/guest/bench/fp/fpbench.c" -o "$OUT/fpbench-rv64.elf" -lm
# shellcheck disable=SC2086
"$HOST_CC" $FP_FLAGS "$ROOT/guest/bench/fp/fpbench.c" -o "$OUT/fpbench-native" -lm

{
  echo "# Bridge-V benchmark builds (tools/build-bench.sh), $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "riscv cc: $("$RV_CC" --version | head -1)"
  echo "host cc:  $("$HOST_CC" --version | head -1)"
  echo "coremark: $(git -C "$ROOT/third_party/coremark" rev-parse HEAD) make PORT_DIR=linux, PORT_CFLAGS=-O2, XCFLAGS='<arch> -static -DPERFORMANCE_RUN=1' (rv64: $RV_ARCH)"
  echo "dhrystone: riscv-tests $(git -C "$ROOT/third_party/riscv-tests" rev-parse HEAD) benchmarks/dhrystone + guest/bench/dhrystone; flags: $DHRY_FLAGS, main: -Ddebug_printf=bridgev_dhry_printf, dhrystone.c: -DPASS2 (rv64: + $RV_ARCH)"
  echo "fpbench: guest/bench/fp/fpbench.c; flags: $FP_FLAGS -lm (rv64: + $RV_ARCH)"
  (cd "$OUT" && sha256sum coremark-rv64.elf coremark-native dhrystone-rv64.elf dhrystone-native \
    fpbench-rv64.elf fpbench-native)
} > "$OUT/BUILDINFO.txt"
echo "build-bench: built 6 benchmarks into ${OUT#"$ROOT"/}"
