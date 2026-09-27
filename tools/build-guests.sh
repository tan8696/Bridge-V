#!/usr/bin/env bash
# Build all RISC-V guest test programs into guest/build/ (P0.4).
#   asm:  guest/asm/<name>.S -> guest/build/<name>.elf              (clang + lld, no libc)
#   C:    guest/c/<name>.c   -> guest/build/<name>-O{0,2}[-nc].elf  (static glibc)
#         "-nc" = compiled without the C extension (rv64imafd). The glibc objects linked in are
#         still rv64gc, so "-nc" binaries are compressed-free only in the program's own code.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
OUT="$ROOT/guest/build"
CC=${RISCV_CC:-riscv64-linux-gnu-gcc}
CLANG=${CLANG:-clang}
mkdir -p "$OUT"

built=0
for src in "$ROOT"/guest/asm/*.S; do
  name=$(basename "$src" .S)
  "$CLANG" --target=riscv64-unknown-linux-gnu -march=rv64gc -mabi=lp64d \
    -nostdlib -static -fuse-ld=lld -o "$OUT/$name.elf" "$src"
  built=$((built + 1))
done

for src in "$ROOT"/guest/c/*.c; do
  name=$(basename "$src" .c)
  for opt in O0 O2; do
    for variant in gc nc; do
      if [[ $variant == gc ]]; then march=rv64gc; suffix=""; else march=rv64imafd_zicsr_zifencei; suffix="-nc"; fi
      "$CC" -static -"$opt" -march="$march" -mabi=lp64d -Wall -Werror \
        -o "$OUT/$name-$opt$suffix.elf" "$src" -lm
      built=$((built + 1))
    done
  done
done

# Bare-metal riscv-tests benchmarks (HTIF printf through tohost/fromhost, P7.7): qsort. In a
# subdirectory: tests/user_programs.rs runs every guest/build/*.elf as a Linux program.
RVB="$ROOT/third_party/riscv-tests/benchmarks"
[[ -f "$RVB/common/crt.S" ]] || git -C "$ROOT" submodule update --init third_party/riscv-tests
mkdir -p "$OUT/bare"
for b in qsort; do
  "$CC" -DPREALLOCATE=1 -mcmodel=medany -static -std=gnu99 -O2 -ffast-math -fno-common \
    -fno-builtin-printf -fno-tree-loop-distribute-patterns -march=rv64gc -mabi=lp64d -no-pie \
    -fno-pic -Wl,--build-id=none -nostdlib -nostartfiles -I"$RVB/common" -I"$RVB/$b" \
    -I"$ROOT/third_party/riscv-tests/env" -T "$RVB/common/test.ld" -o "$OUT/bare/$b.elf" \
    "$RVB/$b"/*.c "$RVB/common/syscalls.c" "$RVB/common/crt.S" -lgcc
  built=$((built + 1))
done

echo "build-guests: built $built programs into ${OUT#"$ROOT"/}"
