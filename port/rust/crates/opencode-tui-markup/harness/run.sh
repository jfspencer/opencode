#!/usr/bin/env bash
# The differential check of opencode-tui-markup against the TS reference (@opentui/core 0.8.1 and its marked in
# port/.reference/node_modules): runs the crate's unit tests, then compares TS and Rust on every corpus
# (harness/diff.ts). Work files go to port/rust/target/opencode-tui-markup.
#
# usage: port/rust/crates/opencode-tui-markup/harness/run.sh [--count N] [--seed S] [--show N] [--only <corpus>]
set -euo pipefail
crate=$(cd "$(dirname "$0")/.." && pwd)
port=$(cd "$crate/../../.." && pwd)
reference="$port/.reference"
work="$port/rust/target/opencode-tui-markup"
manifest="$port/rust/Cargo.toml"
[ -d "$reference/node_modules/.bun" ] || {
  echo "no TS reference at $reference (run port/scripts/worktree-setup.sh in a worktree)" >&2
  exit 1
}
mkdir -p "$work/wasm"
rm -rf "$work/failures"

# The grammar WASMs the TUI downloads at run time (grammars/SOURCES.txt), checked against harness/wasm.sha256.
grep -v '^#' "$crate/grammars/SOURCES.txt" | while read -r _ _ _ _ _ _ _ wasm; do
  case "$wasm" in http*) ;; *) continue ;; esac
  file="$work/wasm/${wasm##*/}"
  [ -s "$file" ] || curl -sSfL --retry 3 -o "$file" "$wasm"
done
(cd "$work/wasm" && sha256sum --quiet -c "$crate/harness/wasm.sha256")

(cd "$crate" && bun harness/gen-wctype.ts check)
cargo test -q --manifest-path "$manifest" -p opencode-tui-markup >"$work/unit-tests.log" 2>&1 || {
  cat "$work/unit-tests.log" >&2
  exit 1
}
cargo build -q --release --manifest-path "$manifest" -p opencode-tui-markup --example markup-driver
cd "$crate"
exec bun harness/diff.ts --driver "$port/rust/target/release/examples/markup-driver" --work "$work" "$@"
