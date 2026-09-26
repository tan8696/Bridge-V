#!/usr/bin/env bash
# Run a RISC-V Linux guest program under the reference emulator (qemu-riscv64) and propagate
# its stdout and exit status. Used for differential comparison against bridgev.
set -euo pipefail
if [[ $# -lt 1 ]]; then echo "usage: $0 <elf> [args...]" >&2; exit 64; fi
exec qemu-riscv64 "$@"
