#!/usr/bin/env bash
# Run every scenario (server and TUI) N times against the recorded goldens, sharded across parallel bun
# processes, and list any scenario that failed in any run. A golden is only part of the standard once it
# reproduces on every run, including under the load of parallel shards.
#
# usage: port/conformance/determinism.sh [--record] [runs=3] [shards=6]
#   --record   re-record every golden first (UPDATE_GOLDENS=1), then verify
set -uo pipefail
here=$(cd "$(dirname "$0")" && pwd)
record=0
if [ "${1:-}" = "--record" ]; then record=1; shift; fi
runs=${1:-3}
shards=${2:-6}
log=$(mktemp -d)

# Files per suite, split round-robin into shards.
shard_files() { # dir pattern shard
  (cd "$1" && ls $2 2>/dev/null) | awk -v n="$shards" -v k="$3" 'NR % n == k'
}

pass() { # label env...
  local label=$1; shift
  local pids=()
  for k in $(seq 0 $((shards - 1))); do
    s=$(shard_files "$here" "scenarios/*.test.ts" "$k" | tr '\n' ' ')
    [ -n "$s" ] && (cd "$here" && env "$@" bun test $s >"$log/$label-server-$k.log" 2>&1) & pids+=($!)
    t=$(shard_files "$here/tui" "*.test.ts" "$k" | tr '\n' ' ')
    [ -n "$t" ] && (cd "$here/tui" && env "$@" bun test $t >"$log/$label-tui-$k.log" 2>&1) & pids+=($!)
  done
  wait "${pids[@]}"
  local p f
  p=$(cat "$log/$label"-*.log | grep -E '^ *[0-9]+ pass' | awk '{s+=$1} END {print s+0}')
  f=$(cat "$log/$label"-*.log | grep -E '^ *[0-9]+ fail' | awk '{s+=$1} END {print s+0}')
  echo "$label: $p pass, $f fail"
}

if [ "$record" = 1 ]; then pass record UPDATE_GOLDENS=1; fi
for i in $(seq 1 "$runs"); do pass "run$i" CONFORMANCE_STRICT=1; done

failures=$(grep -hE '^\(fail\)' "$log"/run*.log | sed -E 's/ \[[0-9.]+m?s\]$//' | sort | uniq -c | sort -rn)
if [ -n "$failures" ]; then
  echo "scenarios that failed at least once (count of $runs runs):"
  echo "$failures"
  echo "logs: $log"
  exit 1
fi
echo "all scenarios reproduced on every run (logs: $log)"
