#!/usr/bin/env bash
# The differential check of opencode-shell against the TS reference (see ../README.md).
#
# usage: port/rust/crates/opencode-shell/harness/check.sh [driver options]
#   e.g. check.sh --cases 20000 --seed 7      a deeper run
#        check.sh --no-upstream --only parse  a quick one
# Needs the TS reference (port/scripts/reference.sh build, or worktree-setup.sh in a linked worktree).
set -euo pipefail
crate=$(cd "$(dirname "$0")/.." && pwd)
cd "$crate"
[ -d ../../../.reference/packages/core/node_modules ] || { echo "no TS reference: run port/scripts/reference.sh build" >&2; exit 1; }
bun harness/gen-unicode.ts --check
bun harness/driver.ts "$@"
