#!/bin/bash
# Build, sign, notarize, and upload the macOS Unterm.app for the tag at HEAD.
#
# One-time setup (per machine):
#
#   1. Make sure your Developer ID Application cert is in the Mac Keychain
#      (Apple's developer portal → Certificates → Developer ID Application).
#
#   2. Stash an app-specific password for notarization in the Keychain:
#
#        xcrun notarytool store-credentials UntermNotary \
#          --apple-id <your-apple-id> \
#          --team-id 6NQM3XP5RF
#
#      (App-specific passwords are generated at
#       https://account.apple.com/account/manage → Sign-In and Security →
#       App-Specific Passwords.)
#
# Usage:
#
#   make release-mac                                 # uses NOTARY_PROFILE=UntermNotary
#   make release-mac NOTARY_PROFILE=OtherProfile     # different stored profile
#
# What it does:
#
#   - Confirms HEAD has an annotated tag.
#   - Builds release universal (x86_64 + aarch64) for unterm, unterm-cli and unterm-core.
#   - Calls ci/sign-macos.sh which signs, notarizes, staples, and zips the .app.
#   - Uploads the resulting zip onto the matching GitHub Release.
set -euo pipefail

ROOT=$(git rev-parse --show-toplevel)
cd "$ROOT"

NOTARY_PROFILE=${NOTARY_PROFILE:-UntermNotary}

TAG=$(git describe --tags --exact-match HEAD 2>/dev/null || true)
if [ -z "$TAG" ]; then
  echo "ERROR: HEAD has no tag. Tag a release first:" >&2
  echo "  git tag -a vX.Y.Z -m 'Unterm vX.Y.Z' && git push origin vX.Y.Z" >&2
  exit 1
fi

# The tag has to name the version actually being built. Nothing downstream
# would catch the drift: the DMG is named from the tag, so a stale bump ships
# 0.65 binaries inside a file called 0.66 and every later bug report cites a
# version that was never built.
WANT_VERSION="${TAG#v}"
HAVE_VERSION=$(awk '/^\[workspace.package\]/{f=1} f && /^version *=/{gsub(/[",]/,"",$3); print $3; exit}' Cargo.toml)
if [ "$WANT_VERSION" != "$HAVE_VERSION" ]; then
  echo "ERROR: tag $TAG does not match the workspace version $HAVE_VERSION." >&2
  echo "Fix the version bump or the tag before releasing." >&2
  exit 1
fi

if ! xcrun notarytool history --keychain-profile "$NOTARY_PROFILE" >/dev/null 2>&1 \
   && [ ! -f "$HOME/.unterm/notary-credentials" ]; then
  echo "ERROR: Notary profile '$NOTARY_PROFILE' not found in Keychain." >&2
  echo >&2
  echo "This has been observed to happen between releases — the macOS" >&2
  echo "keychain occasionally prunes the notarytool credential. To" >&2
  echo "restore it:" >&2
  echo >&2
  echo "  1. The three values are already on this machine, in" >&2
  echo "     ~/.unterm/notary-credentials (chmod 600). This script and" >&2
  echo "     ci/sign-macos.sh fall back to it when the Keychain has no" >&2
  echo "     profile, so a missing profile need not stop a release." >&2
  echo >&2
  echo "  2. To put the Keychain profile back:" >&2
  echo "       set -a; . ~/.unterm/notary-credentials; set +a" >&2
  echo "       xcrun notarytool store-credentials $NOTARY_PROFILE \\" >&2
  echo "         --apple-id \"$NOTARY_APPLE_ID\" --team-id \"$NOTARY_TEAM_ID\" \\" >&2
  echo "         --password \"$NOTARY_PASSWORD\"" >&2
  echo >&2
  echo "     The Apple ID is slushy@139.com, not the git author's address —" >&2
  echo "     a hint here once named the wrong one and cost an afternoon." >&2
  echo "     Writing to the Keychain needs a session that may prompt; a" >&2
  echo "     non-interactive shell gets \"User interaction is not allowed\"" >&2
  echo "     even when the credentials themselves validate." >&2
  echo >&2
  echo "  3. If the password itself is gone, get a new one from:" >&2
  echo "     https://account.apple.com/account/manage" >&2
  echo "     → Sign-In and Security → App-Specific Passwords" >&2
  echo >&2
  echo "  4. Re-run: make release-mac" >&2
  echo >&2
  echo "(See ci/release-mac.sh:44 for the pre-check that raised this.)" >&2
  exit 1
fi

if ! command -v gh >/dev/null 2>&1; then
  echo "ERROR: 'gh' CLI not found. Install with: brew install gh" >&2
  exit 1
fi

echo ">> Building universal release for $TAG"
rustup target add x86_64-apple-darwin aarch64-apple-darwin >/dev/null
for triple in x86_64-apple-darwin aarch64-apple-darwin; do
  cargo build --release --target "$triple" \
    -p unterm-app -p unterm-cli -p unterm-core
done

echo ">> Signing + notarizing as $TAG"
TAG_NAME="$TAG" NOTARY_PROFILE="$NOTARY_PROFILE" bash ci/sign-macos.sh

dmg="Unterm-macos-$TAG.dmg"
if [ ! -f "$dmg" ]; then
  echo "ERROR: expected $dmg not produced by ci/sign-macos.sh" >&2
  exit 1
fi

# Existence is not identity: ask the binaries inside the finished DMG what
# version they are, before anyone can download them.
echo ">> Probing $dmg for the version it actually carries"
probe_mount=$(mktemp -d)
hdiutil attach -nobrowse -quiet -mountpoint "$probe_mount" "$dmg"
probe_failed=""
for binary in unterm unterm-cli unterm-core ; do
  got=$("$probe_mount/Unterm.app/Contents/MacOS/$binary" --version 2>/dev/null | awk '{print $NF}')
  if [ "$got" != "$WANT_VERSION" ]; then
    echo "ERROR: DMG ships $binary '$got', expected $WANT_VERSION" >&2
    probe_failed="yes"
  else
    echo "   $binary $got"
  fi
done
hdiutil detach "$probe_mount" -quiet || true
rmdir "$probe_mount" 2>/dev/null || true
if [ -n "$probe_failed" ]; then
  exit 1
fi

echo ">> Uploading $dmg to release $TAG"
# `--clobber` so re-runs just overwrite the asset; `gh release create` first if
# the release doesn't exist yet (the Linux/Windows workflows usually create it).
if ! gh release view "$TAG" >/dev/null 2>&1; then
  # Losing this race is not a failure. The Linux and Windows workflows create
  # the same release, and one of them can land between the check above and
  # this line -- which is exactly what happened on 0.71.2: the whole script
  # exited 1 with a signed, notarised, stapled dmg sitting there unuploaded,
  # for the one reason that did not matter.
  if ! gh release create "$TAG" --title "Unterm $TAG" --notes "Unterm $TAG"; then
    gh release view "$TAG" >/dev/null 2>&1 ||
      { echo "release $TAG could not be created or found" >&2; exit 1; }
  fi
fi
gh release upload "$TAG" "$dmg" --clobber

# Existence is not the same as having uploaded it. On 0.70.0 the upload died
# mid-transfer with an EOF and said so only in a line nobody read; the release
# then shipped without a macOS build until someone looked.
if ! gh release view "$TAG" --json assets --jq '.assets[].name' | grep -qx "$(basename "$dmg")"; then
  echo "upload reported success but $dmg is not on release $TAG" >&2
  exit 1
fi

echo ">> Done. Asset $dmg attached to release $TAG."
