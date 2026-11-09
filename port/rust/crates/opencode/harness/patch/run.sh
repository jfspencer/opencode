#!/usr/bin/env bash
# Differential check of the patch tool's format (tool/patch_text.rs) against the reference's @opencode/util/patch:
# generated patch texts (well-formed, mutated, and each parser branch's edge cases) through `Patch.parse` (the hunks,
# or the error text), and every update hunk through `Patch.derive` on generated files (the result and BOM, or the
# thrown message).
#
# usage: port/rust/crates/opencode/harness/patch/run.sh [cases=20000] [seed=1]
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
packages=$(cd "$here/../../../../../.reference/packages" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
bun "$here/patchcases.ts" "$packages" "${2:-1}" "${1:-20000}" >"$work/patch.json"
cd "$here/../.."
PATCH_CASES=$work/patch.json cargo test --quiet -p opencode recorded_patches -- --ignored --nocapture
