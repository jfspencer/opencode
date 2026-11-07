#!/usr/bin/env bash
# Run conformance scenarios with coverage.
# usage: port/coverage/oracle.sh [files...]   files relative to port/conformance; tui/* files run in the TUI
#                                            suite. No files: run everything (server scenarios and TUI).
# LABEL env picks the run label (default "oracle"). Writes out/<label>-conformance*/lcov.info and out/<label>.ref.
set -uo pipefail
root=$(git rev-parse --show-toplevel)
out="$root/port/coverage/out"
label=${LABEL:-oracle}
mkdir -p "$out"
git -C "$root" rev-parse HEAD >"$out/$label.ref"
rm -rf "$out/$label-conformance" "$out/$label-conformance-tui"
server=() tui=()
for f in "$@"; do case "$f" in tui/*) tui+=("${f#tui/}") ;; *) server+=("$f") ;; esac; done
run() { # dir suite files...
  local dir=$1 suite=$2; shift 2
  cd "$dir" || return
  local start=$(date +%s)
  timeout 3600 bun test --coverage --coverage-reporter=lcov --coverage-dir="$out/$label-$suite" "$@" >"$out/run-$label-$suite.log" 2>&1
  echo "$label $suite exit=$? $(($(date +%s) - start))s $(grep -E '^ *[0-9]+ (pass|fail)|^Ran ' "$out/run-$label-$suite.log" | tr '\n' ' ')"
}
if [ $# -eq 0 ] || [ ${#server[@]} -gt 0 ]; then run "$root/port/conformance" conformance "${server[@]}"; fi
if [ $# -eq 0 ] || [ ${#tui[@]} -gt 0 ]; then run "$root/port/conformance/tui" conformance-tui "${tui[@]}"; fi
