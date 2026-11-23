#!/usr/bin/env bash
# Thin wrapper: the implementation is `cargo xtask link-deps` (port/rust/crates/xtask), shared by macOS and Linux.
set -euo pipefail
root=$(git rev-parse --show-toplevel)
bin=$root/port/rust/target/debug/xtask
[ -x "$bin" ] || (cd "$root/port/rust" && cargo build --quiet -p xtask)
exec "$bin" link-deps "$@"
