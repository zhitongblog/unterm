#!/bin/sh
# Unterm one-shot installer for macOS and Linux.
#
# Usage:
#   curl -fsSL https://unterm.app/install.sh | sh
#
# What it does:
#   - Detects OS (darwin / linux) and architecture (x86_64 / arm64).
#   - Resolves the latest release tag via the GitHub API.
#   - macOS:  downloads the universal .dmg, mounts it, copies Unterm.app
#             to /Applications, ejects.
#   - Linux:  downloads the .deb if dpkg + apt are available; otherwise
#             grabs the .AppImage and drops it into ~/.local/bin.
#
# Re-running upgrades in place. Set UNTERM_VERSION=v0.5.2 to pin a
# specific tag. Pipe `| sh -s -- --dry-run` to print actions only.

set -eu

REPO="zhitongblog/unterm"
DRY_RUN=0
UNTERM_VERSION=${UNTERM_VERSION:-}

# Color helpers — only when stderr is a TTY, so piping to a log stays clean.
if [ -t 2 ]; then
  c_blue='\033[1;34m'; c_green='\033[1;32m'; c_yellow='\033[1;33m'
  c_red='\033[1;31m'; c_reset='\033[0m'
else
  c_blue=''; c_green=''; c_yellow=''; c_red=''; c_reset=''
fi
say()  { printf "${c_blue}» %s${c_reset}\n" "$*" >&2; }
ok()   { printf "${c_green}✓ %s${c_reset}\n" "$*" >&2; }
warn() { printf "${c_yellow}! %s${c_reset}\n" "$*" >&2; }
die()  { printf "${c_red}✗ %s${c_reset}\n" "$*" >&2; exit 1; }
run()  {
  if [ "$DRY_RUN" -eq 1 ]; then printf "  [dry-run] %s\n" "$*" >&2
  else eval "$@"; fi
}

# --- Argument parsing ------------------------------------------------------
for arg in "$@"; do
  case "$arg" in
    --dry-run) DRY_RUN=1 ;;
    --version=*) UNTERM_VERSION="${arg#--version=}" ;;
    -h|--help)
      cat <<EOF
Usage: install.sh [--dry-run] [--version=vX.Y.Z]
EOF
      exit 0 ;;
    *) die "unknown argument: $arg" ;;
  esac
done

# --- Platform detection ----------------------------------------------------
os=$(uname -s | tr '[:upper:]' '[:lower:]')
arch=$(uname -m)
case "$arch" in
  x86_64|amd64)  arch=x86_64 ;;
  arm64|aarch64) arch=arm64  ;;
  *) die "unsupported architecture: $arch" ;;
esac

# Need either curl or wget — picked at runtime.
if command -v curl >/dev/null 2>&1; then
  fetch() { curl -fsSL "$1" -o "$2"; }
  fetch_stdout() { curl -fsSL "$1"; }
elif command -v wget >/dev/null 2>&1; then
  fetch() { wget -qO "$2" "$1"; }
  fetch_stdout() { wget -qO- "$1"; }
else
  die "need curl or wget on PATH"
fi

# --- Resolve target tag ---------------------------------------------------
if [ -z "$UNTERM_VERSION" ]; then
  say "looking up latest Unterm release..."
  # The API returns a JSON blob; grab the `tag_name` value with a tolerant
  # regex so we don't drag in jq as a dependency.
  api="https://api.github.com/repos/$REPO/releases/latest"
  UNTERM_VERSION=$(fetch_stdout "$api" 2>/dev/null | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -1)
  if [ -z "$UNTERM_VERSION" ]; then
    # 60 anonymous API requests an hour per address, shared by an office or
    # a VPN: the release page's redirect names the tag without the API.
    warn "GitHub API unavailable; reading the release page instead"
    if command -v curl >/dev/null 2>&1; then
      loc=$(curl -fsSI "https://github.com/$REPO/releases/latest" | tr -d '\r' | sed -n 's/^[Ll]ocation: *//p' | head -1)
    else
      loc=$(wget -S --max-redirect=0 -O /dev/null "https://github.com/$REPO/releases/latest" 2>&1 | sed -n 's/^ *[Ll]ocation: *//p' | head -1)
    fi
    UNTERM_VERSION=${loc##*/tag/}
    case "$UNTERM_VERSION" in v*) ;; *) UNTERM_VERSION="" ;; esac
  fi
  [ -n "$UNTERM_VERSION" ] || die "couldn't resolve the latest release from GitHub"
fi
ok "Unterm $UNTERM_VERSION"
DL_BASE="https://github.com/$REPO/releases/download/$UNTERM_VERSION"

# --- macOS ----------------------------------------------------------------
if [ "$os" = "darwin" ]; then
  asset="Unterm-macos-$UNTERM_VERSION.dmg"
  url="$DL_BASE/$asset"
  tmp=$(mktemp -d)
  trap 'rm -rf "$tmp"' EXIT

  say "downloading $asset (~120 MB)"
  run "fetch '$url' '$tmp/$asset'"

  mountpoint="$tmp/mnt"
  say "mounting"
  run "hdiutil attach -nobrowse -mountpoint '$mountpoint' '$tmp/$asset' >/dev/null"

  if [ -d /Applications/Unterm.app ]; then
    warn "/Applications/Unterm.app exists — quitting and replacing"
    run "osascript -e 'tell application \"unterm\" to quit' 2>/dev/null || true"
    sleep 1
    run "rm -rf /Applications/Unterm.app"
  fi
  say "copying Unterm.app to /Applications"
  run "cp -R '$mountpoint/Unterm.app' /Applications/"

  say "ejecting"
  run "hdiutil detach '$mountpoint' >/dev/null"

  ok "installed to /Applications/Unterm.app"
  say "launch with: open /Applications/Unterm.app"
  exit 0
fi

# --- Linux ----------------------------------------------------------------
if [ "$os" = "linux" ]; then
  # x86_64 and arm64 both ship. Map the normalized arch to the per-arch asset
  # naming the release pipeline produces: .deb tags non-amd64 with `.arm64`
  # (ci/deploy.sh), AppImage names carry `uname -m` (ci/appimage.sh).
  case "$arch" in
    x86_64) deb_suffix=""; appimage_arch="x86_64" ;;
    arm64)  deb_suffix=".arm64"; appimage_arch="aarch64" ;;
    *) die "Linux ships x86_64 and arm64 (arch=$arch). Build from source: https://github.com/$REPO" ;;
  esac

  # Prefer .deb when dpkg + apt exist (Debian/Ubuntu/Mint/Pop!_OS); apt
  # resolves runtime deps, integrates with system, gets a `unterm`
  # command on PATH automatically.
  if command -v dpkg >/dev/null 2>&1 && command -v apt >/dev/null 2>&1; then
    asset="unterm-$UNTERM_VERSION$deb_suffix.deb"
    url="$DL_BASE/$asset"
    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT

    say "downloading $asset"
    run "fetch '$url' '$tmp/$asset'"

    say "installing (apt may ask for sudo to resolve deps)"
    if [ "$(id -u)" -ne 0 ] && command -v sudo >/dev/null 2>&1; then
      run "sudo apt update"
      run "sudo apt install -y '$tmp/$asset'"
    else
      run "apt update"
      run "apt install -y '$tmp/$asset'"
    fi
    ok "installed via apt"
    say "launch with: unterm"
    exit 0
  fi

  # Fallback: AppImage into ~/.local/bin/unterm. No deps assumed beyond
  # what every desktop Linux ships; AppImage bundles its own libraries.
  asset="Unterm-$UNTERM_VERSION-$appimage_arch.AppImage"
  url="$DL_BASE/$asset"
  dest_dir="$HOME/.local/bin"
  dest="$dest_dir/unterm"
  mkdir -p "$dest_dir"

  say "downloading $asset"
  run "fetch '$url' '$dest'"
  run "chmod +x '$dest'"

  ok "installed to $dest"
  case ":$PATH:" in
    *":$dest_dir:"*) ;;
    *) warn "$dest_dir is not on PATH — add 'export PATH=\"\$HOME/.local/bin:\$PATH\"' to your shell rc" ;;
  esac
  say "launch with: unterm"
  exit 0
fi

die "unsupported OS: $os (this script handles darwin and linux; for Windows use install.ps1)"
