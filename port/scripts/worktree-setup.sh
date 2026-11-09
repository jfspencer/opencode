#!/usr/bin/env bash
# Prepares a linked git worktree of this repo for port work: links the main checkout's TS reference
# (port/.reference, port/conformance/ref), its vendored sources (port/.vendor) and its installed packages (port/conformance/node_modules,
# port/rust/gen/node_modules), so the conformance standard, gen-types.sh and the differential harnesses run there.
# The reference is shared: build it (port/scripts/reference.sh) only from the main checkout.
#
# usage: port/scripts/worktree-setup.sh        (run inside the worktree)
set -euo pipefail
root=$(git rev-parse --show-toplevel)
main=$(dirname "$(git rev-parse --path-format=absolute --git-common-dir)")
[ "$root" != "$main" ] || { echo "this is the main checkout; nothing to link" >&2; exit 0; }
[ -d "$main/port/.reference/packages" ] || { echo "no TS reference in $main: run port/scripts/reference.sh build there" >&2; exit 1; }
link() { # target, link
  [ -e "$2" ] && [ ! -L "$2" ] && { echo "$2 exists and is not a link" >&2; exit 1; }
  ln -sfn "$1" "$2"
}
link "$main/port/.reference" "$root/port/.reference"
link "$main/port/.reference" "$root/port/conformance/ref"
link "$main/port/conformance/node_modules" "$root/port/conformance/node_modules"
[ -d "$main/port/rust/gen/node_modules" ] && link "$main/port/rust/gen/node_modules" "$root/port/rust/gen/node_modules"
[ -d "$main/port/.vendor" ] && link "$main/port/.vendor" "$root/port/.vendor"
# A link to a directory does not match a `dir/` ignore pattern; keep it out of commits.
exclude="$main/.git/info/exclude"
grep -qxF "/port/.reference" "$exclude" || echo "/port/.reference" >>"$exclude"
grep -qxF "/port/.vendor" "$exclude" || echo "/port/.vendor" >>"$exclude"
echo "linked $root to the reference and packages of $main"
