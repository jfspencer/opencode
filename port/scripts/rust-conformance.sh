#!/usr/bin/env bash
# Runs the conformance standard against the Rust build and compares the scenarios that pass with the ledger
# (port/conformance/rust-passing.txt): a ledgered scenario that fails is a regression (exit 1); scenarios that pass
# but are not ledgered yet are listed, and --update writes them in.
#
# usage: port/scripts/rust-conformance.sh [--update] [--release] [shards=6] [scenario files...]
#   files are relative to port/conformance (default: every server scenario file); `tui/<file>` runs a TUI scenario
#   file from port/conformance/tui (its bunfig.toml adds the SolidJS transform) against `<binary> --server <url>`
#   in a pseudo-terminal, and `tui` alone stands for every TUI file. Ledger lines are `<file> › <scenario>` either
#   way (`tui/home.test.ts › ...`); in-process-only TUI scenarios are skipped, as the harness skips them.
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
# The TUI crates build from fetched sources (gitignored port/.vendor): OpenTUI's (Yoga) and the tree-sitter grammars.
[ -d "$root/port/.vendor/opentui/packages/native/zig-deps/yoga" ] || "$root/port/scripts/vendor-opentui.sh" || exit 1
[ -d "$root/port/.vendor/tree-sitter" ] || "$root/port/scripts/vendor-grammars.sh" || exit 1
(cd "$root/port/rust" && OPENCODE_VERSION=conformance OPENCODE_CHANNEL=conformance cargo build --quiet "${flags[@]}" -p opencode) ||
  exit 1
binary=$root/port/rust/target/$profile/opencode

files=()
for file in "$@"; do
  if [ "${file%/}" = tui ]; then files+=($(cd "$here" && ls tui/*.test.ts)); else files+=("$file"); fi
done
[ ${#files[@]} -eq 0 ] && files=($(cd "$here" && ls scenarios/*.test.ts))
# The reports of the last run (on disk: /tmp is a small tmpfs the scenarios' sandboxes already fill).
log=$root/port/rust/target/conformance-logs
rm -rf "$log" && mkdir -p "$log"
pids=()
for k in $(seq 0 $((shards - 1))); do
  shard=()
  tui=()
  for i in "${!files[@]}"; do
    [ $((i % shards)) -eq "$k" ] || continue
    # A TUI file is given to bun as a path (./<file>): a bare name is a filter, and home.test.ts would also pick
    # nav-home.test.ts.
    case "${files[$i]}" in
    tui/*) tui+=("./${files[$i]#tui/}") ;;
    *) shard+=("${files[$i]}") ;;
    esac
  done
  [ ${#shard[@]} -eq 0 ] && [ ${#tui[@]} -eq 0 ] && continue
  (
    if [ ${#shard[@]} -gt 0 ]; then
      cd "$here" && CONFORMANCE_IMPL=$binary CONFORMANCE_STRICT=1 bun test --reporter=junit \
        --reporter-outfile="$log/shard-$k.xml" "${shard[@]}" >"$log/shard-$k.log" 2>&1
    fi
    if [ ${#tui[@]} -gt 0 ]; then
      cd "$here/tui" && CONFORMANCE_IMPL=$binary CONFORMANCE_STRICT=1 bun test --reporter=junit \
        --reporter-outfile="$log/shard-$k-tui.xml" "${tui[@]}" >"$log/shard-$k-tui.log" 2>&1
    fi
  ) &
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
  # Passes are added; nothing is removed, so a regression stays visible (a renamed or deleted scenario's line is
  # removed by hand, see "not run" below).
  { cat "$ledger"; cat "$log/pass.txt"; } | sed '/^$/d' | sort -u >"$ledger.tmp"
  mv "$ledger.tmp" "$ledger"
  echo "ledger updated: $(wc -l <"$ledger") scenarios"
fi
# Ledgered scenarios of the files that ran which neither passed, failed nor were skipped: renamed or deleted.
results skip >"$log/skip.txt"
stale=$(comm -23 <(printf '%s\n' "$expected" | sed '/^$/d') <(sort -u "$log/pass.txt" "$log/fail.txt" "$log/skip.txt"))
if [ -n "$stale" ]; then
  echo "in the ledger but not run (renamed or removed? edit the ledger by hand):"
  printf '%s\n' "$stale" | sed 's/^/  /'
fi
if [ -n "$regressed" ]; then
  echo "REGRESSED: in the ledger but failing ($(printf '%s\n' "$regressed" | wc -l)):"
  printf '%s\n' "$regressed" | sed 's/^/  /'
  exit 1
fi
