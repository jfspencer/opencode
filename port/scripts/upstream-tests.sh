#!/usr/bin/env bash
# Run upstream's own in-scope test files (port/coverage/out/lists.json, from classify.py) in a source tree and list
# the tests that fail. A deviation patch that changes behavior an upstream test asserts must update that test in the
# same patch, so the reference (upstream + port/patches) passes upstream's suite exactly as well as upstream does.
#
# usage: port/scripts/upstream-tests.sh <label> [tree]     tree: default port/.reference; `.` = plain upstream
#        port/scripts/upstream-tests.sh compare <a> <b>    tests failing in run <b> but not in run <a>
# Writes port/coverage/out/upstream-<label>/<package>.log and failures.txt.
set -uo pipefail
root=$(git rev-parse --show-toplevel)
out="$root/port/coverage/out"
if [ "${1:-}" = "compare" ]; then
  comm -13 <(sort -u "$out/upstream-$2/failures.txt") <(sort -u "$out/upstream-$3/failures.txt")
  exit 0
fi
label=${1:?usage: $0 <label> [tree]}
tree=$(realpath "${2:-$root/port/.reference}")
dir="$out/upstream-$label"
rm -rf "$dir" && mkdir -p "$dir"
[ -f "$out/lists.json" ] || python3 -I "$root/port/coverage/classify.py" >/dev/null
pick() {
  python3 -I -c "
import json, sys
d = json.load(open('$out/lists.json'))
fs = [f for c in 'A,B,C,D,E,F,G'.split(',') for f in d.get(c, [])]
if sys.argv[1] == '-': print(' '.join(sorted({f.split('/')[1] for f in fs})))
else: print(' '.join('./' + f.split('/', 2)[2] for f in fs if f.startswith('packages/' + sys.argv[1] + '/')))
" "$1"
}
for pkg in $(pick -); do
  start=$(date +%s)
  # shellcheck disable=SC2046
  (cd "$tree/packages/$pkg" && timeout 1800 bun test $(pick "$pkg") >"$dir/$pkg.log" 2>&1)
  status=$?
  grep -hE '^\(fail\)' "$dir/$pkg.log" | sed -E "s/ \[[0-9.]+m?s\]$//; s/^/$pkg: /" >>"$dir/failures.txt"
  echo "$label $pkg exit=$status $(($(date +%s) - start))s $(grep -E '^ *[0-9]+ (pass|fail)' "$dir/$pkg.log" | tr -s ' \n' ' ')"
done
touch "$dir/failures.txt"
echo "$(wc -l <"$dir/failures.txt") failing tests in $tree (list: $dir/failures.txt)"
