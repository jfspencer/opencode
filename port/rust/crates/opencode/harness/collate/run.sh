#!/usr/bin/env bash
# The differential check of src/collate.rs (`localeCompare`): regenerates ICU's dump and checks the checked-in table
# against it (gen/src/collation.ts --check, which needs a C compiler and libicu-dev of Bun's ICU major version),
# builds the Rust driver (collate.rs compiled on its own), then compares (check.ts):
#   - the collation elements of every code point, contraction and prefix mapping with ICU's, and those of every
#     generated string below with ICU's CollationElementIterator (the iteration ucol_strcoll runs);
#   - the order with Bun's `localeCompare`: every code point on its own sorted (and each neighbour pair both ways),
#     generated pairs (mixed scripts, combining marks, contractions, mutations: marks inserted, case and
#     normalization changes, swaps), pairs within each pool (Han of every block, compatibility ideographs, radicals,
#     Hangul and jamo, Thai and Lao, Tibetan, kana, emoji, selectors, controls, unassigned and noncharacters, ...),
#     contractions with marks inside, ASCII (JavaScriptCore's fast path), sorted samples and hand-picked strings.
# Work files go to port/rust/target/collate.
#
# usage: port/rust/crates/opencode/harness/collate/run.sh [--count N] [--seed S] [--show N]
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
rust=$(cd "$here/../../../.." && pwd)
work="$rust/target/collate"
mkdir -p "$work"
(cd "$rust/gen" && bun src/collation.ts --check --dump "$work/dump.txt")
cc -O2 -o "$work/icu-dump" "$rust/gen/src/collation-dump.c" -licui18n -licuuc -licudata
rustc --edition 2024 -O -o "$work/driver" "$here/driver.rs"
exec bun "$here/check.ts" --driver "$work/driver" --icu "$work/icu-dump" --dump "$work/dump.txt" --work "$work" "$@"
