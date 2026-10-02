#!/usr/bin/env bash
# Set a release's notes from its CHANGELOG.md section, with an invitation to
# star the repository, and to share a post about it, at the end. Run once the release exists.
#
#   bash ci/release-notes.sh vX.Y.Z
set -euo pipefail
TAG="${1:?usage: ci/release-notes.sh vX.Y.Z}"
cd "$(dirname "$0")/.."
notes="$(mktemp)"
# The section from "## vX.Y.Z" up to (not including) the next "## v".
awk -v tag="$TAG" '
  $0 ~ "^## " tag "( |$)" { on = 1; next }
  on && /^## v/ { exit }
  on { print }
' CHANGELOG.md | sed -e '/./,$!d' > "$notes"
if [ ! -s "$notes" ]; then
  echo "CHANGELOG.md has no section for $TAG" >&2
  exit 1
fi
cat >> "$notes" <<'MD'

---

**Install:** download below, or `brew install --cask zhitongblog/tap/unterm` · `scoop bucket add zhitongblog https://github.com/zhitongblog/scoop-bucket; scoop install unterm` · already on 0.71.17 or later: `unterm-cli update`.

If Unterm is useful to you, a ⭐ on [the repository](https://github.com/zhitongblog/unterm) helps other people find it. Wrote about it? [Share the link](https://unterm.app/ambassador) and it's listed on the site.
MD
gh release edit "$TAG" --repo zhitongblog/unterm --notes-file "$notes"
echo ">> notes for $TAG set from CHANGELOG.md ($(wc -l < "$notes" | tr -d ' ') lines)"
