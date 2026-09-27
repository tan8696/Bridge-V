#!/usr/bin/env bash
# Fetch the Phase 9 guest images (P9.4, D50) into guest/build/linux/:
#   Image        Ubuntu 24.04 riscv64 kernel (linux-image-6.8.0-60-generic, EFI-stub Image)
#   busybox      Ubuntu busybox-static 1.36.1 (riscv64, static)
#   rootfs.cpio  initramfs built by `bridgev mkinitramfs` (BusyBox + /init, see
#                src/system/machine.rs)
# kernel.org and GitHub are blocked by the container's network policy; the Ubuntu ports archive
# is reachable. Package checksums are pinned below. Nothing here is committed to git.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
OUT="$ROOT/guest/build/linux"
PORTS=http://ports.ubuntu.com/ubuntu-ports/pool/main
KDEB=linux-image-6.8.0-60-generic_6.8.0-60.63.1_riscv64.deb
KSHA=76fed85a231f9605b75f0efc0d6ade95c9fa7cc43c17186d8434971a6ba363ca
BDEB=busybox-static_1.36.1-6ubuntu3.1_riscv64.deb
BSHA=71cc910853f67ee183fa3c188583a39bee5df4dbfe31bd0bd6958458f0d73990
mkdir -p "$OUT/deb"
fetch() { # <url> <file> <sha256>
  if ! echo "$3  $OUT/deb/$2" | sha256sum -c --quiet >/dev/null 2>&1; then
    curl -fsS --retry 3 -o "$OUT/deb/$2" "$1/$2"
    echo "$3  $OUT/deb/$2" | sha256sum -c --quiet
  fi
}
fetch "$PORTS/l/linux-riscv" "$KDEB" "$KSHA"
fetch "$PORTS/b/busybox" "$BDEB" "$BSHA"
tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
dpkg-deb -x "$OUT/deb/$KDEB" "$tmp/k"
dpkg-deb -x "$OUT/deb/$BDEB" "$tmp/b"
cp "$tmp"/k/boot/vmlinuz-6.8.0-60-generic "$OUT/Image"
cp "$tmp"/b/usr/bin/busybox "$OUT/busybox"
BRIDGEV=${BRIDGEV:-$ROOT/target/release/bridgev}
[[ -x $BRIDGEV ]] || (cd "$ROOT" && cargo build --release -q)
"$BRIDGEV" mkinitramfs --busybox "$OUT/busybox" --out "$OUT/rootfs.cpio"
(cd "$OUT" && sha256sum Image busybox rootfs.cpio > SHA256SUMS)
echo "fetch-guest-images: $(cd "$OUT" && ls Image busybox rootfs.cpio | tr '\n' ' ')in ${OUT#"$ROOT"/}"
