#!/usr/bin/env bash
# The differential check of the parser port against acorn: generates the corpus (or takes one), dumps it with acorn
# under bun and with the Rust port, and reports the differences (count, then the first few).
#
# usage: harness/parser/check.sh [corpus.jsonl]
#   WORK=dir   where the corpus and dumps go, kept afterwards (default: a fresh directory under $TMPDIR, removed when
#              there are no differences; the dumps of the default corpus take about 500 MB)
#   SHOWN=n    how many differences to print (default 10)
#   REAL=1     generate the corpus with a larger sample of real-world code (gen-corpus.ts --real)
set -euo pipefail
crate="$(cd "$(dirname "$0")/../.." && pwd)"
keep="${WORK:+1}"
work="${WORK:-$(mktemp -d "${TMPDIR:-/tmp}/parser-check.XXXXXX")}"
mkdir -p "$work"
cd "$crate"

corpus="${1:-$work/corpus.jsonl}"
if [ -z "${1:-}" ]; then
  bun harness/parser/gen-corpus.ts ${REAL:+--real} > "$corpus"
fi
cargo build --offline --release -q -p opencode-codemode --example parse_dump
bun harness/parser/acorn-dump.ts "$corpus" > "$work/acorn.jsonl"
../../target/release/examples/parse_dump "$corpus" > "$work/rust.jsonl"
if bun harness/parser/compare.ts "$corpus" "$work/acorn.jsonl" "$work/rust.jsonl" "${SHOWN:-10}"; then
  [ -n "$keep" ] || rm -rf "$work"
else
  echo "outputs kept in $work"
  exit 1
fi
