#!/usr/bin/env bash
# Publish a finished Unterm release to Homebrew, Scoop and winget.
#
# Usage:
#
#   bash ci/publish-packages.sh vX.Y.Z [--dry-run]
#
# Run it after the GitHub release for vX.Y.Z carries every asset: the Windows
# and Linux ones come from CI, the macOS dmg from ci/release-mac.sh. So this is
# the step after `make release-mac`.
#
# What it does:
#
#   1. Checks that the release exists, is published (not a draft or
#      prerelease) and has the five assets the package managers point at.
#   2. Downloads them into a temp dir, computes their sha256 and checks each
#      against the digest GitHub recorded on upload; reads the ProductCode of
#      both MSIs; checks the Windows zips still have the layout the Scoop
#      manifest's extract_dir names.
#   3. Renders packaging/homebrew, packaging/scoop and packaging/winget and
#      lints what it rendered (JSON parse; `brew style` when brew is present).
#   4. Without --dry-run:
#        a. clones zhitongblog/homebrew-tap, writes Casks/unterm.rb, commits
#           "unterm X.Y.Z" and pushes -- unless the file is already identical;
#        b. the same for zhitongblog/scoop-bucket and bucket/unterm.json;
#        c. winget: `komac update zhitongblog.Unterm ... --submit`, which
#           opens a PR on microsoft/winget-pkgs. Without komac, or before the
#           package's first version has been merged there, it prints exactly
#           what to run instead and does not fail.
#
# With --dry-run nothing remote is touched (no clone, no push, no PR): the
# rendered files are left in a temp dir and their paths printed.
#
# Requires: gh (authenticated), git, unzip, shasum or sha256sum. Optional:
# msiinfo (msitools) or python3 for MSI ProductCodes, brew for `brew style`,
# komac for winget. See packaging/README.md.
set -euo pipefail

usage() {
  echo "usage: bash ci/publish-packages.sh vX.Y.Z [--dry-run]" >&2
  exit 2
}

TAG=""
DRY_RUN=0
for arg in "$@"; do
  case "$arg" in
    --dry-run) DRY_RUN=1 ;;
    -h|--help) usage ;;
    v*) [ -z "$TAG" ] || usage; TAG="$arg" ;;
    *) usage ;;
  esac
done
[ -n "$TAG" ] || usage
if ! [[ "$TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "ERROR: '$TAG' is not a release tag of the form vX.Y.Z" >&2
  exit 2
fi
VERSION="${TAG#v}"

SOURCE_REPO="${UNTERM_SOURCE_REPO:-zhitongblog/unterm}"
TAP_REPO="${UNTERM_TAP_REPO:-zhitongblog/homebrew-tap}"
BUCKET_REPO="${UNTERM_BUCKET_REPO:-zhitongblog/scoop-bucket}"
WINGET_ID="zhitongblog.Unterm"
WINGET_PATH="manifests/z/zhitongblog/Unterm"

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
PKG="$ROOT/packaging"

say()  { printf '>> %s\n' "$*"; }
note() { printf '   %s\n' "$*"; }
warn() { printf 'WARNING: %s\n' "$*" >&2; }
die()  { printf 'ERROR: %s\n' "$*" >&2; exit 1; }

for tool in gh git unzip; do
  command -v "$tool" >/dev/null 2>&1 || die "'$tool' is required but not on PATH"
done
if command -v shasum >/dev/null 2>&1; then
  sha256() { shasum -a 256 "$1" | awk '{print $1}'; }
elif command -v sha256sum >/dev/null 2>&1; then
  sha256() { sha256sum "$1" | awk '{print $1}'; }
else
  die "need shasum or sha256sum"
fi

WORK=$(mktemp -d "${TMPDIR:-/tmp}/unterm-publish-$VERSION.XXXXXX")
DL="$WORK/download"
OUT="$WORK/rendered"
mkdir -p "$DL" "$OUT"

LINT_TAP="unterm-publish/lint"
LINT_TAP_CREATED=0
cleanup() {
  if [ "$LINT_TAP_CREATED" = 1 ]; then
    brew untap --force "$LINT_TAP" >/dev/null 2>&1 || true
  fi
  # The downloads are ~200 MB and only needed for hashing; the rendered
  # manifests stay for inspection (and for a first winget submission).
  rm -rf "$DL" "$WORK/homebrew-tap" "$WORK/scoop-bucket" "$WORK/venv"
  # A run that stopped before rendering leaves nothing worth keeping.
  rmdir "$OUT" "$WORK" 2>/dev/null || true
}
trap cleanup EXIT

# ---------------------------------------------------------------------------
# 1. The release and its assets
# ---------------------------------------------------------------------------
DMG="Unterm-macos-$TAG.dmg"
ZIP_X64="Unterm-windows-x64-$TAG.zip"
ZIP_ARM64="Unterm-windows-arm64-$TAG.zip"
MSI_X64="Unterm-$VERSION-x64.msi"
MSI_ARM64="Unterm-$VERSION-arm64.msi"
ASSETS=("$DMG" "$ZIP_X64" "$ZIP_ARM64" "$MSI_X64" "$MSI_ARM64")

say "Checking release $TAG on $SOURCE_REPO"
# gh applies --jq to its own output only, so each field is its own (cheap)
# query rather than a dependency on a system jq.
gh_jq() {
  gh release view "$TAG" --repo "$SOURCE_REPO" \
    --json tagName,isDraft,isPrerelease,publishedAt,assets --jq "$1"
}
if ! err=$(gh release view "$TAG" --repo "$SOURCE_REPO" --json tagName 2>&1 >/dev/null); then
  die "release $TAG not found on $SOURCE_REPO: $err"
fi
[ "$(gh_jq .isDraft)" = "false" ] || die "$TAG is still a draft; publish it first"
if [ "$(gh_jq .isPrerelease)" = "true" ] && [ "${UNTERM_PUBLISH_ALLOW_PRERELEASE:-0}" != "1" ]; then
  die "$TAG is a prerelease; package managers track stable releases (UNTERM_PUBLISH_ALLOW_PRERELEASE=1 to override)"
fi
RELEASE_DATE=$(gh_jq '.publishedAt[0:10]')
asset_list=$(gh_jq '.assets[] | [.name, (.digest // "")] | @tsv')

missing=()
for asset in "${ASSETS[@]}"; do
  awk -F'\t' -v n="$asset" '$1==n{found=1} END{exit !found}' <<<"$asset_list" \
    || missing+=("$asset")
done
if [ "${#missing[@]}" -gt 0 ]; then
  echo "ERROR: $TAG is missing assets the packages need:" >&2
  printf '  %s\n' "${missing[@]}" >&2
  echo "The Windows assets come from the release-windows workflow, the dmg from" >&2
  echo "ci/release-mac.sh. Re-run this once they are uploaded." >&2
  exit 1
fi
note "published $RELEASE_DATE, all ${#ASSETS[@]} assets present"

# ---------------------------------------------------------------------------
# 2. Hashes, ProductCodes, zip layout
# ---------------------------------------------------------------------------
say "Downloading assets into $DL"
dl_args=()
for asset in "${ASSETS[@]}"; do dl_args+=(--pattern "$asset"); done
gh release download "$TAG" --repo "$SOURCE_REPO" --dir "$DL" --clobber "${dl_args[@]}"

declare_hash() {
  local asset="$1" got want
  got=$(sha256 "$DL/$asset")
  want=$(awk -F'\t' -v n="$asset" '$1==n{print $2}' <<<"$asset_list")
  want="${want#sha256:}"
  if [ -n "$want" ] && [ "$want" != "$got" ]; then
    die "$asset: downloaded sha256 $got but GitHub recorded $want"
  fi
  printf '%s' "$got"
}
SHA_DMG=$(declare_hash "$DMG")
SHA_ZIP_X64=$(declare_hash "$ZIP_X64")
SHA_ZIP_ARM64=$(declare_hash "$ZIP_ARM64")
SHA_MSI_X64=$(declare_hash "$MSI_X64")
SHA_MSI_ARM64=$(declare_hash "$MSI_ARM64")
for asset in "${ASSETS[@]}"; do note "$(sha256 "$DL/$asset")  $asset"; done

# The Scoop manifest's extract_dir and bin/shortcuts name these paths; if
# ci/deploy.sh ever changes the staging layout the manifest must follow.
ZIP_PREFIX="unterm-release-stage/unterm"
for zip in "$ZIP_X64" "$ZIP_ARM64"; do
  listing=$(unzip -Z1 "$DL/$zip")
  for exe in unterm.exe unterm-cli.exe unterm-core.exe; do
    grep -qxF "$ZIP_PREFIX/$exe" <<<"$listing" \
      || die "$zip has no $ZIP_PREFIX/$exe; update extract_dir in packaging/scoop/unterm.json.tmpl"
  done
done
note "zip layout matches extract_dir $ZIP_PREFIX"

# WiX generates a fresh ProductCode per build (Unterm.wxs sets none), so it
# has to be read back from each MSI. winget does not require it -- the fixed
# UpgradeCode in AppsAndFeaturesEntries already ties installs together -- but
# it lets winget match an installed copy exactly. komac reads it on its own.
msi_product_code() {
  local msi="$1" code="" py=python3
  if command -v msiinfo >/dev/null 2>&1; then
    code=$(msiinfo export "$msi" Property 2>/dev/null \
      | awk -F'\t' '$1=="ProductCode"{print $2}' | tr -d '\r') || code=""
  fi
  if [ -z "$code" ] && command -v python3 >/dev/null 2>&1; then
    # pymsi (PyPI: python-msi) in a throwaway venv when it is not installed.
    if ! python3 -c 'import pymsi' >/dev/null 2>&1; then
      if [ ! -x "$WORK/venv/bin/python" ]; then
        { python3 -m venv "$WORK/venv" \
            && "$WORK/venv/bin/pip" install --quiet python-msi; } >/dev/null 2>&1 \
          || { rm -rf "$WORK/venv"; return 0; }
      fi
      py="$WORK/venv/bin/python"
    fi
    code=$("$py" - "$msi" 2>/dev/null <<'PY'
import sys
from pathlib import Path
import pymsi
rows = pymsi.Package(Path(sys.argv[1])).get("Property").rows
print(next(r["Value"] for r in rows if r["Property"] == "ProductCode"))
PY
) || code=""
  fi
  if [[ "$code" =~ ^\{[0-9A-Fa-f]{8}(-[0-9A-Fa-f]{4}){3}-[0-9A-Fa-f]{12}\}$ ]]; then
    printf '%s' "$code"
  fi
}
PC_X64=$(msi_product_code "$DL/$MSI_X64")
PC_ARM64=$(msi_product_code "$DL/$MSI_ARM64")
if [ -n "$PC_X64" ] && [ -n "$PC_ARM64" ]; then
  note "ProductCode x64 $PC_X64, arm64 $PC_ARM64"
else
  warn "could not read the MSI ProductCodes (install msitools or python3); the winget installer manifest omits them"
  PC_X64="" PC_ARM64=""
fi

# ---------------------------------------------------------------------------
# 3. Render and lint
# ---------------------------------------------------------------------------
upper() { printf '%s' "$1" | tr '[:lower:]' '[:upper:]'; }

render() {
  local src="$1" dst="$2"
  mkdir -p "$(dirname "$dst")"
  sed \
    -e "s/@@VERSION@@/$VERSION/g" \
    -e "s/@@RELEASE_DATE@@/$RELEASE_DATE/g" \
    -e "s/@@SHA256_DMG@@/$SHA_DMG/g" \
    -e "s/@@SHA256_ZIP_X64@@/$SHA_ZIP_X64/g" \
    -e "s/@@SHA256_ZIP_ARM64@@/$SHA_ZIP_ARM64/g" \
    -e "s/@@SHA256_MSI_X64@@/$(upper "$SHA_MSI_X64")/g" \
    -e "s/@@SHA256_MSI_ARM64@@/$(upper "$SHA_MSI_ARM64")/g" \
    "$src" > "$dst"
  if [ -n "$PC_X64" ]; then
    sed -i.bak \
      -e "s/@@PRODUCTCODE_MSI_X64@@/$PC_X64/g" \
      -e "s/@@PRODUCTCODE_MSI_ARM64@@/$PC_ARM64/g" "$dst"
  else
    sed -i.bak -e '/@@PRODUCTCODE_MSI_/d' "$dst"
  fi
  rm -f "$dst.bak"
  if grep -n '@@[A-Z0-9_]*@@' "$dst" >&2; then
    die "unrendered placeholder left in $dst"
  fi
}

say "Rendering manifests into $OUT"
CASK="$OUT/homebrew/Casks/unterm.rb"
SCOOP="$OUT/scoop/bucket/unterm.json"
WINGET_DIR="$OUT/winget/$WINGET_PATH/$VERSION"
render "$PKG/homebrew/unterm.rb.tmpl" "$CASK"
render "$PKG/scoop/unterm.json.tmpl" "$SCOOP"
for tmpl in "$PKG"/winget/*.yaml.tmpl; do
  name=$(basename "$tmpl" .tmpl)
  render "$tmpl" "$WINGET_DIR/$name"
done

if command -v python3 >/dev/null 2>&1; then
  python3 -c 'import json,sys; json.load(open(sys.argv[1], encoding="utf-8"))' "$SCOOP" \
    || die "$SCOOP is not valid JSON"
  note "scoop manifest parses as JSON"
fi

if command -v brew >/dev/null 2>&1 && [ "${UNTERM_PUBLISH_SKIP_BREW_STYLE:-0}" != "1" ]; then
  # `brew style` only accepts casks that live in a tap, so lint through a
  # throwaway local tap (removed on exit; nothing is pushed).
  export HOMEBREW_NO_AUTO_UPDATE=1 HOMEBREW_NO_ANALYTICS=1
  brew untap --force "$LINT_TAP" >/dev/null 2>&1 || true
  brew tap-new --no-git "$LINT_TAP" >/dev/null
  LINT_TAP_CREATED=1
  lint_dir="$(brew --repository "$LINT_TAP")/Casks"
  mkdir -p "$lint_dir"
  cp "$CASK" "$lint_dir/unterm.rb"
  brew style --cask "$LINT_TAP/unterm" || die "brew style rejected the rendered cask ($CASK)"
  note "brew style: cask clean"
else
  note "brew not found (or UNTERM_PUBLISH_SKIP_BREW_STYLE=1); skipping brew style"
fi

KOMAC_URLS=(
  "https://github.com/$SOURCE_REPO/releases/download/$TAG/$MSI_X64"
  "https://github.com/$SOURCE_REPO/releases/download/$TAG/$MSI_ARM64"
)
KOMAC_CMD=(komac update "$WINGET_ID" --version "$VERSION"
  --urls "${KOMAC_URLS[@]}"
  --release-notes-url "https://github.com/$SOURCE_REPO/releases/tag/$TAG"
  --submit)

# Read-only: where winget-pkgs stands for this package.
winget_state() {
  if gh api "repos/microsoft/winget-pkgs/contents/$WINGET_PATH/$VERSION" >/dev/null 2>&1; then
    echo published
  elif gh api "repos/microsoft/winget-pkgs/contents/$WINGET_PATH" >/dev/null 2>&1; then
    echo update
  else
    echo new
  fi
}

# The in-app updater falls back to SHA256SUMS when the GitHub API is
# rate-limited; attach it before anything points people at this release.
if [ "$DRY_RUN" = 0 ]; then
  bash "$(dirname "$0")/release-checksums.sh" "$TAG"
  bash "$(dirname "$0")/release-notes.sh" "$TAG"
fi

if [ "$DRY_RUN" = 1 ]; then
  say "Dry run: nothing was cloned, pushed or submitted. Rendered files:"
  note "$CASK"
  note "$SCOOP"
  for f in "$WINGET_DIR"/*.yaml; do note "$f"; done
  case "$(winget_state)" in
    published)
      say "winget-pkgs already has $WINGET_ID $VERSION; the winget step would do nothing" ;;
    update)
      say "Without --dry-run the winget step would run:"
      note "${KOMAC_CMD[*]}" ;;
    new)
      say "$WINGET_ID is not in winget-pkgs yet; its first version goes in by hand:"
      note "komac submit '$WINGET_DIR'"
      note "(packaging/README.md, 'First winget submission')" ;;
  esac
  exit 0
fi

# ---------------------------------------------------------------------------
# 4. Publish
# ---------------------------------------------------------------------------
# Clone REPO, put SRC at DEST inside it, commit and push if it changed.
publish_file() {
  local repo="$1" src="$2" dest="$3" clone
  clone="$WORK/$(basename "$repo")"
  rm -rf "$clone"
  say "Updating $repo:$dest"
  if [ -n "${UNTERM_PUBLISH_GIT_BASE:-}" ]; then
    # Test hook: clone <base>/<repo name> (e.g. local bare repos) instead.
    git clone --quiet --depth 1 "$UNTERM_PUBLISH_GIT_BASE/$(basename "$repo")" "$clone"
  else
    gh repo clone "$repo" "$clone" -- --depth 1 --quiet
  fi
  mkdir -p "$clone/$(dirname "$dest")"
  cp "$src" "$clone/$dest"
  git -C "$clone" add "$dest"
  if git -C "$clone" diff --cached --quiet; then
    note "$dest already matches $VERSION; nothing to commit"
    return 0
  fi
  git -C "$clone" commit --quiet -m "unterm $VERSION"
  git -C "$clone" push --quiet origin HEAD
  note "pushed $(git -C "$clone" rev-parse --short HEAD) \"unterm $VERSION\""
}

publish_file "$TAP_REPO" "$CASK" "Casks/unterm.rb"
publish_file "$BUCKET_REPO" "$SCOOP" "bucket/unterm.json"

say "winget: $WINGET_ID $VERSION"
winget_manual() {
  echo "   The rendered manifests are kept in:"
  echo "     $WINGET_DIR"
}
case "$(winget_state)" in
  published)
    note "winget-pkgs already has $WINGET_ID $VERSION; nothing to submit" ;;
  new)
    warn "$WINGET_ID is not in microsoft/winget-pkgs yet, so 'komac update' cannot be used."
    echo "   First submission (once; see packaging/README.md, 'First winget submission'):"
    echo "     komac submit '$WINGET_DIR'"
    winget_manual ;;
  update)
    if command -v komac >/dev/null 2>&1; then
      # komac finds an open PR for the same version itself and stops there.
      "${KOMAC_CMD[@]}"
    else
      warn "komac is not installed; the winget update was NOT submitted."
      echo "   Install it (brew install komac, or winget install Russell.Komac), then run:"
      printf '     %q' "${KOMAC_CMD[@]}"; echo
      winget_manual
    fi ;;
esac

say "Done: $TAG published to Homebrew ($TAP_REPO) and Scoop ($BUCKET_REPO); winget as reported above."
