#!/usr/bin/env bash
# Differential check of the file tools against the TS reference, on random cases recorded from it:
# - jsdiff 8.0.4's fileDiff (tool/file_diff.rs);
# - edit.ts's matching, replacement and miss hints (tool/edit.rs), its pure functions extracted from the source;
# - ReadToolFileSystem.read's pages and errors in the memory environment (tool/file_read.rs): budgets, chunks, long
#   lines, CRLF, NUL bytes, BOMs.
#
# usage: port/rust/crates/opencode/harness/file-tools/run.sh [cases=10000] [seed=1] [read cases=200]
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
core=$(cd "$here/../../../../../.reference/packages/core" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cp "$here/editcases.ts" "$here/diffcases.ts" "$work/"
# The pure functions of edit.ts, from `crlf` to the last helper before the plugin.
{ awk '/^const crlf = /{p=1} /^\/\*\* Deferred edit behavior/{p=0} p' "$core/src/tool/plugin/edit.ts"; echo "export { findOccurrences, matchView, missHint }"; } >"$work/edit-core-mod.ts"
sed -i "s#\.\./\.\./\.\./\.\./\.\./\.reference/packages/core#$core#" "$work/diffcases.ts"
bun "$work/editcases.ts" "${2:-1}" "${1:-10000}" >"$work/edit.json"
bun "$work/diffcases.ts" "${2:-1}" "${1:-10000}" >"$work/diff.json"
(cd "$core" && bun "$here/readcases.ts" "${2:-1}" "${3:-200}" "$core") >"$work/read.json"
cd "$here/../.."
EDIT_CASES=$work/edit.json DIFF_CASES=$work/diff.json READ_CASES=$work/read.json cargo test --quiet -p opencode recorded -- --ignored
