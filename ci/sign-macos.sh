#!/bin/bash
# Sign and (optionally) notarize a local macOS Unterm.app build, then wrap
# it in a signed + notarized .dmg ready for distribution.
#
# Usage:
#   ci/sign-macos.sh                          # sign only, no notarize, no DMG
#   NOTARY_PROFILE=UntermNotary ci/sign-macos.sh
#
# To create the notary profile once (interactive — you'll be asked for Apple ID
# and an app-specific password from https://account.apple.com/account/manage):
#   xcrun notarytool store-credentials UntermNotary \
#     --apple-id <your-apple-id> \
#     --team-id 6NQM3XP5RF
set -euo pipefail
set -x


# codesign with retries: the --timestamp flag needs timestamp.apple.com, which
# flakes behind rotating proxies / spotty networks. A single flake used to
# kill the whole release run; retry with a pause instead (the timestamp
# service outages we see are seconds-to-a-minute long).
codesign_retry() {
  local i
  for i in 1 2 3 4 5 6; do
    if /usr/bin/codesign "$@"; then
      return 0
    fi
    echo "codesign attempt $i failed (timestamp service flake?) — retrying in 20s" >&2
    sleep 20
  done
  echo "codesign failed after 6 attempts" >&2
  return 1
}

ROOT=$(git rev-parse --show-toplevel)
cd "$ROOT"

TARGET_DIR=${TARGET_DIR:-target}
TAG_NAME=${TAG_NAME:-local-$(date +%Y%m%d-%H%M%S)}
DEV_ID=${DEV_ID:-"Developer ID Application: xiangdong li (6NQM3XP5RF)"}
NOTARY_PROFILE=${NOTARY_PROFILE:-}

# Stage the .app under Unterm-macos-<tag>/Unterm.app
stagedir=Unterm-macos-$TAG_NAME
dmgname=$stagedir.dmg

# Notarize an artifact (zip or dmg), waiting for the verdict. The macOS
# keychain has twice pruned the UntermNotary credential MID-RELEASE
# (pre-check passed, the actual submit failed), so when the profile is
# missing fall back to inline credentials from ~/.unterm/notary-credentials
# (KEY=VALUE lines: NOTARY_APPLE_ID / NOTARY_TEAM_ID / NOTARY_PASSWORD,
# chmod 600).
notary_submit() {
  artifact="$1"
  # The S3 upload behind notarytool flakes behind rotating proxy nodes
  # (deadlineExceeded mid-multipart, TLS -9816); an aborted upload leaves
  # its submission id In Progress forever, so the only correct move is a
  # fresh submit. Retry the whole submit, never wait on the dead id.
  local i
  for i in 1 2 3 4 5; do
    if notary_submit_once "$artifact"; then
      return 0
    fi
    echo "notarytool submit attempt $i failed (upload flake?) — retrying in 30s" >&2
    sleep 30
  done
  echo "notarytool submit failed after 5 attempts" >&2
  return 1
}

notary_submit_once() {
  artifact="$1"
  if xcrun notarytool history --keychain-profile "$NOTARY_PROFILE" >/dev/null 2>&1 ; then
    xcrun notarytool submit "$artifact" --keychain-profile "$NOTARY_PROFILE" --wait
    return
  fi
  creds="$HOME/.unterm/notary-credentials"
  if [ ! -f "$creds" ] ; then
    echo "ERROR: keychain profile '$NOTARY_PROFILE' is gone and $creds does not exist." >&2
    echo "Create it with NOTARY_APPLE_ID / NOTARY_TEAM_ID / NOTARY_PASSWORD lines (chmod 600)." >&2
    exit 1
  fi
  echo "keychain profile '$NOTARY_PROFILE' missing — using inline credentials from $creds"
  # shellcheck disable=SC1090
  . "$creds"
  xcrun notarytool submit "$artifact" --apple-id "$NOTARY_APPLE_ID" \
    --team-id "$NOTARY_TEAM_ID" --password "$NOTARY_PASSWORD" --wait
}
rm -rf "$stagedir" "$dmgname"
mkdir "$stagedir"
cp -r assets/macos/Unterm.app "$stagedir/"
rm -f "$stagedir/Unterm.app/"*.dylib
mkdir -p "$stagedir/Unterm.app/Contents/MacOS"
mkdir -p "$stagedir/Unterm.app/Contents/Resources"
cp -r assets/shell-integration/* "$stagedir/Unterm.app/Contents/Resources"
cp -r assets/shell-completion "$stagedir/Unterm.app/Contents/Resources"
# The example config, shipped for reference. Unterm reads only
# ~/.unterm/unterm.conf; this copy is never loaded on its own.
cp assets/unterm.conf "$stagedir/Unterm.app/Contents/Resources/unterm.conf"
# The bundled faces: the chrome's icon language (Symbols Nerd Font), emoji,
# and the DEFAULT terminal font (JetBrains Mono Regular, opened by file name).
# The v0.61.0 DMG shipped without any of these -- every icon in an installed
# copy was an empty box and the rows fell back to the system monospace.
mkdir -p "$stagedir/Unterm.app/Contents/Resources/assets/fonts"
cp assets/fonts/SymbolsNerdFontMono-Regular.ttf \
   assets/fonts/NotoColorEmoji.ttf \
   assets/fonts/JetBrainsMono-Regular.ttf \
   "$stagedir/Unterm.app/Contents/Resources/assets/fonts/"
tic -xe wezterm -o "$stagedir/Unterm.app/Contents/Resources/terminfo" termwiz/data/wezterm.terminfo

# Stamp CFBundleShortVersionString on the main app from the tag, so Finder
# Get Info shows the real release version instead of the stale literal in
# the template Info.plist. build-macos-finder-sync.sh does the same for the
# appex; we just mirror that pattern here for the outer bundle.
if [ -n "${TAG_NAME:-}" ]; then
  short_version="${TAG_NAME#v}"
  # Pad "0.20" → "0.20.0" so Finder shows a three-part SemVer that matches
  # Cargo.toml + Unterm.wxs, rather than the abbreviated tag.
  if [[ "$short_version" =~ ^[0-9]+\.[0-9]+$ ]]; then
    short_version="${short_version}.0"
  fi
  /usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $short_version" \
    "$stagedir/Unterm.app/Contents/Info.plist"
fi

bash ci/build-macos-finder-sync.sh "$stagedir/Unterm.app"

for bin in unterm unterm-cli unterm-core ; do
  # Prefer the per-arch builds (target/<triple>/release/$bin) and lipo them
  # together into a fat universal binary. We only fall back to the host-arch
  # direct path (target/release/$bin) if no per-arch builds exist at all —
  # otherwise we'd happily ship a stale single-arch binary from an earlier
  # `cargo build --release` (no --target) when newer per-arch builds are
  # sitting right next to it. (We ate this on 2026-05-01: shipped a v0.5.1
  # DMG whose binary predated the scrollback feature it claimed to include.)
  # Guard against stale per-arch artifacts (the 2026-05-01 and 2026-06-10
  # trap): if a plain host-arch build is NEWER than the per-arch ones, the
  # per-arch dirs are leftovers from an older release — prefer the fresh
  # build instead of silently shipping outdated binaries.
  use_per_arch=false
  if compgen -G "$TARGET_DIR/*/release/$bin" >/dev/null ; then
    use_per_arch=true
    if [[ -f "$TARGET_DIR/release/$bin" ]]; then
      newest_arch=$(ls -t "$TARGET_DIR"/*/release/$bin | head -1)
      if [[ "$TARGET_DIR/release/$bin" -nt "$newest_arch" ]]; then
        echo "WARNING: $TARGET_DIR/release/$bin is newer than per-arch builds; using it (stale per-arch artifacts ignored)" >&2
        use_per_arch=false
      fi
    fi
  fi
  if $use_per_arch ; then
    lipo "$TARGET_DIR"/*/release/$bin -output "$stagedir/Unterm.app/Contents/MacOS/$bin" -create
  elif [[ -f "$TARGET_DIR/release/$bin" ]] ; then
    cp "$TARGET_DIR/release/$bin" "$stagedir/Unterm.app/Contents/MacOS/$bin"
  else
    echo "ERROR: missing build artifact $bin — run 'cargo build --release -p unterm-app -p unterm-cli -p unterm-core' first"
    exit 1
  fi
done

# Sign every binary individually, then the bundle.
#
# Order matters: codesign walks bundle subcomponents while signing the main
# executable, and refuses with "code object is not signed at all" if it
# encounters a sibling binary that hasn't been signed yet. Iterating
# alphabetically (the previous bug) put `unterm` before `unterm-mux` and
# tripped that check. Sign helpers first, main last, then the bundle.
HELPERS=(unterm-cli unterm-core unterm-mux strip-ansi-escapes)
for bin in "${HELPERS[@]}" ; do
  bin_path="$stagedir/Unterm.app/Contents/MacOS/$bin"
  if [[ -f "$bin_path" ]] ; then
    codesign_retry --force --options runtime --timestamp \
      --entitlements ci/macos-entitlement.plist \
      --sign "$DEV_ID" "$bin_path"
  fi
done

if [[ -d "$stagedir/Unterm.app/Contents/PlugIns/UntermFinderSync.appex" ]] ; then
  # App Sandbox entitlement is MANDATORY for FinderSync extensions; without it
  # pluginkit silently refuses to register the appex and the right-click
  # "Open in Unterm" never appears. We shipped the appex with no entitlements
  # through v0.23 and the Repair Finder Integration script could never recover
  # — only re-signing with --entitlements (and fixing the principal class
  # name in assets/macos/FinderSync/Info.plist) makes the extension load.
  codesign_retry --force --options runtime --timestamp \
    --entitlements ci/macos-finder-sync-entitlement.plist \
    --sign "$DEV_ID" "$stagedir/Unterm.app/Contents/PlugIns/UntermFinderSync.appex"
fi

codesign_retry --force --options runtime --timestamp \
  --entitlements ci/macos-entitlement.plist \
  --sign "$DEV_ID" "$stagedir/Unterm.app/Contents/MacOS/unterm"
codesign_retry --force --options runtime --timestamp \
  --entitlements ci/macos-entitlement.plist \
  --sign "$DEV_ID" "$stagedir/Unterm.app"

/usr/bin/codesign --verify --strict --verbose=2 "$stagedir/Unterm.app"

if [ -n "$NOTARY_PROFILE" ] ; then
  # Notarize the .app first, via a transient zip — Apple's notary service
  # accepts both .zip and .dmg, and zipping the .app is the cheapest container.
  notary_zip="$stagedir.notary.zip"
  rm -f "$notary_zip"
  ditto -c -k --keepParent "$stagedir/Unterm.app" "$notary_zip"
  echo "Submitting .app to Apple notary service via profile ${NOTARY_PROFILE}..."
  notary_submit "$notary_zip"
  rm -f "$notary_zip"
  xcrun stapler staple "$stagedir/Unterm.app"
fi

# Build the .dmg. We give it a clean drag-to-install layout: the .app and a
# symlink to /Applications side by side in the mounted volume root.
dmg_stage="$stagedir.dmg-stage"
rm -rf "$dmg_stage"
mkdir "$dmg_stage"
cp -R "$stagedir/Unterm.app" "$dmg_stage/Unterm.app"
cp -R "assets/macos/Repair Finder Integration.app" "$dmg_stage/"
chmod +x "$dmg_stage/Repair Finder Integration.app/Contents/MacOS/repair-finder-integration"
if [ -n "${TAG_NAME:-}" ]; then
  /usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $short_version" \
    "$dmg_stage/Repair Finder Integration.app/Contents/Info.plist"
fi
codesign_retry --force --options runtime --timestamp \
  --sign "$DEV_ID" "$dmg_stage/Repair Finder Integration.app"
ln -s /Applications "$dmg_stage/Applications"
hdiutil create -volname "Unterm" -srcfolder "$dmg_stage" \
  -ov -format UDZO "$dmgname"
rm -rf "$dmg_stage"

# Sign the DMG so Gatekeeper trusts the container itself, not just the .app
# inside. `--timestamp` adds an Apple-server timestamp so verification keeps
# working after the cert eventually expires.
codesign_retry --force --sign "$DEV_ID" --timestamp "$dmgname"
/usr/bin/codesign --verify --verbose=2 "$dmgname"

if [ -n "$NOTARY_PROFILE" ] ; then
  echo "Submitting .dmg to Apple notary service via profile ${NOTARY_PROFILE}..."
  notary_submit "$dmgname"
  xcrun stapler staple "$dmgname"
  spctl --assess --type install --verbose "$dmgname" || true
fi

set +x
echo "Signed: $stagedir/Unterm.app"
echo "DMG:    $dmgname"
# Use a real `if` (not `&& echo`) so the exit status of this script is always
# 0 on success — under `set -e`, a `[ -z "$X" ] && echo` short-circuit returns
# non-zero when $X is set, which would tank any caller that pipefails on us.
if [ -z "$NOTARY_PROFILE" ]; then
  echo "NOTE: not notarized — set NOTARY_PROFILE=<name> after running 'xcrun notarytool store-credentials <name>'"
fi
