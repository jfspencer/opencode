#!/usr/bin/env bash
# The differential check of opencode-tui-sync against the TS reference's @opencode/client (see src/lib.rs): checks
# the generated operation table, runs the crate's unit tests, then runs the TS driver (the reference's client and
# Solid data layer, under the TUI's Solid build) and the Rust driver on the same cases and compares their results.
# Work files go to port/rust/target/opencode-tui-sync.
#
# usage: port/rust/crates/opencode-tui-sync/harness/run.sh [--seed N] [--count N] [--only kind,...] [--show N]
#        [--cases extra.jsonl ...]
set -euo pipefail
crate=$(cd "$(dirname "$0")/.." && pwd)
port=$(cd "$crate/../../.." && pwd)
reference="$port/.reference"
work="$port/rust/target/opencode-tui-sync"
manifest="$port/rust/Cargo.toml"
[ -d "$reference/packages/client" ] || {
  echo "no TS reference at $reference (run port/scripts/worktree-setup.sh in a worktree)" >&2
  exit 1
}
mkdir -p "$work"
(cd "$crate" && bun harness/gen-client.ts check)
cargo test -q --manifest-path "$manifest" -p opencode-tui-sync --lib >"$work/unit-tests.log" 2>&1 || {
  cat "$work/unit-tests.log" >&2
  exit 1
}
cargo build -q --manifest-path "$manifest" -p opencode-tui-sync --example sync-driver
exec bun "$crate/harness/diff.ts" --driver "$port/rust/target/debug/examples/sync-driver" --work "$work" "$@"
