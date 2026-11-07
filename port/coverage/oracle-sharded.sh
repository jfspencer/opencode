#!/usr/bin/env bash
# Run every conformance scenario (server + TUI) with coverage, split across parallel shards.
# usage: port/coverage/oracle-sharded.sh [label=oracle] [shards=8]
# Prints the union label to pass to report.py / uncovered.py (e.g. oracle0+oracle1+...).
set -uo pipefail
root=$(git rev-parse --show-toplevel)
base=${1:-oracle}
shards=${2:-8}
cd "$root/port/conformance"
files=$( (ls scenarios/*.test.ts; ls tui/*.test.ts) )
pids=()
for k in $(seq 0 $((shards - 1))); do
  mine=$(echo "$files" | awk -v n="$shards" -v k="$k" 'NR % n == k' | tr '\n' ' ')
  LABEL="$base$k" "$root/port/coverage/oracle.sh" $mine &
  pids+=($!)
done
wait "${pids[@]}"
echo "union label: $(seq 0 $((shards - 1)) | sed "s/^/$base/" | paste -sd+)"
