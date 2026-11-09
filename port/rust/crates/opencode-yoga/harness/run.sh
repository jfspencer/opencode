#!/usr/bin/env bash
# The differential check of opencode-yoga against OpenTUI 0.8.1's own Yoga (see harness/oracle.ts): runs the crate's
# unit tests, has the oracle generate and run the programs through the reference's libopentui.so, replays them
# through the Rust crate (examples/yoga-driver.rs), and compares every answer, and what Yoga's default logger
# printed. Work files go to port/rust/target/opencode-yoga.
#
# usage: port/rust/crates/opencode-yoga/harness/run.sh [--count N] [--seed S] [--show N]
set -euo pipefail
crate=$(cd "$(dirname "$0")/.." && pwd)
port=$(cd "$crate/../../.." && pwd)
work="$port/rust/target/opencode-yoga"
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

cargo test -q --manifest-path "$manifest" -p opencode-yoga >"$work/unit-tests.log" 2>&1 || {
  cat "$work/unit-tests.log" >&2
  exit 1
}
cargo build -q --release --manifest-path "$manifest" -p opencode-yoga --example yoga-driver

bun "$crate/harness/oracle.ts" --programs "$work/programs.txt" --results "$work/ts.txt" --count "$count" \
  --seed "$seed" >"$work/ts-stdout.txt"
"$port/rust/target/release/examples/yoga-driver" "$work/programs.txt" "$work/rust.txt" >"$work/rust-stdout.txt"
exec bun "$crate/harness/compare.ts" --work "$work" --show "$show"
