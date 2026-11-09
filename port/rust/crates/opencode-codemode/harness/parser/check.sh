#!/usr/bin/env bash
# The differential check of the parser port against acorn: generates the corpus (or takes one), dumps it with acorn
# under bun and with the Rust port, and reports the differences (count, then the first few).
#
# usage: harness/parser/check.sh [corpus.jsonl]
#   WORK=dir   where the corpus and dumps go (default: a fresh directory under $TMPDIR)
#   SHOWN=n    how many differences to print (default 10)
set -euo pipefail
crate="$(cd "$(dirname "$0")/../.." && pwd)"
work="${WORK:-$(mktemp -d "${TMPDIR:-/tmp}/parser-check.XXXXXX")}"
mkdir -p "$work"
cd "$crate"

corpus="${1:-$work/corpus.jsonl}"
if [ -z "${1:-}" ]; then
  bun harness/parser/gen-corpus.ts > "$corpus"
fi
cargo build --offline --release -q -p opencode-codemode --example parse_dump
bun harness/parser/acorn-dump.ts "$corpus" > "$work/acorn.jsonl"
../../target/release/examples/parse_dump "$corpus" > "$work/rust.jsonl"
echo "outputs in $work"
bun harness/parser/compare.ts "$corpus" "$work/acorn.jsonl" "$work/rust.jsonl" "${SHOWN:-10}"
