#!/usr/bin/env bash
# Fetches the C sources of the tree-sitter grammars opencode-tui-markup compiles, as listed in its
# grammars/SOURCES.txt, into port/.vendor/tree-sitter: each archive (checked against its sha256 and kept in
# .archives/), then per grammar its src/parser.c, scanner and headers plus the repository's common/ headers, at
# the path SOURCES.txt names. The crate's build.rs compiles from there. Run it again when SOURCES.txt changes.
#
# usage: port/scripts/vendor-grammars.sh [destination]
#   destination defaults to port/.vendor/tree-sitter; build.rs reads OPENCODE_TREE_SITTER_VENDOR, else the default.
set -euo pipefail
port=$(cd "$(dirname "$0")/.." && pwd)
sources="$port/rust/crates/opencode-tui-markup/grammars/SOURCES.txt"
dest=${1:-$port/.vendor/tree-sitter}
archives="$dest/.archives"
mkdir -p "$archives"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

grep -v '^#' "$sources" | while read -r grammar _repo _tag archive sha src _licence _wasm; do
  [ -n "$grammar" ] || continue
  dir=${src%%/*}
  file="$archives/$dir.tar.gz"
  if [ ! -s "$file" ] || ! echo "$sha  $file" | sha256sum --quiet -c - >/dev/null 2>&1; then
    curl -sSfL --retry 3 -o "$file.part" "$archive"
    mv "$file.part" "$file"
  fi
  echo "$sha  $file" | sha256sum --quiet -c - || { echo "$file: sha256 is not $sha" >&2; exit 1; }
  if [ ! -d "$tmp/$dir" ]; then
    mkdir -p "$tmp/$dir"
    # Tag archives have one top directory; release archives hold the repository files directly.
    first=$(tar -tf "$file" | head -n 1 || true)
    case "$first" in
      ./* | src/* | LICENSE*) tar -xf "$file" -C "$tmp/$dir" ;;
      *) tar -xf "$file" -C "$tmp/$dir" --strip-components=1 ;;
    esac
  fi
  out="$dest/$src"
  rm -rf "$out"
  mkdir -p "$out/tree_sitter"
  cp "$tmp/$src"/*.c "$out/"
  find "$tmp/$src" -maxdepth 1 -name '*.h' -exec cp {} "$out/" \;
  cp "$tmp/$src"/tree_sitter/*.h "$out/tree_sitter/"
  for licence in "$tmp/$dir"/LICENSE* "$tmp/$dir"/COPYING*; do
    [ -f "$licence" ] && cp "$licence" "$dest/$dir/"
  done
  if [ -d "$tmp/$dir/common" ]; then
    mkdir -p "$dest/$dir/common"
    find "$tmp/$dir/common" -maxdepth 1 -name '*.h' -exec cp {} "$dest/$dir/common/" \;
  fi
  echo "$grammar: $src"
done
# build.rs compares this copy with the crate's to catch a stale vendor directory.
cp "$sources" "$dest/SOURCES.txt"
echo "tree-sitter grammars in $dest"
