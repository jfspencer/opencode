#!/usr/bin/env bash
# Generates the Rust wire types (port/rust/crates/opencode-protocol/src/generated/) from the TS reference's Effect
# schemas, and proves them against TS; also the database bootstrap and the OpenAPI document
# (port/rust/crates/opencode/src/generated/).
#
# usage: port/scripts/gen-types.sh               regenerate (after an upstream merge or a patch change)
#        port/scripts/gen-types.sh check [opts]  fail unless the checked-in code is what the reference generates;
#                                                cargo build + unit tests; property round-trip against TS
#   opts go to the round-trip: --samples N (values per root type, default 12) --mutants N (per value, default 2)
#   --seed N --only RootType --verbose
# The reference must be current: port/scripts/reference.sh build.
set -euo pipefail
root=$(git rev-parse --show-toplevel)
gen=$root/port/rust/gen
ref=${REFERENCE:-$root/port/.reference}
[ -d "$ref/packages/protocol/node_modules/effect" ] || { echo "no TS reference at $ref: run port/scripts/reference.sh build" >&2; exit 1; }

# The reference must be built from this checkout's upstream code, or the generated types describe another API.
base_file="$(git -C "$ref" rev-parse --absolute-git-dir)/port-base"
if [ -f "$base_file" ] && ! git -C "$root" diff --quiet "$(cat "$base_file")" HEAD -- . ':(exclude)port'; then
  echo "the TS reference is older than this checkout's upstream code: run port/scripts/reference.sh build" >&2
  exit 1
fi

# Load the reference's own module instances (effect, @opencode/schema, @opencode/protocol), like link-deps.sh.
mkdir -p "$gen/node_modules/@opencode"
ln -sfn "$(realpath --relative-to="$gen/node_modules" "$ref/packages/protocol/node_modules/effect")" "$gen/node_modules/effect"
for p in schema protocol; do
  ln -sfn "$(realpath --relative-to="$gen/node_modules/@opencode" "$ref/packages/$p")" "$gen/node_modules/@opencode/$p"
done

case "${1:-generate}" in
generate)
  (cd "$gen" && bun src/generate.ts && bun src/database.ts && bun src/mime.ts && REFERENCE=$ref bun src/openapi.ts)
  ;;
check)
  shift
  start=$(date +%s.%N)
  (cd "$gen" && bun src/generate.ts --check && bun src/database.ts --check && bun src/mime.ts --check && REFERENCE=$ref bun src/openapi.ts --check)
  (cd "$root/port/rust" && cargo build --locked --quiet && cargo test --locked --quiet -p opencode-protocol 2>&1 | grep -E "^test result" | head -1)
  (cd "$gen" && bun src/roundtrip.ts "$@")
  echo "gen-types check: ok ($(printf '%.1f' "$(echo "$(date +%s.%N) - $start" | bc)")s)"
  ;;
*)
  sed -n '2,10p' "$0" | sed 's/^# \{0,1\}//'
  exit 1
  ;;
esac
