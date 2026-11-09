#!/usr/bin/env bash
# The differential harness of opencode-tui-core's buffer, output and terminal modules against the reference's
# @opentui/core 0.8.1 (libopentui.so): the same op scripts run on both sides and every cell, frame byte and setup
# byte must agree.
#   buffer   OptimizedBuffer ops (gen.ts: hand-picked and random), then every cell (ops.ts / harness buffer)
#   frames   frames drawn into a test renderer set up against Ghostty's terminal core, their output bytes
#            (frames.ts / harness frames)
#   setup    terminal setup, the replies Ghostty sends and teardown, the control bytes (setup.ts / harness setup)
#   renderables  LineNumber and Diff scenes on a test renderer, every cell's character and colors
#            (renderables.ts / examples/renderables.rs)
# usage: port/rust/crates/opencode-tui-core/harness/run.sh [random buffer cases=2000]
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
rust=$(cd "$here/../../.." && pwd)
out=$rust/target/tui-core-harness
mkdir -p "$out"
count=${1:-2000}

(cd "$rust" && cargo build --quiet -p opencode-tui-core --example harness --example renderables)
harness=$rust/target/debug/examples/harness
status=0

echo "buffer: $count random cases and the hand-picked ones"
bun "$here/gen.ts" "$count" >"$out/cases.txt"
bun "$here/ops.ts" "$out/cases.txt" >"$out/buffer-ts.txt"
"$harness" buffer "$out/cases.txt" >"$out/buffer-rust.txt"
python3 -I "$here/compare.py" "$out/buffer-ts.txt" "$out/buffer-rust.txt" || status=1

echo "frames"
(cd "$here" && bun frames.ts "$out/frames-script.txt" "$out/frames-ts.txt" >/dev/null)
"$harness" frames "$out/frames-script.txt" >"$out/frames-rust.txt"
python3 -I "$here/compare.py" "$out/frames-ts.txt" "$out/frames-rust.txt" || status=1

echo "setup"
(cd "$here" && bun setup.ts "$out/setup-log.txt" "$out/setup-ts.txt" >/dev/null)
"$harness" setup "$out/setup-log.txt" >"$out/setup-rust.txt"
python3 -I "$here/compare.py" "$out/setup-ts.txt" "$out/setup-rust.txt" || status=1

echo "renderables"
(cd "$here" && bun renderables.ts "$out/renderables-cases.txt" "$out/renderables-ts.txt" 400)
"$rust/target/debug/examples/renderables" "$out/renderables-cases.txt" >"$out/renderables-rust.txt"
python3 -I "$here/compare.py" "$out/renderables-ts.txt" "$out/renderables-rust.txt" || status=1

exit $status
