#!/usr/bin/env bash
# Differential check of `inventory::jsonc::set_root_property` against the reference's jsonc-parser 3.3.1: generated
# JSONC documents (every layout: line breaks, indentation, comments, trailing commas; their mutations; hand-picked edge
# cases) and root property edits (set and remove, other keys and values too), through `applyEdits(text, modify(...))`
# with the options `Config.update` passes. Zero differences are required.
#
# usage: port/rust/crates/opencode/harness/jsonc/run.sh [cases=20000] [seed=1]
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
packages=$(cd "$here/../../../../../.reference/packages" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
bun "$here/jsonccases.ts" "$packages" "${2:-1}" "${1:-20000}" >"$work/jsonc.json"
cd "$here/../.."
JSONC_CASES=$work/jsonc.json cargo test --quiet -p opencode recorded_edits -- --ignored --nocapture
