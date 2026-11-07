#!/usr/bin/env bash
# Point the conformance suite at a TS reference: port/conformance/ref -> <reference root>, and link the
# dependencies the suite imports from that reference's isolated node_modules, so the suite loads the same
# module instances as the reference it drives. port/ stays outside upstream's bun workspace: joining it would
# mean editing upstream's package.json, which would conflict on upstream pulls.
#
# usage: port/scripts/link-deps.sh [reference-root]
#   default: port/.reference (upstream + deviation patches, built by port/scripts/reference.sh build)
#   the repo root: plain upstream, without the deviation patches (after `bun install` at the repo root)
# port/conformance/ref and port/conformance/node_modules are gitignored.
set -euo pipefail
root=$(git rev-parse --show-toplevel)
target=$(realpath "${1:-$root/port/.reference}")
[ -d "$target/packages/server/node_modules" ] || { echo "$target has no installed packages/server" >&2; exit 1; }
conf="$root/port/conformance"
ln -sfn "$(realpath --relative-to="$conf" "$target")" "$conf/ref"
nm="$conf/node_modules"
rm -rf "$nm"
mkdir -p "$nm/@opencode"
ln -sfn ../ref/packages/server/node_modules/effect "$nm/effect"
for p in "$target"/packages/server/node_modules/@opencode/*; do
  ln -sfn "../../ref/packages/server/node_modules/@opencode/$(basename "$p")" "$nm/@opencode/$(basename "$p")"
done

# The TUI driver (port/conformance/tui) renders packages/tui, which needs its own dependencies (@opentui/*,
# solid-js...). Link whatever packages/tui has that the suite doesn't already resolve.
for p in "$target"/packages/tui/node_modules/* "$target"/packages/tui/node_modules/@*/*; do
  name=${p#"$target/packages/tui/node_modules/"}
  case "$name" in @*/) continue ;; .bin|.bin/*|@opencode|@opencode/*|effect|@*) [ "${name%%/*}" = "$name" ] && continue ;; esac
  [ -e "$nm/$name" ] && continue
  mkdir -p "$(dirname "$nm/$name")"
  depth=$(echo "$name" | tr -cd '/' | wc -c)
  up=$(printf '../%.0s' $(seq 1 $((depth + 1))))
  ln -sfn "${up}ref/packages/tui/node_modules/$name" "$nm/$name"
done
echo "conformance reference: $target ($(ls "$nm/@opencode" | wc -l) @opencode packages, $(ls -d "$nm"/@opentui/* | wc -l) @opentui)"
