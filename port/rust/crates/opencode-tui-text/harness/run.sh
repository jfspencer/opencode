#!/usr/bin/env bash
# The differential check of opencode-tui-text against @opentui/core 0.8.1 (see src/lib.rs): checks the generated
# Unicode tables, runs the crate's unit tests, then runs the same operation programs on the TS classes (over the
# reference's libopentui.so) and on the crate (examples/text-driver), comparing every op's result. Work files go to
# port/rust/target/opencode-tui-text.
#
# usage: port/rust/crates/opencode-tui-text/harness/run.sh [--count N] [--seed S] [--show N] [--no-tables] [--case I]
set -euo pipefail
crate=$(cd "$(dirname "$0")/.." && pwd)
port=$(cd "$crate/../../.." && pwd)
reference="$port/.reference"
work="$port/rust/target/opencode-tui-text"
manifest="$port/rust/Cargo.toml"
[ -d "$reference/packages/tui/node_modules/@opentui/core" ] || {
  echo "no TS reference at $reference (run port/scripts/worktree-setup.sh in a worktree)" >&2
  exit 1
}
mkdir -p "$work"
(cd "$crate" && bun scripts/gen-unicode.ts check)
cargo test -q --manifest-path "$manifest" -p opencode-tui-text >"$work/unit-tests.log" 2>&1 || {
  cat "$work/unit-tests.log" >&2
  exit 1
}
cargo build -q --release --manifest-path "$manifest" -p opencode-tui-text --example text-driver
cd "$crate"
exec bun harness/diff.ts --driver "$port/rust/target/release/examples/text-driver" --work "$work" "$@"
