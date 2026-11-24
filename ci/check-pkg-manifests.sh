#!/bin/bash
# Verify that all release version declarations and package-manager templates
# stay in sync with the workspace version in Cargo.toml.
set -euo pipefail

ROOT=$(git rev-parse --show-toplevel)
cd "$ROOT"

WANT=$(awk '/^\[workspace\.package\]/{f=1} f && /^version *=/{gsub(/[",]/,"",$3); print $3; exit}' Cargo.toml)
if [ -z "$WANT" ]; then
  echo "ERROR: could not read [workspace.package] version from Cargo.toml" >&2
  exit 1
fi

echo "Checking release version consistency against Cargo.toml ($WANT)..."
FAIL=0

check_pattern() {
  local file="$1"
  local pattern="$2"
  if [ ! -f "$file" ]; then
    echo "  FAIL: missing file $file" >&2
    FAIL=1
    return
  fi
  if grep -Eq "$pattern" "$file"; then
    echo "  OK:   $file"
  else
    echo "  FAIL: $file does not match expected version $WANT" >&2
    FAIL=1
  fi
}

check_pattern "unterm-agents/Cargo.toml" "^version = \"$WANT\""
check_pattern "unterm-profile/Cargo.toml" "^version = \"$WANT\""
check_pattern "unterm-settings/Cargo.toml" "^version = \"$WANT\""
check_pattern "installer/Unterm.wxs" "Version=\"$WANT\""
check_pattern "web/functions/api/download.ts" "v$WANT"
check_pattern "web/src/lib/stats.ts" "v$WANT"

# Verify packaging templates exist for Homebrew, Scoop, and winget.
for tmpl in \
  packaging/homebrew/unterm.rb.tmpl \
  packaging/scoop/unterm.json.tmpl \
  packaging/winget/zhitongblog.Unterm.yaml.tmpl \
  packaging/winget/zhitongblog.Unterm.installer.yaml.tmpl; do
  if [ -f "$tmpl" ]; then
    echo "  OK:   $tmpl present"
  else
    echo "  FAIL: missing packaging template $tmpl" >&2
    FAIL=1
  fi
done

if [ "$FAIL" -ne 0 ]; then
  echo "Version consistency check FAILED." >&2
  exit 1
fi

echo "All release version declarations and packaging templates are consistent ($WANT)."
