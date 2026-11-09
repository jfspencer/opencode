#!/usr/bin/env bash
# The RegExp differential check: bun (JavaScriptCore) generates and answers the cases, the Rust port recomputes
# them and reports differences. Arguments: a scale for the random parts (default 1: 671,451 cases; 3 gives about
# 1.6 million) and how many differences to print.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
workspace="$(cd "$here/../../../.." && pwd)"
scale="${1:-1}"
reports="${2:-40}"
cd "$workspace"
cargo build --release --offline --example jsregex_check --quiet
bun "$here/oracle.ts" "$scale" | "$workspace/target/release/examples/jsregex_check" "$reports"
