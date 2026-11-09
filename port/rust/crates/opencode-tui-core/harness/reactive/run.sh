#!/usr/bin/env bash
# The differential check of the reactive layer (src/reactive) against solid-js 1.9.17: random scripts (gen.ts) and
# the hand-written ones in cases/ run on both, and their logs (effect order, batching, cleanup timing, list rows)
# must be identical. Work files go to port/rust/target/opencode-tui-core/reactive.
#
# usage: port/rust/crates/opencode-tui-core/harness/reactive/run.sh [count=2000] [seed=1]
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
port=$(cd "$here/../../../../.." && pwd)
work="$port/rust/target/opencode-tui-core/reactive"
count=${1:-2000}
seed=${2:-1}
export OPENCODE_REFERENCE="$port/.reference"
rm -rf "$work"
mkdir -p "$work/scripts"
bun "$here/gen.ts" "$work/scripts" "$count" "$seed"
cp "$here"/cases/*.txt "$work/scripts/" 2>/dev/null || true
cargo build -q --manifest-path "$port/rust/Cargo.toml" -p opencode-tui-core --example reactive-script
"$port/rust/target/debug/examples/reactive-script" "$work"/scripts/*.txt
# solid-js, one process per script (its module state is per process).
ls "$work"/scripts/*.txt | xargs -P 4 -I{} sh -c 'bun "$0" "$1" > "$1.solid"' "$here/solid.ts" {}
diffs=0
for script in "$work"/scripts/*.txt; do
  if ! cmp -s "$script.solid" "$script.rust"; then
    diffs=$((diffs + 1))
    [ "$diffs" -le 3 ] && { echo "== $script"; diff "$script.solid" "$script.rust" | head -20; }
  fi
done
total=$(ls "$work"/scripts/*.txt | wc -l)
echo "reactive: $((total - diffs))/$total scripts identical"
[ "$diffs" -eq 0 ]
