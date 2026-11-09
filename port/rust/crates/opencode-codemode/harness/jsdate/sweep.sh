#!/usr/bin/env bash
# Runs the jsdate differential check over every canonical zone in src/jsdate/zone_names.rs at a small scale and
# prints one summary line per zone (differences by kind). Zones with differences are usually ones whose rules
# changed between bun's ICU tz data and the system's tz database.
#
# usage: harness/jsdate/sweep.sh [scale]     (default 0.02, about 8,000 checked values per zone)
set -u
here="$(cd "$(dirname "$0")" && pwd)"
crate="$(cd "$here/../.." && pwd)"
scale="${1:-0.02}"
(cd "$crate/../.." && cargo build --offline --release -q -p opencode-codemode --example jsdate_check) || exit 2
checker="$crate/../../target/release/examples/jsdate_check"
zones=$(grep -o 'zone("[^"]*", "[^"]*"' "$crate/src/jsdate/zone_names.rs" | sed 's/.*, "\(.*\)"/\1/' | sort -u)
for zone in $zones; do
  summary=$(TZ="$zone" bun "$here/oracle.ts" "$scale" 2>/dev/null | TZ="$zone" "$checker" 0 | sed -nE \
    -e '/^jsdate_check/!s/^ *(.*): +[0-9]+ checked, +([1-9][0-9]*) different$/\1=\2/p' \
    -e 's/^jsdate_check [^:]*: ([0-9]+) checked, ([0-9]+) different$/total \1 checked, \2 different/p' | tac | paste -sd' ')
  echo "$zone: $summary"
done
