#!/usr/bin/env bash
# Deprecated wrapper: the implementation lives in `port/rust/crates/xtask`. Canonical command:
#   (cd port/rust && cargo xtask determinism ...)
# Kept so existing docs and CI keep working. It execs the built binary; when the binary is missing it builds
# once through `cargo run` (never on every invocation).
set -euo pipefail
root=$(git rev-parse --show-toplevel)
bin=$root/port/rust/target/debug/xtask
if [ -x "$bin" ]; then
  exec "$bin" determinism "$@"
fi
exec cargo run --quiet --manifest-path "$root/port/rust/Cargo.toml" -p xtask -- determinism "$@"
