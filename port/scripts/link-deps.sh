#!/usr/bin/env bash
# port/ stays outside upstream's bun workspace: joining it would mean editing upstream's package.json,
# which would conflict on upstream pulls. Instead, link the dependencies the conformance suite imports
# from packages/server's isolated node_modules, so the suite loads the same module instances as the TS
# reference it drives. port/conformance/node_modules is gitignored by the root .gitignore.
# Run after `bun install` at the repo root.
set -euo pipefail
root=$(git rev-parse --show-toplevel)
nm="$root/port/conformance/node_modules"
mkdir -p "$nm/@opencode"
ln -sfn ../../../packages/server/node_modules/effect "$nm/effect"
for p in "$root"/packages/server/node_modules/@opencode/*; do
  ln -sfn "../../../../packages/server/node_modules/@opencode/$(basename "$p")" "$nm/@opencode/$(basename "$p")"
done
echo "linked: effect $(ls "$nm/@opencode" | sed 's/^/@opencode\//' | tr '\n' ' ')"

# The TUI driver (port/conformance/tui) renders packages/tui, which needs its own dependencies (@opentui/*,
# solid-js...). Link whatever packages/tui has that the suite doesn't already resolve.
for p in "$root"/packages/tui/node_modules/* "$root"/packages/tui/node_modules/@*/*; do
  name=${p#"$root/packages/tui/node_modules/"}
  case "$name" in @*/) continue ;; .bin|.bin/*|@opencode|@opencode/*|effect|@*) [ "${name%%/*}" = "$name" ] && continue ;; esac
  [ -e "$nm/$name" ] && continue
  mkdir -p "$(dirname "$nm/$name")"
  depth=$(echo "$name" | tr -cd '/' | wc -c)
  up=$(printf '../%.0s' $(seq 1 $((depth + 3))))
  ln -sfn "${up}packages/tui/node_modules/$name" "$nm/$name"
done
echo "linked TUI deps: $(cd "$nm" && ls -d @opentui/* solid-js 2>/dev/null | tr '\n' ' ')"
