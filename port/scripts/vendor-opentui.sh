#!/bin/sh
# Fetches the sources of the OpenTUI release the TS reference's TUI is built on (TS layer, Zig core and its
# vendored Zig dependencies: Yoga, uucode, Ghostty) into port/.vendor/opentui, read-only material for the Rust
# TUI port and its differential harnesses. Run it again after an upstream merge that bumps @opentui/core.
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
VERSION=$(sed -n 's/.*"@opentui\/core": "\([0-9.]*\)".*/\1/p' "$ROOT/../package.json" | head -n 1)
[ -n "$VERSION" ] || { echo "no @opentui/core version in the root package.json" >&2; exit 1; }
DEST="$ROOT/.vendor/opentui"
if [ -d "$DEST/.git" ] && [ "$(git -C "$DEST" describe --tags 2>/dev/null)" = "v$VERSION" ]; then
  echo "opentui v$VERSION already in $DEST"
  exit 0
fi
rm -rf "$DEST"
mkdir -p "$ROOT/.vendor"
git -c advice.detachedHead=false clone --quiet --depth 1 --branch "v$VERSION" https://github.com/anomalyco/opentui "$DEST"
mkdir -p "$DEST/packages/native/zig-deps"
tar -xzf "$DEST/packages/native/src/vendor/zig-deps.tar.gz" -C "$DEST/packages/native/zig-deps"
echo "opentui v$VERSION in $DEST"
