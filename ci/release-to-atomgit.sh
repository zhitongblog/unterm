#!/usr/bin/env bash
#
# Sync a GitHub release to AtomGit / GitCode (binaries + body), run from your Mac.
#
# Why this exists:
#   Unterm is hosted on AtomGit (https://atomgit.com/zhitongblog/unterm).
#   AtomGit uploads release attachments directly to Huawei Cloud OBS via presigned PUT
#   URLs (`/api/v5/repos/:owner/:repo/releases/:tag/upload_url`), giving mainland China
#   users fast direct downloads without hitting GitHub CDN throttling.
#
# Usage:
#   bash ci/release-to-atomgit.sh v0.71.23
#
# Requires:
#   - `gh` CLI authenticated to the GitHub repo
#   - `ATOMGIT_TOKEN` env var (or in .env.local) — AtomGit personal access token
#   - `ATOMGIT_USER` env var (default: zhitongblog)
#   - jq, curl, python3

set -euo pipefail

if [ $# -lt 1 ]; then
  echo "Usage: $0 <tag> (e.g. $0 v0.71.23)" >&2
  exit 1
fi
TAG="$1"
VER="${TAG#v}"

# Load secrets from .env.local if present.
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
if [ -f "$ROOT/.env.local" ]; then
  set -a
  # shellcheck disable=SC1091
  source "$ROOT/.env.local"
  set +a
fi

: "${ATOMGIT_TOKEN:?Set ATOMGIT_TOKEN (export or in .env.local)}"
ATOMGIT_USER="${ATOMGIT_USER:-zhitongblog}"
REPO_GH="zhitongblog/unterm"
REPO_ATOMGIT="$ATOMGIT_USER/unterm"
API="https://api.gitcode.com/api/v5/repos/$REPO_ATOMGIT"

echo "==> Sync $TAG  ($REPO_GH → atomgit.com/$REPO_ATOMGIT)"

if [ -n "${ATOMGIT_WORK_DIR:-}" ]; then
  WORK="$ATOMGIT_WORK_DIR"
  mkdir -p "$WORK"
  echo "==> Reusing work dir $WORK (kept on exit)"
else
  WORK="$(mktemp -d -t atomgit-unterm-release-XXXXX)"
  trap 'rm -rf "$WORK"' EXIT
fi
cd "$WORK"

# Pre-seed local build outputs when present so we don't re-download ~250 MB from GitHub.
seed_local() {
  local src="$1" dst="$2"
  if [ -f "$src" ] && [ ! -f "$dst" ]; then
    cp -c "$src" "$dst" 2>/dev/null || cp "$src" "$dst"
  fi
}
seed_local "$ROOT/Unterm-macos-${TAG}.dmg" "Unterm-macos-${TAG}.dmg"
seed_local "$ROOT/Unterm-${VER}-x64.msi" "Unterm-${VER}-x64.msi"
seed_local "$ROOT/Unterm-${VER}-arm64.msi" "Unterm-${VER}-arm64.msi"
seed_local "$ROOT/Unterm-windows-x64-${TAG}.zip" "Unterm-windows-x64-${TAG}.zip"
seed_local "$ROOT/Unterm-windows-arm64-${TAG}.zip" "Unterm-windows-arm64-${TAG}.zip"
seed_local "$ROOT/Unterm-${TAG}-x86_64.AppImage" "Unterm-${TAG}-x86_64.AppImage"
seed_local "$ROOT/Unterm-${TAG}-aarch64.AppImage" "Unterm-${TAG}-aarch64.AppImage"
seed_local "$ROOT/unterm-${TAG}.deb" "unterm-${TAG}.deb"
seed_local "$ROOT/unterm-${TAG}.arm64.deb" "unterm-${TAG}.arm64.deb"

echo "==> Downloading GitHub assets to $WORK"
gh release view "$TAG" --repo "$REPO_GH" --json assets \
  -q '.assets[] | "\(.size)\t\(.name)"' > .manifest
MISSING=0
while IFS=$'\t' read -r want name; do
  if [ -f "$name" ] && [ "$(stat -f%z "$name" 2>/dev/null || stat -c%s "$name")" = "$want" ]; then
    printf "  = %-45s cached (%s bytes)\n" "$name" "$want"
    continue
  fi
  ok=0
  for try in 1 2 3; do
    printf "  ↓ %-45s attempt %d ... " "$name" "$try"
    if gh release download "$TAG" --repo "$REPO_GH" --pattern "$name" --clobber >/dev/null 2>&1 \
      && [ "$(stat -f%z "$name" 2>/dev/null || stat -c%s "$name")" = "$want" ]; then
      echo "ok"; ok=1; break
    fi
    echo "retry"
  done
  [ "$ok" = 1 ] || { echo "  ✗ could not fetch $name"; MISSING=1; }
done < .manifest
cut -f2 .manifest > .names
rm -f .manifest
[ "$MISSING" = 0 ] || { echo "Aborting: some assets could not be downloaded." >&2; exit 1; }

# 1. Resolve GitHub release metadata (name + body).
META=$(gh release view "$TAG" --repo "$REPO_GH" --json name,body)
NAME=$(echo "$META" | jq -r '.name')
[ -n "$NAME" ] && [ "$NAME" != "null" ] || NAME="Unterm $TAG"
BODY=$(echo "$META" | jq -r '.body // ""')
[ -n "$BODY" ] || BODY="Release $TAG — see https://unterm.app/changelog"

# 2. Check if the AtomGit release already exists for this tag.
EXISTING_TAG=$(curl -fsS "$API/releases/tags/$TAG?access_token=$ATOMGIT_TOKEN" 2>/dev/null \
  | jq -r '.tag_name // empty' || true)

if [ -n "$EXISTING_TAG" ]; then
  echo "==> Reusing AtomGit release tag=$EXISTING_TAG (resuming)"
  jq -nR \
    --arg name "$NAME" \
    --arg body "$BODY" \
    '{name: $name, body: $body}' \
  | curl -fsS -X PATCH \
      -H "Content-Type: application/json" \
      -d @- \
      "$API/releases/$TAG?access_token=$ATOMGIT_TOKEN" >/dev/null || true
else
  echo "==> Creating AtomGit release $TAG"
  RESP=$(jq -nR \
    --arg tag "$TAG" \
    --arg name "$NAME" \
    --arg body "$BODY" \
    --arg ref "master" \
    '{tag_name: $tag, name: $name, body: $body, target_commitish: $ref, release_status: "latest"}' \
  | curl -fsS -X POST \
      -H "Content-Type: application/json" \
      -d @- \
      "$API/releases?access_token=$ATOMGIT_TOKEN")
  CREATED_TAG=$(echo "$RESP" | jq -r '.tag_name // empty')
  if [ -z "$CREATED_TAG" ]; then
    echo "Failed to create AtomGit release:"
    echo "$RESP" | jq .
    exit 1
  fi
  echo "  created release tag=$CREATED_TAG"
fi

# 3. Upload every asset via presigned OBS PUT URL.
echo "==> Uploading assets to AtomGit release $TAG"

uploaded_names() {
  curl -fsS --max-time 120 "$API/releases/tags/$TAG?access_token=$ATOMGIT_TOKEN" \
    | jq -r '.assets[]? | select(.type != "source") | .name // empty' || true
}
HAVE="$(uploaded_names)"
FAILED=0

for f in $(cat .names); do
  SIZE=$(du -h "$f" | cut -f1)
  if printf '%s\n' "$HAVE" | grep -Fxq "$f"; then
    printf "  = %-45s %6s ... already on AtomGit\n" "$f" "$SIZE"
    continue
  fi
  ok=0
  for try in 1 2 3 4; do
    printf "  ↑ %-45s %6s attempt %d ... " "$f" "$SIZE" "$try"
    if python3 - "$API" "$TAG" "$ATOMGIT_TOKEN" "$f" <<'PY'
import json, sys, urllib.parse, urllib.request

api, tag, token, fname = sys.argv[1:5]
q = urllib.parse.urlencode({"access_token": token, "file_name": fname})
req = urllib.request.Request(
    f"{api}/releases/{urllib.parse.quote(tag, safe='')}/upload_url?{q}",
    headers={"Accept": "application/json"},
)
with urllib.request.urlopen(req, timeout=30) as resp:
    info = json.loads(resp.read())

upload_url = info["url"]
headers = dict(info.get("headers") or {})
with open(fname, "rb") as fp:
    data = fp.read()

put_req = urllib.request.Request(upload_url, data=data, method="PUT", headers=headers)
with urllib.request.urlopen(put_req, timeout=600) as put_resp:
    if not (200 <= put_resp.status < 300):
        sys.exit(1)
PY
    then
      HAVE="$(uploaded_names)"
      if printf '%s\n' "$HAVE" | grep -Fxq "$f"; then
        echo "ok"; ok=1; break
      fi
    fi
    echo "retry"
    sleep 3
  done
  [ "$ok" = 1 ] || { echo "    ✗ gave up on $f"; FAILED=1; }
done

echo
echo "==> Done"
COUNT=$(uploaded_names | grep -c . || true)
echo "  $COUNT assets on https://atomgit.com/$REPO_ATOMGIT/releases/tag/$TAG"
if [ "$FAILED" = 1 ]; then
  echo "  ⚠ some assets did not upload — re-run this script to resume." >&2
  exit 1
fi
