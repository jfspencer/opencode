#!/usr/bin/env bash
# Records the TS TUI's data layer in real runs: runs TUI scenario files (port/conformance/tui) against the Rust
# server with the TS TUI under harness/record/recorder.ts, then turns the recordings into data cases for
# harness/diff.ts (`--cases <out>/recorded.jsonl`).
#
# usage: harness/record/run.sh <out dir> <scenario file>...   (files relative to port/conformance/tui)
# The Rust server binary is port/rust/target/debug/opencode, built with the oracle's identity
# (OPENCODE_VERSION=conformance OPENCODE_CHANNEL=conformance cargo build -p opencode).
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
port=$(cd "$here/../../../../.." && pwd)
out=$(mkdir -p "$1" && cd "$1" && pwd)
shift
server="$port/rust/target/debug/opencode"
[ -x "$server" ] || { echo "no server binary at $server" >&2; exit 1; }
# The TUI process gets a scrubbed environment, so the wrapper carries its settings itself.
wrapper="$out/opencode"
cat >"$wrapper" <<EOF
#!/usr/bin/env bash
export SYNC_RECORD_DIR="$out/raw"
export SYNC_RECORD_SERVER="$server"
exec "$here/opencode.sh" "\$@"
EOF
chmod +x "$wrapper"
status=0
for file in "$@"; do
  (cd "$port/conformance/tui" && CONFORMANCE_IMPL="$wrapper" bun test "$file") >"$out/$(basename "$file").log" 2>&1 || status=$?
done
bun "$here/cases.ts" "$out/raw" >"$out/recorded.jsonl"
echo "$(wc -l <"$out/recorded.jsonl") recorded cases in $out/recorded.jsonl (scenario runs exited $status)"
