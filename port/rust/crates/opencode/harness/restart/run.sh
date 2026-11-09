#!/usr/bin/env bash
# Differential check of restart recovery (session/restart.rs): one database with a session and backgrounded job rows
# (running, completed and failed shell jobs, an image job, a subagent whose child is gone, a job of a missing session),
# booted as the managed server (`serve --service`, in a sandbox, on a free port) by the TS reference and by the Rust
# debug build; the session's inbox and the job list afterwards must be the same.
#
# usage: port/rust/crates/opencode/harness/restart/run.sh
set -u
here=$(cd "$(dirname "$0")" && pwd)
port=$(cd "$here/../../../../.." && pwd)
reference=$port/.reference
rust=$port/rust/target/debug/opencode
(cd "$port/rust" && OPENCODE_VERSION=conformance OPENCODE_CHANNEL=conformance cargo build --quiet -p opencode) || exit 1
preload=$(cd "$reference/packages/cli" && bun -e 'console.log(Bun.resolveSync("@opentui/solid/preload", process.cwd()))')
out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT
pid=
url=
boot() { # name command...: starts a server, leaving its URL in $url and its PID in $pid
  local name=$1
  shift
  local box=$out/$name
  mkdir -p "$box/home" "$box/config" "$box/data" "$box/state" "$box/cache"
  (
    export HOME=$box/home XDG_CONFIG_HOME=$box/config XDG_DATA_HOME=$box/data XDG_STATE_HOME=$box/state XDG_CACHE_HOME=$box/cache
    export OPENCODE_SERVER_PASSWORD=secret OPENCODE_DISABLE_AUTOUPDATE=1 OPENCODE_DB=$out/$name.db OPENCODE_DISABLE_FILEWATCHER=1
    export BUN_OPTIONS="--preload $preload --define OPENCODE_VERSION='\"conformance\"' --define OPENCODE_CHANNEL='\"conformance\"'"
    cd "$out/project"
    exec "$@" serve ${SERVE_FLAGS:-} --hostname 127.0.0.1 --port 0
  ) >"$out/$name.log" 2>&1 &
  pid=$!
  url=
  for _ in $(seq 1 150); do
    url=$(grep -o 'http://127.0.0.1:[0-9]*' "$out/$name.log" | head -1)
    [ -n "$url" ] && break
    sleep 0.2
  done
}
# The service keeps its password in its config directory.
key() { python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["password"])' "$out/$1/config/opencode/service-conformance.json"; }
mkdir -p "$out/project"
boot seed "$rust"
curl -s -u opencode:secret -H 'content-type: application/json' \
  -d "{\"id\":\"ses_00000000000000restartcheck\",\"location\":{\"directory\":\"$out/project\"}}" "$url/api/session" >/dev/null
kill "$pid"
wait "$pid" 2>/dev/null
python3 -I "$here/seed.py" "$out/seed.db"
for name in ts rust; do cp "$out/seed.db" "$out/$name.db"; done
for name in ts rust; do
  case $name in
  ts) SERVE_FLAGS=--service boot ts bun "$reference/packages/cli/src/index.ts" ;;
  rust) SERVE_FLAGS=--service boot rust "$rust" ;;
  esac
  sleep 2
  curl -s -u "opencode:$(key "$name")" "$url/api/session/ses_00000000000000restartcheck/inbox" >"$out/$name.inbox.json"
  curl -s -u "opencode:$(key "$name")" "$url/api/experimental/job" >"$out/$name.jobs.json"
  kill "$pid"
  wait "$pid" 2>/dev/null
done
python3 -I - "$out" <<'PY'
import json, re, sys
out = sys.argv[1]
failed = False
for kind in ["inbox", "jobs"]:
    read = lambda name: json.loads(re.sub(r'"created": ?\d+', '"created":0', open(f"{out}/{name}.{kind}.json").read()))
    ts, rust = read("ts"), read("rust")
    same = ts == rust
    failed |= not same
    print(f"{kind}: {'same' if same else 'different'} ({len(ts.get('data', []))} items)")
    if not same:
        print("  ts:  ", json.dumps(ts)[:2000])
        print("  rust:", json.dumps(rust)[:2000])
sys.exit(1 if failed else 0)
PY
