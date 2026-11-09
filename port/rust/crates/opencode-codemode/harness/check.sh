#!/usr/bin/env bash
# The differential check: every corpus under harness/corpus runs through the TS reference interpreter and through
# the Rust crate, and the JSON lines must be byte-identical.
#
# Every corpus's programs also go through `scanTools` on both sides (the `<name>.scan` lines).
#
# usage: harness/check.sh [corpus-name...] [--show N]     (run from anywhere; TZ is pinned to UTC)
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
crate=$(dirname "$here")
workspace=$(cd "$crate/../.." && pwd)
show=5
names=()
while [ $# -gt 0 ]; do
  case "$1" in
    --show) show="$2"; shift 2 ;;
    *) names+=("$1"); shift ;;
  esac
done
[ ${#names[@]} -gt 0 ] || names=($(cd "$here/corpus" && ls *.jsonl | sed 's/\.jsonl$//'))
export TZ=UTC
out=$(mktemp -d "${TMPDIR:-/tmp}/codemode-check.XXXXXX")
trap 'rm -rf "$out"' EXIT
(cd "$workspace" && cargo build --offline --release --example codemode_run -q)
runner="$workspace/target/release/examples/codemode_run"
status=0
for name in "${names[@]}"; do
  corpus="$here/corpus/$name.jsonl"
  bun "$here/run-ts.ts" "$corpus" > "$out/$name.ts.jsonl"
  "$runner" "$corpus" > "$out/$name.rust.jsonl"
  echo "== $name"
  bun "$here/compare.ts" "$out/$name.ts.jsonl" "$out/$name.rust.jsonl" --show "$show" || status=1
  bun "$here/run-ts.ts" "$corpus" --scan > "$out/$name.scan.ts.jsonl"
  "$runner" "$corpus" --scan > "$out/$name.scan.rust.jsonl"
  echo "== $name.scan"
  bun "$here/compare.ts" "$out/$name.scan.ts.jsonl" "$out/$name.scan.rust.jsonl" --show "$show" || status=1
done
exit $status
