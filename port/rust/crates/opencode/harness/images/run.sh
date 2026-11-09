#!/usr/bin/env bash
# Differential check of image normalization (tool/file_media.rs) against the reference's photon (photon-node 0.3.4):
# generated PNG and JPEG images, opaque and transparent, smooth and noisy, from tiny to over the size and byte limits
# (resized, re-encoded, or too large to fit), and a few undecodable ones: the decision, the type and the exact bytes.
#
# usage: port/rust/crates/opencode/harness/images/run.sh [cases=40] [seed=1]
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
core=$(cd "$here/../../../../../.reference/packages/core" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
(cd "$core" && bun "$here/imagecases.ts" "$core" "${2:-1}" "${1:-40}") >"$work/images.json"
cd "$here/../.."
IMAGE_CASES=$work/images.json cargo test --quiet -p opencode recorded_images -- --ignored --nocapture
