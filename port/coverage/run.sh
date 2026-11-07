#!/usr/bin/env bash
# Run upstream test files of the given categories (see classify.py), per package, with coverage.
# usage: port/coverage/run.sh <label> <category,...>
#   port/coverage/run.sh bb A,B,D,E                 # tests that can run against Rust
#   port/coverage/run.sh all A,B,C,D,E,E-other,F,G  # the full in-scope upstream suite
# Writes port/coverage/out/<label>-<package>/lcov.info and run-<label>-<package>.log.
set -uo pipefail
root=$(git rev-parse --show-toplevel)
out="$root/port/coverage/out"
label=$1
cats=$2
git -C "$root" rev-parse HEAD >"$out/$label.ref"
pick() {
  python3 -I -c "
import json, sys
d = json.load(open('$out/lists.json'))
fs = [f for c in '$cats'.split(',') for f in d.get(c, [])]
if sys.argv[1] == '-': print(' '.join(sorted({f.split('/')[1] for f in fs})))
else: print(' '.join('./' + f.split('/', 2)[2] for f in fs if f.startswith('packages/' + sys.argv[1] + '/')))
" "$1"
}
for pkg in $(pick -); do
  cd "$root/packages/$pkg"
  start=$(date +%s)
  # shellcheck disable=SC2046
  timeout 1200 bun test --coverage --coverage-reporter=lcov --coverage-dir="$out/$label-$pkg" $(pick "$pkg") >"$out/run-$label-$pkg.log" 2>&1
  status=$?
  echo "$label $pkg exit=$status $(($(date +%s) - start))s $(grep -E '^ *[0-9]+ (pass|fail|skip)|^Ran ' "$out/run-$label-$pkg.log" | tr '\n' ' ')"
done
