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

echo "build-guests: built $built programs into ${OUT#"$ROOT"/}"
