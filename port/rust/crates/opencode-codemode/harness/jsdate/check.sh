#!/usr/bin/env bash
# Differential check of opencode_codemode::jsdate against bun: for each time zone, oracle.ts generates the cases
# with bun's JavaScriptCore and examples/jsdate_check.rs recomputes them. UTC (what the conformance suite uses) runs
# first and must have no difference; the other zones are reported.
#
# usage: harness/jsdate/check.sh [scale] [zone...]     (default scale 1 = about 420,000 checked values per zone)
# SEED=n picks another case stream than the default one. The zone "unset" runs without TZ (the system zone).
set -u
here="$(cd "$(dirname "$0")" && pwd)"
crate="$(cd "$here/../.." && pwd)"
scale="${1:-1}"
shift || true
seed="${SEED:-1592646270}"
zones=("$@")
if [ ${#zones[@]} -eq 0 ]; then
  zones=(UTC America/New_York Europe/Berlin Asia/Kolkata Australia/Lord_Howe Etc/GMT-14 Europe/Dublin America/Denver)
fi

(cd "$crate/../.." && cargo build --offline --release -q -p opencode-codemode --example jsdate_check) || exit 2
checker="$crate/../../target/release/examples/jsdate_check"

status=0
for zone in "${zones[@]}"; do
  if [ "$zone" = unset ]; then
    # The system zone (/etc/localtime), as when TZ is not set.
    echo "== TZ unset"
    env -u TZ bun "$here/oracle.ts" "$scale" "$seed" | env -u TZ "$checker" 10
  else
    echo "== TZ=$zone"
    TZ="$zone" bun "$here/oracle.ts" "$scale" "$seed" | TZ="$zone" "$checker" 10
  fi
  result=("${PIPESTATUS[@]}")
  if [ "${result[0]}" -ne 0 ]; then
    echo "oracle failed under TZ=$zone"
    status=2
  elif [ "${result[1]}" -ne 0 ] && [ "$zone" = UTC ]; then
    status=1
  fi
done
exit $status
