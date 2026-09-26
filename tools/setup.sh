#!/usr/bin/env bash
# Install the host packages Bridge-V needs (CLAUDE.md §4). Idempotent: packages that are
# already present are skipped, so re-running is cheap.
set -euo pipefail

PKGS=(
  # RISC-V cross toolchain for guest programs, riscv-tests, CoreMark, Linux
  gcc-riscv64-linux-gnu g++-riscv64-linux-gnu libc6-dev-riscv64-cross
  # Reference emulators for differential testing
  qemu-user qemu-system-misc
  # Devicetree compiler (validate generated DTBs)
  device-tree-compiler
  # Build tools for riscv-tests and the Linux kernel
  autoconf automake make flex bison bc libssl-dev libelf-dev cpio
)

SUDO=""
if [[ $(id -u) -ne 0 ]]; then SUDO="sudo"; fi

missing=()
for p in "${PKGS[@]}"; do
  dpkg-query -W -f='${Status}' "$p" 2>/dev/null | grep -q "install ok installed" || missing+=("$p")
done

if ((${#missing[@]})); then
  echo "setup: installing: ${missing[*]}"
  # Third-party PPAs may be blocked by the network policy; the Ubuntu archive is enough.
  $SUDO apt-get update -qq || echo "setup: warning: apt-get update reported errors (continuing)"
  if ! $SUDO env DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends "${missing[@]}"; then
    echo "setup: ERROR: apt-get install failed. If the network policy blocks the Ubuntu archive," >&2
    echo "setup: check: curl -sS \"\$HTTPS_PROXY/__agentproxy/status\"" >&2
    exit 1
  fi
else
  echo "setup: all packages already installed"
fi

echo "setup: tool versions"
for t in rustc cargo clang ld.lld riscv64-linux-gnu-gcc qemu-riscv64 qemu-system-riscv64 dtc; do
  if command -v "$t" >/dev/null 2>&1; then
    printf '  %-22s %s\n' "$t" "$("$t" --version 2>&1 | head -1)"
  else
    printf '  %-22s MISSING\n' "$t"
  fi
done
