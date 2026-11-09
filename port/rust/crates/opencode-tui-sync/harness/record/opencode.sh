#!/usr/bin/env bash
# An `opencode` for CONFORMANCE_IMPL that records the TUI's data layer in the TUI scenarios: `serve` runs the Rust
# server ($SYNC_RECORD_SERVER), the TUI (`--server <url>`) is the TS reference's, run as the "ts" implementation
# runs it (port/conformance/lib/server.ts tsEnv) with harness/record/recorder.ts preloaded. Recordings go to
# $SYNC_RECORD_DIR (see harness/record/run.sh).
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
port=$(cd "$here/../../../../.." && pwd)
if [ "${1:-}" = "serve" ]; then
  exec "$SYNC_RECORD_SERVER" "$@"
fi
reference=$(realpath "$port/conformance/ref")
preload=$(cd "$reference/packages/cli" && bun -e 'console.log(Bun.resolveSync("@opentui/solid/preload", process.cwd()))')
# The recorder instruments a .tsx module (without JSX) before the Solid plugin would claim it.
export BUN_OPTIONS="--preload $here/recorder.ts --preload $preload --define OPENCODE_VERSION='\"conformance\"' --define OPENCODE_CHANNEL='\"conformance\"' --define OPENCODE_ARTIFACT='\"cli\"'"
exec bun "$reference/packages/cli/src/index.ts" "$@"
