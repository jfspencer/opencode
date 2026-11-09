#!/usr/bin/env bash
# Differential check of the file tools against the TS reference: jsdiff's fileDiff (tool/file_diff.rs) and edit.ts's
# matching, replacement and miss hints (tool/edit.rs), on random cases recorded from the reference.
#
# usage: port/rust/crates/opencode/harness/file-tools/run.sh [cases=10000] [seed=1]
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
reference=$here/../../../../../.reference/packages/core/src/tool/plugin/edit.ts
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cp "$here/editcases.ts" "$here/diffcases.ts" "$work/"
# The pure functions of edit.ts, from `crlf` to the last helper before the plugin.
{ awk '/^const crlf = /{p=1} /^\/\*\* Deferred edit behavior/{p=0} p' "$reference"; echo "export { findOccurrences, matchView, missHint }"; } >"$work/edit-core-mod.ts"
sed -i "s#\.\./\.\./\.\./\.\./\.\./\.reference#$here/../../../../../.reference#" "$work/diffcases.ts"
bun "$work/editcases.ts" "${2:-1}" "${1:-10000}" >"$work/edit.json"
bun "$work/diffcases.ts" "${2:-1}" "${1:-10000}" >"$work/diff.json"
cd "$here/../.."
EDIT_CASES=$work/edit.json DIFF_CASES=$work/diff.json cargo test --quiet -p opencode recorded -- --ignored
