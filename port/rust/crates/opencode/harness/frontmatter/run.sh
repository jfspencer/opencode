#!/usr/bin/env bash
# Differential check of markdown frontmatter (inventory/yaml.rs, markdown.rs, frontmatter_js.rs, jsvalue.rs and the
# agent, command and skill decoding in agents.rs, commands.rs, skills.rs) against the reference: generated and
# hand-picked markdown files through gray-matter (`ConfigMarkdown.parse`: the data as a JS value graph, or that it
# throws) and through the config plugins' decoding (the agent, command and skill, or that the file is skipped, or that
# the plugin dies). Requires 0 differences.
#
# usage: port/rust/crates/opencode/harness/frontmatter/run.sh [cases=5000] [seed=1]
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
packages=$(cd "$here/../../../../../.reference/packages" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
export TZ=UTC
bun "$here/cases.ts" "$packages" "${2:-1}" "${1:-5000}" >"$work/cases.json"
cd "$here/../.."
FRONTMATTER_CASES=$work/cases.json cargo test --quiet -p opencode frontmatter_matches_the_reference -- --ignored --nocapture
