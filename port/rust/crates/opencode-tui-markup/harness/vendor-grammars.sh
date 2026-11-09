#!/usr/bin/env bash
# Vendors the C sources of the tree-sitter grammars OpenTUI 0.8.1 and the TUI bundle (see grammars/SOURCES.txt)
# into grammars/: each repository's licence, its common/ headers and, per grammar, src/parser.c, the scanner and
# the headers they include. Downloads go to the scratch directory given as the first argument.
#
# usage: harness/vendor-grammars.sh <download dir>
set -euo pipefail
crate=$(cd "$(dirname "$0")/.." && pwd)
work=$1
mkdir -p "$work/tar" "$work/src"
grep -v '^#' "$crate/grammars/SOURCES.txt" | while read -r grammar repo tag archive src _license; do
  [ -z "$grammar" ] && continue
  dir=${src%%/*}
  tarball="$work/tar/$dir.tar.gz"
  if [ ! -s "$tarball" ]; then
    curl -sSfL --retry 3 -o "$tarball" "$archive"
  fi
  if [ ! -d "$work/src/$dir" ]; then
    mkdir -p "$work/src/$dir"
    if tar -tzf "$tarball" | head -1 | grep -q '^\./'; then
      tar -xzf "$tarball" -C "$work/src/$dir"
    else
      tar -xzf "$tarball" -C "$work/src/$dir" --strip-components=1
    fi
  fi
  out="$crate/grammars/$src"
  rm -rf "$out"
  mkdir -p "$out/tree_sitter"
  cp "$work/src/$src"/*.c "$out/"
  find "$work/src/$src" -maxdepth 1 -name '*.h' -exec cp {} "$out/" \;
  cp "$work/src/$src"/tree_sitter/*.h "$out/tree_sitter/"
  for licence in "$work/src/$dir"/LICENSE* "$work/src/$dir"/COPYING*; do
    [ -f "$licence" ] && cp "$licence" "$crate/grammars/$dir/"
  done
  if [ -d "$work/src/$dir/common" ]; then
    mkdir -p "$crate/grammars/$dir/common"
    find "$work/src/$dir/common" -maxdepth 1 -name '*.h' -exec cp {} "$crate/grammars/$dir/common/" \;
  fi
done
