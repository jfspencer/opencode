#!/usr/bin/env bash
# Merge the latest upstream dev into this fork's dev. The fork never edits upstream files, so this merge
# cannot conflict. Prints the upstream range that the conformance loop has to process. Does not push.
set -euo pipefail
root=$(git rev-parse --show-toplevel)
cd "$root"
[ "$(git rev-parse --abbrev-ref HEAD)" = dev ] || { echo "switch to dev first" >&2; exit 1; }
git diff --quiet && git diff --cached --quiet || { echo "commit or stash your changes first" >&2; exit 1; }
port/scripts/check-upstream-clean.sh
before=$(git merge-base HEAD upstream/dev)
git fetch --quiet upstream dev
after=$(git rev-parse upstream/dev)
if [ "$before" = "$after" ]; then
  echo "already up to date with upstream $(git rev-parse --short "$after")"
  exit 0
fi
git merge --no-edit upstream/dev
echo
echo "merged upstream $(git rev-parse --short "$before")..$(git rev-parse --short "$after") ($(git rev-list --count "$before..$after") commits)"
echo "next: bun install, port/scripts/link-deps.sh, run the conformance loop for that range, then: git push origin dev"
