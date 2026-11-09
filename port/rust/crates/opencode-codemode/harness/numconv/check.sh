#!/usr/bin/env bash
# Differential check of src/numconv.rs against bun (JavaScriptCore + ICU).
#
#   harness/numconv/check.sh [count] [seed] [function...]
#
# For each function (default: all), oracle.ts prints the edge cases plus <count> random cases
# (default 1000000) generated from <seed> (default 1), and examples/numconv_check.rs recomputes them.
# The functions run in parallel; per-function reports land in a temporary directory under $TMPDIR,
# removed on exit. Exit status 1 when any case differs. NUMCONV_PROFILE=dev checks a debug build
# (overflow checks on); the default is release.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
crate="$(cd "$here/../.." && pwd)"
workspace="$(cd "$crate/../.." && pwd)"
count="${1:-1000000}"
seed="${2:-1}"
shift $(($# > 2 ? 2 : $#))
if [ $# -gt 0 ]; then
  fns=("$@")
else
  fns=(toString toStringRadix toFixed toExponential toPrecision toLocaleString stringToNumber parseInt parseFloat toInt32 toUint32 indexToString)
fi

profile="${NUMCONV_PROFILE:-release}"
(cd "$workspace" && cargo build --offline --profile "$profile" --quiet -p opencode-codemode --example numconv_check)
bin="${CARGO_TARGET_DIR:-$workspace/target}/$([ "$profile" = dev ] && echo debug || echo "$profile")/examples/numconv_check"

out="$(mktemp -d "${TMPDIR:-/tmp}/numconv-check.XXXXXX")"
trap 'rm -rf "$out"' EXIT

pids=()
for f in "${fns[@]}"; do
  (
    set +e
    bun "$here/oracle.ts" "$f" "$count" "$seed" | "$bin" >"$out/$f.txt" 2>&1
    echo "${PIPESTATUS[0]} ${PIPESTATUS[1]}" >"$out/$f.status"
  ) &
  pids+=($!)
done
for p in "${pids[@]}"; do wait "$p" || true; done

status=0
for f in "${fns[@]}"; do
  cat "$out/$f.txt"
  if [ "$(cat "$out/$f.status")" != "0 0" ]; then
    echo "$f: oracle/check exit statuses $(cat "$out/$f.status")"
    status=1
  fi
done
exit $status
