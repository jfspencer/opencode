#!/usr/bin/env bash
# The `opencode-pty` daemon (persistent terminals) for a Rust install. TS ships the daemon inside its single-file
# executable (packages/cli/script/build.ts embeds `@opencode/pty`'s platform binary) and installs it on first use under
# `<cache>/opencode/bin/opencode-pty/<version>-<sha256>/`; `OPENCODE_PTY_BIN` overrides it, and without an embedded
# daemon `opencode-pty` comes from PATH. The Rust executable locates it the same way (src/persistent_pty/binary.rs)
# and embeds it when built with OPENCODE_PTY_EMBED=<daemon> (build.rs).
#
# The daemon is the version the reference's core pins (`@opencode/pty` in packages/core/package.json), whose protocol
# the server speaks. Upstream's release workflow (.github/workflows/release-process-daemon.yml) builds it with Zig 0.16
# for libghostty-vt and publishes it to npm; the reference gets that build through `bun install`.
#
# usage: port/scripts/opencode-pty.sh [--source] [--install]
#   (default)  the reference's prebuilt daemon (@opencode/pty-<platform>-<arch>[-<libc>] in its node_modules)
#   --source   build packages/process-daemon at its release tag (process-daemon-v<version>) with cargo instead: needs
#              Zig 0.16 on PATH and the network (Ghostty's pinned source, the crates, the pinned Rust toolchain)
#   --install  then build the release executable with the daemon embedded (port/rust/target/release/opencode)
# Prints the daemon's path: port/rust/target/opencode-pty/<version>/opencode-pty.
set -euo pipefail
root=$(git rev-parse --show-toplevel)
ref=$root/port/.reference
source=0
install=0
for arg in "$@"; do
  case "$arg" in
  --source) source=1 ;;
  --install) install=1 ;;
  *) echo "usage: $0 [--source] [--install]" >&2; exit 2 ;;
  esac
done
[ -d "$ref/packages/core" ] || { echo "no TS reference at $ref: run port/scripts/reference.sh build (or worktree-setup.sh)" >&2; exit 1; }

version=$(sed -nE 's/^ *"@opencode\/pty": "([^"]+)".*/\1/p' "$ref/packages/core/package.json" | head -1)
[ -n "$version" ] || { echo "packages/core/package.json pins no @opencode/pty version" >&2; exit 1; }
protocol=$(sed -nE 's/^const PROTOCOL_VERSION: f64 = ([0-9]+)\.0;$/\1/p' "$root/port/rust/crates/opencode/src/persistent_pty/daemon.rs")
out=$root/port/rust/target/opencode-pty/$version
mkdir -p "$out"

if [ "$source" = 1 ]; then
  tag=process-daemon-v$version
  git rev-parse -q --verify "refs/tags/$tag" >/dev/null || { echo "no tag $tag: git fetch upstream --tags" >&2; exit 1; }
  zig=$(command -v zig) || { echo "the daemon's build.rs needs Zig 0.16 (libghostty-vt); without it, use the prebuilt daemon (no --source)" >&2; exit 1; }
  work=$out/source
  rm -rf "$work" && mkdir -p "$work"
  git archive "$tag" packages/process-daemon | tar -x -C "$work"
  daemon=$work/packages/process-daemon
  # As the release workflow builds it: a baseline CPU through script/release-bin/zig.
  chmod +x "$daemon/script/release-bin/zig"
  (cd "$daemon" && OPENCODE_REAL_ZIG=$zig PATH="$daemon/script/release-bin:$PATH" cargo build --release --locked)
  cp "$daemon/target/release/opencode-pty" "$out/opencode-pty"
else
  prebuilt=$(cd "$ref/packages/core" && bun -e 'const pty = await import("@opencode/pty"); console.log(pty.binaryPath ?? "")')
  [ -n "$prebuilt" ] && [ -x "$prebuilt" ] || { echo "the reference has no prebuilt opencode-pty for this platform: use --source" >&2; exit 1; }
  # Unchanged bytes keep their file (and its time), so an embedding build is not redone for nothing.
  cmp -s "$prebuilt" "$out/opencode-pty" || cp "$prebuilt" "$out/opencode-pty"
fi
chmod 755 "$out/opencode-pty"

# The daemon names its version and protocol; the server refuses any other protocol.
reported=$("$out/opencode-pty" --version)
[ "$reported" = "opencode-pty $version (protocol $protocol)" ] ||
  { echo "unexpected daemon: $reported (want opencode-pty $version, protocol $protocol)" >&2; exit 1; }

if [ "$install" = 1 ]; then
  (cd "$root/port/rust" && OPENCODE_PTY_EMBED=$out/opencode-pty OPENCODE_PTY_EMBED_VERSION=$version cargo build --release -p opencode)
  echo "built $root/port/rust/target/release/opencode with $reported embedded" >&2
fi
echo "$out/opencode-pty"
