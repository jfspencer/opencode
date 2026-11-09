#!/usr/bin/env bash
# The differential check of opencode-tui's theme and config modules against the TS reference (src/theme, src/config):
# checks that the embedded theme assets are the reference's, runs the crate's unit tests, then compares TS and Rust on
# every built-in theme and on generated cases (see harness/diff.ts). Work files go to port/rust/target/opencode-tui.
#
# usage: port/rust/crates/opencode-tui/harness/run.sh [--count N] [--seed S] [--show N] [--only theme|config]
set -euo pipefail
crate=$(cd "$(dirname "$0")/.." && pwd)
port=$(cd "$crate/../../.." && pwd)
reference="$port/.reference"
work="$port/rust/target/opencode-tui"
manifest="$port/rust/Cargo.toml"
[ -d "$reference/packages/tui" ] || {
  echo "no TS reference at $reference (run port/scripts/worktree-setup.sh in a worktree)" >&2
  exit 1
}
mkdir -p "$work"

# The theme files are embedded from the crate; they must be the reference's.
for file in "$reference"/packages/tui/src/theme/assets/*.json "$reference"/packages/tui/src/theme/assets/v2/*.json; do
  relative=${file#"$reference"/packages/tui/src/theme/assets/}
  cmp -s "$file" "$crate/src/theme/assets/$relative" || {
    echo "theme asset differs from the reference: $relative (copy it from $file)" >&2
    exit 1
  }
done

cargo test -q --manifest-path "$manifest" -p opencode-tui >"$work/unit-tests.log" 2>&1 || {
  cat "$work/unit-tests.log" >&2
  exit 1
}
cargo build -q --manifest-path "$manifest" -p opencode-tui --example tui-driver
mkdir -p "$work/files"
# The TUI's config module has JSX: load it with the SolidJS transform, as the TUI is loaded.
preload=$(cd "$reference/packages/tui" && bun -e 'console.log(Bun.resolveSync("@opentui/solid/preload", process.cwd()))')
OPENCODE_TUI_REFERENCE="$reference" OPENCODE_TUI_WORK="$work/files" exec bun --preload "$preload" "$crate/harness/diff.ts" --driver "$port/rust/target/debug/examples/tui-driver" "$@"
