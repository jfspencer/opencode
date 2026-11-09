#!/usr/bin/env bash
# Differential check of npm.rs against the reference's own libraries:
# 1. node-semver, npm-pick-manifest and npm-package-arg (npm/semver.rs, npm/pick.rs, npm/spec.rs) on generated and
#    hand-picked versions, ranges, specs and packuments (semvercases.ts);
# 2. whole installs (npm/tree.rs, npm/reify.rs): generated registries served on loopback, each package installed by the
#    reference's `Npm.which` and by the Rust port into fresh caches, then the trees compared file by file (treecases.ts).
#
# usage: port/rust/crates/opencode/harness/npm/run.sh [semver cases=20000] [seed=1] [install cases=200]
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
port=$(cd "$here/../../../../.." && pwd)
bun_dir="$port/.reference/node_modules/.bun"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
bun "$here/semvercases.ts" "$bun_dir" "${2:-1}" "${1:-20000}" >"$work/semver.json"
cd "$port/rust"
NPM_SEMVER_CASES=$work/semver.json cargo test --quiet -p opencode recorded_semver_cases -- --ignored --nocapture
if [ -f "$here/treecases.ts" ]; then
  binary=$(cargo test --quiet -p opencode --no-run --message-format=json 2>/dev/null |
    python3 -I -c 'import json,sys
for line in sys.stdin:
    m = json.loads(line)
    if m.get("reason") == "compiler-artifact" and m.get("executable") and m["target"]["name"] == "opencode" and m["profile"]["test"]:
        print(m["executable"])' | tail -1)
  bun "$here/treecases.ts" --driver "$binary" --reference "$port/.reference" --work "$work" --seed "${2:-1}" --count "${3:-200}"
fi
