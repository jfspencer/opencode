#!/usr/bin/env bash
# The differential check of opencode-tui-input against the reference's @opentui/core and @opentui/keymap 0.8.1: runs
# the crate's unit tests; has harness/oracle.ts generate and run the stdin programs through the TS StdinParser on a
# manual clock and harness/keymap-oracle.ts the keymap programs through the TS Keymap; replays both through the Rust
# crate (examples/input-driver.rs, examples/keymap-driver.rs); and compares every answer. Work files go to
# port/rust/target/opencode-tui-input.
#
# usage: port/rust/crates/opencode-tui-input/harness/run.sh [--count N] [--seed S] [--show N]
set -euo pipefail
crate=$(cd "$(dirname "$0")/.." && pwd)
port=$(cd "$crate/../../.." && pwd)
work="$port/rust/target/opencode-tui-input"
manifest="$port/rust/Cargo.toml"
count=20000
seed=1
show=3
while [ $# -gt 0 ]; do
  case "$1" in
    --count) count=$2; shift 2 ;;
    --seed) seed=$2; shift 2 ;;
    --show) show=$2; shift 2 ;;
    *) echo "unknown option $1" >&2; exit 2 ;;
  esac
done
[ -d "$port/.reference/packages/tui" ] || {
  echo "no TS reference at $port/.reference (run port/scripts/worktree-setup.sh in a worktree)" >&2
  exit 1
}
rm -rf "$work"
mkdir -p "$work"

cargo test -q --manifest-path "$manifest" -p opencode-tui-input >"$work/unit-tests.log" 2>&1 || {
  cat "$work/unit-tests.log" >&2
  exit 1
}
cargo build -q --release --manifest-path "$manifest" -p opencode-tui-input --example input-driver --example keymap-driver

status=0
bun "$crate/harness/oracle.ts" --programs "$work/stdin-programs.txt" --results "$work/stdin-ts.txt" --count "$count" \
  --seed "$seed"
"$port/rust/target/release/examples/input-driver" "$work/stdin-programs.txt" "$work/stdin-rust.txt"
bun "$crate/harness/compare.ts" --work "$work" --name stdin- --show "$show" || status=1

bun "$crate/harness/keymap-oracle.ts" --programs "$work/keymap-programs.txt" --results "$work/keymap-ts.txt" \
  --count "$count" --seed "$seed"
"$port/rust/target/release/examples/keymap-driver" "$work/keymap-programs.txt" "$work/keymap-rust.txt"
bun "$crate/harness/compare.ts" --work "$work" --name keymap- --show "$show" || status=1
exit $status
