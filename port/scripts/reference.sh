#!/usr/bin/env bash
# The TS reference that the conformance oracle records: upstream (this fork's HEAD; upstream paths are never
# modified here) plus the deviation patches in port/patches/, which fix upstream bugs the Rust port must not
# reproduce (see port/conformance/DEVIATIONS.md). It is materialized as a sparse git worktree without port/,
# with its own `bun install` (seconds: bun hardlinks its cache), and the conformance drivers load it through
# port/conformance/ref (set by link-deps.sh).
#
# usage: port/scripts/reference.sh build                 (re)create it from HEAD + port/patches/*.patch
#        port/scripts/reference.sh save "D123 area: fix" [body]   commit the reference's uncommitted changes
#                                                         as a new deviation patch (exported to port/patches/);
#                                                         the body is the deviation record (see patches/README.md)
#        port/scripts/reference.sh amend D123            fold the uncommitted changes into D123's patch
#        port/scripts/reference.sh export                rewrite port/patches/ from the reference's commits
#        port/scripts/reference.sh status                is it current? what is uncommitted?
# Patches apply in file-name order. A patch that stops applying after an upstream merge is the signal to
# re-check the bug against the new upstream code: re-derive the fix, or drop it if upstream fixed it.
# REFERENCE=<dir> overrides the location (default port/.reference).
set -euo pipefail
root=$(git rev-parse --show-toplevel)
ref=${REFERENCE:-$root/port/.reference}
patches=$root/port/patches
# Patch headers carry a neutral identity, not a person's.
export GIT_AUTHOR_NAME=opencode-port GIT_AUTHOR_EMAIL=port@localhost GIT_COMMITTER_NAME=opencode-port GIT_COMMITTER_EMAIL=port@localhost
g() { git -C "$ref" "$@"; }
base_file() { echo "$(g rev-parse --absolute-git-dir)/port-base"; }
base() { cat "$(base_file)"; }
# What the reference last exported (build/save/amend/export). Commits that differ from it were made by hand.
snapshot() { echo "$(g rev-parse --absolute-git-dir)/port-exported"; }
remember() { rm -rf "$(snapshot)" && export_patches "$(snapshot)"; }
need() { [ -f "$ref/.git" ] && [ -f "$(base_file)" ] || { echo "no reference at $ref: run $0 build" >&2; exit 1; }; }

export_patches() { # dir
  local dir=$1 sha subject id slug
  mkdir -p "$dir"
  rm -f "$dir"/D*.patch
  for sha in $(g rev-list --reverse "$(base)..HEAD"); do
    subject=$(g log -1 --format=%s "$sha")
    id=${subject%% *}
    [[ $id =~ ^D[0-9]{3}$ ]] || { echo "commit $sha: subject must start with a deviation ID (D123): $subject" >&2; exit 1; }
    slug=$(echo "${subject#* }" | tr 'A-Z' 'a-z' | sed -E 's/[^a-z0-9]+/-/g; s/^-|-$//g' | cut -c1-60 | sed -E 's/-$//')
    g format-patch -1 "$sha" --zero-commit --no-signature --no-numbered --no-stat --stdout >"$dir/$id-$slug.patch"
  done
}

case "${1:-}" in
build)
  if [ -f "$ref/.git" ]; then
    [ -z "$(g status --porcelain)" ] || { echo "the reference has uncommitted changes: save, amend or discard them first" >&2; g status --short >&2; exit 1; }
    if [ -f "$(base_file)" ] && [ -d "$(snapshot)" ]; then
      tmp=$(mktemp -d); export_patches "$tmp"
      diff -rq "$tmp" "$(snapshot)" >/dev/null 2>&1 || { echo "the reference has commits made by hand, not saved to port/patches: run $0 export first" >&2; rm -rf "$tmp"; exit 1; }
      rm -rf "$tmp"
    fi
  else
    git -C "$root" worktree add -q --no-checkout --detach "$ref" HEAD
    g sparse-checkout set --no-cone '/*' '!/port/'
  fi
  head=$(git -C "$root" rev-parse HEAD)
  g checkout -q --force --detach "$head"
  echo "$head" >"$(base_file)"
  for p in $(ls "$patches"/D*.patch 2>/dev/null | sort); do
    g am -q --3way "$p" || { g am --abort; echo "$(basename "$p") no longer applies to upstream $(git -C "$root" rev-parse --short HEAD): re-check the bug, then re-derive the fix or drop the patch" >&2; exit 1; }
  done
  remember
  (cd "$ref" && bun install --frozen-lockfile >/dev/null)
  "$root/port/scripts/link-deps.sh" "$ref"
  echo "reference: upstream $(git -C "$root" rev-parse --short HEAD) + $(g rev-list --count "$(base)..HEAD") deviation patches"
  ;;
save)
  need
  [[ ${2:-} =~ ^D[0-9]{3}\  ]] || { echo "usage: $0 save \"D123 area: what is fixed\"" >&2; exit 1; }
  ! g log --format=%s "$(base)..HEAD" | grep -q "^${2%% *} " || { echo "${2%% *} exists: use amend" >&2; exit 1; }
  [ -n "$(g status --porcelain)" ] || { echo "nothing to save" >&2; exit 1; }
  g add -A && g commit -q -m "$2" ${3:+-m "$3"}
  export_patches "$patches" && remember
  echo "saved $(ls "$patches"/"${2%% *}"-*.patch)"
  ;;
amend)
  need
  sha=$(g log --format='%H %s' "$(base)..HEAD" | awk -v id="${2:-}" '$2 == id { print $1 }')
  [ -n "$sha" ] || { echo "no deviation ${2:-} in the reference" >&2; exit 1; }
  [ -n "$(g status --porcelain)" ] || { echo "nothing to amend" >&2; exit 1; }
  g add -A && g commit -q --fixup="$sha"
  GIT_SEQUENCE_EDITOR=true g rebase -q -i --autosquash "$(base)"
  export_patches "$patches" && remember
  echo "amended $2"
  ;;
export)
  need
  export_patches "$patches" && remember
  ls "$patches"
  ;;
status)
  need
  b=$(base)
  if git -C "$root" diff --quiet "$b" HEAD -- . ':(exclude)port'; then echo "upstream: current"; else echo "upstream: changed since the reference was built: run $0 build"; fi
  tmp=$(mktemp -d); export_patches "$tmp"
  if diff -rq "$tmp" "$patches" -x '*.md' >/dev/null 2>&1 || { [ -z "$(ls "$tmp")" ] && [ -z "$(ls "$patches"/D*.patch 2>/dev/null)" ]; }; then echo "patches: in sync ($(ls "$tmp" | wc -l))"; else echo "patches: the reference and port/patches differ"; diff -rq "$tmp" "$patches" -x '*.md' || true; fi
  rm -rf "$tmp"
  changes=$(g status --short)
  if [ -n "$changes" ]; then echo "uncommitted:"; echo "$changes"; else echo "uncommitted: none"; fi
  ;;
*)
  sed -n '2,18p' "$0" | sed 's/^# \{0,1\}//'
  exit 1
  ;;
esac
