#!/usr/bin/env bash
# The differential check of opencode-html against the TS reference (see src/lib.rs): checks the generated entity
# table, runs the crate's unit tests, records the inputs of the upstream converter tests, then compares the TS and
# Rust converters on the whole corpus. Work files go to port/rust/target/opencode-html.
#
# usage: port/rust/crates/opencode-html/harness/run.sh [--coverage] [--count N] [--seed S] [--show N]
#   --coverage  build the driver with LLVM coverage (cargo-llvm-cov) and report the lines of the crate the corpus
#               reached; the line report is written to port/rust/target/opencode-html/coverage.txt
set -euo pipefail
crate=$(cd "$(dirname "$0")/.." && pwd)
port=$(cd "$crate/../../.." && pwd)
reference="$port/.reference"
work="$port/rust/target/opencode-html"
manifest="$port/rust/Cargo.toml"
coverage=
if [ "${1:-}" = "--coverage" ]; then
  coverage=1
  shift
fi
[ -d "$reference/packages/core" ] || {
  echo "no TS reference at $reference (run port/scripts/worktree-setup.sh in a worktree)" >&2
  exit 1
}
rm -rf "$work"
mkdir -p "$work"

(cd "$crate" && bun harness/gen-entities.ts check)
cargo test -q --manifest-path "$manifest" -p opencode-html >"$work/unit-tests.log" 2>&1 || {
  cat "$work/unit-tests.log" >&2
  exit 1
}

# The upstream tests of the converters, run as they are, recording each document the parser is given.
(
  cd "$reference/packages/core"
  OPENCODE_HTML_RECORD="$work/upstream.jsonl" OPENCODE_HTML_REFERENCE="$reference" \
    bun test --preload "$crate/harness/record-upstream.ts" test/tool-webfetch.test.ts -t "WebFetchTool helpers"
) >"$work/upstream-tests.log" 2>&1 || {
  echo "the upstream tests failed: $work/upstream-tests.log" >&2
  exit 1
}

if [ -z "$coverage" ]; then
  cargo build -q --release --manifest-path "$manifest" -p opencode-html --example html-driver
  exec bun "$crate/harness/diff.ts" --driver "$port/rust/target/release/examples/html-driver" --work "$work" \
    --upstream "$work/upstream.jsonl" "$@"
fi

status=0
(
  cd "$port/rust"
  eval "$(cargo llvm-cov show-env --sh 2>/dev/null)"
  # Its own target directory: cargo's fingerprints do not see the coverage flags the wrapper adds.
  export CARGO_TARGET_DIR="$port/rust/target/opencode-html-coverage"
  export CARGO_LLVM_COV_TARGET_DIR="$CARGO_TARGET_DIR" CARGO_LLVM_COV_BUILD_DIR="$CARGO_TARGET_DIR"
  export LLVM_PROFILE_FILE="$CARGO_TARGET_DIR/opencode-html-%p-%22m.profraw"
  rm -f "$CARGO_TARGET_DIR"/*.profraw
  cargo build -q --release -p opencode-html --example html-driver
  bun "$crate/harness/diff.ts" --driver "$CARGO_TARGET_DIR/release/examples/html-driver" --work "$work" \
    --upstream "$work/upstream.jsonl" "$@" || status=$?
  ignore='examples/|entities_data'
  cargo llvm-cov report --release -p opencode-html --ignore-filename-regex "$ignore" --text --show-missing-lines \
    >"$work/coverage.txt" 2>/dev/null || true
  cargo llvm-cov report --release -p opencode-html --ignore-filename-regex "$ignore" --summary-only
  exit $status
)
