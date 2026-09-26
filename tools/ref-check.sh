#!/usr/bin/env bash
# Check every built guest program against tests/data/expected/ under qemu-riscv64 (P0.6).
#   tools/ref-check.sh           compare all variants against the expected files
#   tools/ref-check.sh --update  regenerate the expected files (from the -O2 rv64gc builds)
# Expected files: <name>.out (stdout), <name>.code (exit status), optional <name>.args.
# Assembly programs use the prefix "asm-".
set -uo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
BUILD="$ROOT/guest/build"
EXP="$ROOT/tests/data/expected"
update=0; [[ ${1:-} == --update ]] && update=1
mkdir -p "$EXP"
pass=0; fail=0
TMP=$(mktemp); trap 'rm -f "$TMP"' EXIT

check() {  # check <expected-name> <elf>
  local key=$1 elf=$2 args=() code
  [[ -f "$EXP/$key.args" ]] && read -r -a args < "$EXP/$key.args"
  "$ROOT/tools/ref-run.sh" "$elf" "${args[@]}" > "$TMP"; code=$?
  if ((update)); then
    cp "$TMP" "$EXP/$key.out"; echo "$code" > "$EXP/$key.code"; return
  fi
  # Byte-exact stdout comparison (trailing newlines matter).
  if cmp -s "$TMP" "$EXP/$key.out" && [[ "$code" == "$(cat "$EXP/$key.code")" ]]; then
    pass=$((pass + 1))
  else
    fail=$((fail + 1)); echo "FAIL: ${elf#"$ROOT"/} (exit $code)"
  fi
}

for src in "$ROOT"/guest/asm/*.S; do
  name=$(basename "$src" .S); check "asm-$name" "$BUILD/$name.elf"
done
for src in "$ROOT"/guest/c/*.c; do
  name=$(basename "$src" .c)
  if ((update)); then check "$name" "$BUILD/$name-O2.elf"; continue; fi
  for v in O0 O2 O0-nc O2-nc; do check "$name" "$BUILD/$name-$v.elf"; done
done

if ((update)); then echo "ref-check: expected files regenerated in ${EXP#"$ROOT"/}"; exit 0; fi
echo "ref-check: $pass passed, $fail failed"
((fail == 0))
