#!/usr/bin/env bash
# `cargo check` of the whole Rust workspace (tests included) for both macOS targets, from Linux.
#
# Two dependencies compile C, which needs the macOS SDK: ring (rustls's crypto) and libsqlite3-sys (bundled SQLite).
# Without the SDK their build scripts cannot compile, so a stub C compiler stands in for clang: it answers cc-rs's
# compiler probe through the real clang and writes an empty object for each compilation. A check links nothing, so
# those objects are never used; every Rust crate is checked for real. A build for macOS still needs the SDK.
#
# usage: port/scripts/check-macos.sh            (needs `rustup target add aarch64-apple-darwin x86_64-apple-darwin`
#                                                and clang)
set -uo pipefail
root=$(git rev-parse --show-toplevel)
stub=$root/port/rust/target/macos-stub-cc
mkdir -p "$(dirname "$stub")"
cat >"$stub" <<'EOF'
#!/bin/sh
for arg in "$@"; do
  # cc-rs's compiler family probe: clang, not clang-cl.
  if [ "$arg" = "-E" ]; then exec clang --target=arm64-apple-macos11 "$@"; fi
  if [ "$arg" = "-?" ]; then exit 1; fi
done
out=""
prev=""
for arg in "$@"; do
  if [ "$prev" = "-o" ]; then out="$arg"; fi
  prev="$arg"
done
if [ -n "$out" ]; then : >"$out"; fi
exit 0
EOF
chmod +x "$stub"
cd "$root/port/rust" || exit 1
status=0
for target in aarch64-apple-darwin x86_64-apple-darwin; do
  var=${target//-/_}
  echo "== $target"
  env "CC_$var=$stub" "AR_$var=ar" cargo check --workspace --tests --target "$target" || status=1
done
exit $status
