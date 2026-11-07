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
