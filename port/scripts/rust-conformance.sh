#!/usr/bin/env bash
# Runs the conformance standard against the Rust build and compares the scenarios that pass with the ledger
# (port/conformance/rust-passing.txt): a ledgered scenario that fails is a regression (exit 1); scenarios that pass
# but are not ledgered yet are listed, and --update writes them in.
#
# usage: port/scripts/rust-conformance.sh [--update] [--release] [shards=6] [scenario files...]
#   files are relative to port/conformance (default: every server scenario file)
# The binary is built with the oracle's build identity (OPENCODE_VERSION/OPENCODE_CHANNEL=conformance, see
# port/conformance/lib/server.ts). CONFORMANCE_MEMORY_LOG and CONFORMANCE_ACTUAL pass through to the run.
set -uo pipefail
# comm and sort must agree with Python's code-point order.
export LC_ALL=C
root=$(git rev-parse --show-toplevel)
here=$root/port/conformance
ledger=$here/rust-passing.txt
update=0
profile=debug
while [ $# -gt 0 ]; do
  case "$1" in
  --update) update=1 ;;
  --release) profile=release ;;
  *) break ;;
  esac
  shift
done
shards=6
if [[ "${1:-}" =~ ^[0-9]+$ ]]; then shards=$1; shift; fi

flags=()
[ "$profile" = release ] && flags=(--release)
(cd "$root/port/rust" && OPENCODE_VERSION=conformance OPENCODE_CHANNEL=conformance cargo build --quiet "${flags[@]}" -p opencode) ||
  exit 1
binary=$root/port/rust/target/$profile/opencode

files=("$@")
[ ${#files[@]} -eq 0 ] && files=($(cd "$here" && ls scenarios/*.test.ts))
log=$(mktemp -d)
pids=()
for k in $(seq 0 $((shards - 1))); do
  shard=()
  for i in "${!files[@]}"; do [ $((i % shards)) -eq "$k" ] && shard+=("${files[$i]}"); done
  [ ${#shard[@]} -eq 0 ] && continue
  (cd "$here" && CONFORMANCE_IMPL=$binary CONFORMANCE_STRICT=1 bun test --reporter=junit \
    --reporter-outfile="$log/shard-$k.xml" "${shard[@]}" >"$log/shard-$k.log" 2>&1) &
  pids+=($!)
done
wait "${pids[@]}"

# "<file> › <scenario>" (file relative to port/conformance) for each result of a kind, from the JUnit reports.
results() { # pass|fail|skip
  python3 -I - "$1" "$here" "$log"/shard-*.xml <<'PY'
import sys, os, xml.etree.ElementTree as ET
want, here, reports = sys.argv[1], sys.argv[2], sys.argv[3:]
out = set()
for report in reports:
    try:
        tree = ET.parse(report)
    except Exception:
        continue
    for case in tree.iter("testcase"):
        kind = "fail" if case.find("failure") is not None or case.find("error") is not None else "skip" if case.find("skipped") is not None else "pass"
        if kind == want:
            out.add(f"{os.path.relpath(case.get('file'), here)} › {case.get('name')}")
if out:
    print("\n".join(sorted(out)))
PY
}
results pass >"$log/pass.txt"
results fail >"$log/fail.txt"
pass=$(wc -l <"$log/pass.txt")
fail=$(wc -l <"$log/fail.txt")
skip=$(results skip | wc -l)
echo "rust: $pass pass, $fail fail, $skip skip (logs: $log)"

# Only the files that ran are compared.
ran=$(printf '%s\n' "${files[@]}" | sort -u)
touch "$ledger"
expected=$(grep -F -f <(printf '%s ›\n' $ran) "$ledger" | sort -u)
regressed=$(comm -23 <(printf '%s\n' "$expected" | sed '/^$/d') "$log/pass.txt")
added=$(comm -13 <(printf '%s\n' "$expected" | sed '/^$/d') "$log/pass.txt")
if [ -n "$added" ]; then
  echo "passing, not in the ledger yet ($(printf '%s\n' "$added" | wc -l)):"
  printf '%s\n' "$added" | head -50 | sed 's/^/  /'
fi
if [ "$update" = 1 ]; then
  { grep -v -F -f <(printf '%s ›\n' $ran) "$ledger"; cat "$log/pass.txt"; } | sed '/^$/d' | sort -u >"$ledger.tmp"
  mv "$ledger.tmp" "$ledger"
  echo "ledger updated: $(wc -l <"$ledger") scenarios"
fi
if [ -n "$regressed" ]; then
  echo "REGRESSED: in the ledger but failing ($(printf '%s\n' "$regressed" | wc -l)):"
  printf '%s\n' "$regressed" | sed 's/^/  /'
  exit 1
fi
