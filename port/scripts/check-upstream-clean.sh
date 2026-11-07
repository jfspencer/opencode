#!/usr/bin/env bash
# Fail if this branch changes anything upstream owns. Pulling from upstream stays conflict-free only while
# every fork change lives under port/ (plus brand-new, port-prefixed GitHub workflow files).
set -euo pipefail
root=$(git rev-parse --show-toplevel)
cd "$root"
allowed='^(port/|\.github/workflows/port-[^/]+\.ya?ml$)'
base=$(git merge-base HEAD upstream/dev)
violations=$({
  git diff --name-only "$base" HEAD # committed on this branch since the last upstream merge
  git diff --name-only --cached     # staged
  git diff --name-only              # unstaged edits to tracked files
} | sort -u | grep -vE "$allowed" || true)
if [ -n "$violations" ]; then
  echo "These changes touch upstream-owned paths and will eventually conflict with upstream:" >&2
  echo "$violations" | sed 's/^/  /' >&2
  echo "Move the change under port/ instead (see port/README.md)." >&2
  exit 1
fi
echo "ok: every fork change since upstream $(git rev-parse --short "$base") is under port/"
