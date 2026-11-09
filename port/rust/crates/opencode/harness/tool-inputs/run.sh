#!/usr/bin/env bash
# Differential check of tool/file_input.rs against the TS reference: random inputs for the tools decoded there (and
# synthetic schemas with recursive $refs, array lengths and compositions) go through the reference's repair and
# tool/runtime.ts's decode, then through Rust; repaired values and "Invalid arguments" texts must be equal.
#
# usage: port/rust/crates/opencode/harness/tool-inputs/run.sh [cases per schema=2000] [seed=1]
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
core=$(cd "$here/../../../../../.reference/packages/core" && pwd)
effect=$(cd "$core" && bun -e 'console.log(Bun.resolveSync("effect", process.cwd()))')
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
# tool-input-repair.ts's own functions, from `decodeJson` to the end, without the plugin.
{
  echo "import { JsonSchema, Option, Predicate, Schema } from \"$effect\""
  awk '/^const decodeJson = /{p=1} /^\/\/ Tool names models carry over/{p=0} p' "$core/src/plugin/tool-input-repair.ts"
  awk '/^function repair\(/{p=1} p' "$core/src/plugin/tool-input-repair.ts"
  echo "export { repair }"
} >"$work/repair.ts"
(cd "$core" && bun "$here/cases.ts" "${2:-1}" "${1:-2000}" "$core" "$work/repair.ts") >"$work/cases.json"
# TOOL_INPUT_KEEP=<file> keeps the recorded cases.
[ -n "${TOOL_INPUT_KEEP:-}" ] && cp "$work/cases.json" "$TOOL_INPUT_KEEP"
cd "$here/../.."
TOOL_INPUT_CASES=$work/cases.json cargo test --quiet -p opencode recorded_inputs -- --ignored
