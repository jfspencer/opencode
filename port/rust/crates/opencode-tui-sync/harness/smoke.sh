#!/usr/bin/env bash
# A live check of the crate's client against the Rust server: starts `opencode serve` in a sandbox (its own HOME and
# XDG directories under a temporary directory, an in-memory database, a random port, no network), runs the
# sync-smoke example against it, and stops it.
#
# usage: harness/smoke.sh   (needs port/rust/target/debug/opencode and the sync-smoke example built)
set -euo pipefail
crate=$(cd "$(dirname "$0")/.." && pwd)
port=$(cd "$crate/../../.." && pwd)
server="$port/rust/target/debug/opencode"
smoke="$port/rust/target/debug/examples/sync-smoke"
sandbox=$(mktemp -d "${TMPDIR:-/tmp}/opencode-tui-sync-smoke.XXXXXX")
trap 'kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; rm -rf "$sandbox"' EXIT
mkdir -p "$sandbox/home" "$sandbox/config" "$sandbox/data" "$sandbox/state" "$sandbox/cache" "$sandbox/project"
echo '{}' >"$sandbox/models.json"
(
  cd "$sandbox/project"
  exec env -i PATH="$PATH" HOME="$sandbox/home" XDG_CONFIG_HOME="$sandbox/config" XDG_DATA_HOME="$sandbox/data" \
    XDG_STATE_HOME="$sandbox/state" XDG_CACHE_HOME="$sandbox/cache" OPENCODE_DB=:memory: \
    OPENCODE_DISABLE_MODELS_FETCH=1 OPENCODE_MODELS_PATH="$sandbox/models.json" OPENCODE_DISABLE_AUTOUPDATE=1 \
    OPENCODE_DISABLE_FILEWATCHER=1 OPENCODE_SERVER_PASSWORD=smoke \
    "$server" serve --hostname 127.0.0.1 --port 0 >"$sandbox/server.log" 2>&1
) &
pid=$!
for _ in $(seq 1 300); do
  url=$(grep -o 'server listening on http://[^ ]*' "$sandbox/server.log" | sed 's/server listening on //' || true)
  [ -n "$url" ] && break
  sleep 0.1
done
[ -n "${url:-}" ] || { cat "$sandbox/server.log" >&2; exit 1; }
"$smoke" "$url" smoke "$sandbox/project"
