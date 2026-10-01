#!/usr/bin/env bash
# Attach SHA256SUMS to a release, built from the digests GitHub recorded when
# each asset was uploaded -- nothing is downloaded or re-hashed here.
#
# The in-app updater reads it when the GitHub API will not answer (the
# anonymous allowance is 60 requests an hour per address), so run this once
# every asset is up: after release-linux, release-windows and release-mac.
#
#   bash ci/release-checksums.sh vX.Y.Z
set -euo pipefail
TAG="${1:?usage: ci/release-checksums.sh vX.Y.Z}"
out="$(mktemp -d)/SHA256SUMS"
gh release view "$TAG" --repo zhitongblog/unterm --json assets \
  --jq '.assets[] | select(.name != "SHA256SUMS") | select(.digest != null) | "\(.digest | ltrimstr("sha256:"))  \(.name)"' \
  | sort -k2 > "$out"
count=$(wc -l < "$out" | tr -d ' ')
if [ "$count" -lt 9 ]; then
  echo "only $count assets carry a digest on $TAG; expected 9 -- is every build uploaded?" >&2
  cat "$out" >&2
  exit 1
fi
gh release upload "$TAG" "$out" --repo zhitongblog/unterm --clobber
echo ">> SHA256SUMS ($count files) attached to $TAG"
