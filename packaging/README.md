# Package-manager channels

Unterm ships through three package managers besides the GitHub release itself:

| Channel  | Where the manifest lives                                   | Installs                         |
|----------|------------------------------------------------------------|----------------------------------|
| Homebrew | `zhitongblog/homebrew-tap`, `Casks/unterm.rb`              | macOS dmg (universal)            |
| Scoop    | `zhitongblog/scoop-bucket`, `bucket/unterm.json`           | Windows zip (x64, arm64)         |
| winget   | `microsoft/winget-pkgs`, `manifests/z/zhitongblog/Unterm/` | Windows MSI (x64, arm64)         |

The files in this directory are templates. `ci/publish-packages.sh` fills them
in for one release (version, sha256 of each asset, MSI ProductCodes, release
date) and publishes the result.

```
packaging/
  homebrew/unterm.rb.tmpl                          cask
  scoop/unterm.json.tmpl                           Scoop manifest
  winget/zhitongblog.Unterm.yaml.tmpl              version manifest
  winget/zhitongblog.Unterm.installer.yaml.tmpl    installer manifest (wix)
  winget/zhitongblog.Unterm.locale.en-US.yaml.tmpl default locale
  winget/zhitongblog.Unterm.locale.zh-CN.yaml.tmpl zh-CN locale
```

Placeholders are `@@NAME@@` (`@@VERSION@@`, `@@SHA256_DMG@@`,
`@@SHA256_ZIP_X64@@`, `@@SHA256_ZIP_ARM64@@`, `@@SHA256_MSI_X64@@`,
`@@SHA256_MSI_ARM64@@`, `@@PRODUCTCODE_MSI_X64@@`, `@@PRODUCTCODE_MSI_ARM64@@`,
`@@RELEASE_DATE@@`). Do not use `{{...}}` for them: `{{appdir}}` in the cask is
a Homebrew install-step token and must reach Homebrew as written.

## For users

macOS (Homebrew):

```bash
brew install --cask zhitongblog/tap/unterm
brew upgrade --cask unterm          # later
```

This installs `/Applications/Unterm.app` and puts `unterm-cli` on the PATH.

Windows (winget):

```powershell
winget install zhitongblog.Unterm
winget upgrade zhitongblog.Unterm   # later
```

This runs the same MSI as the release page (per-machine, UAC prompt, Explorer
"Open in Unterm" entries). The MSI does not add itself to PATH, so
`unterm-cli` is at `C:\Program Files\Unterm\unterm-cli.exe`.

Windows (Scoop):

```powershell
scoop bucket add zhitongblog https://github.com/zhitongblog/scoop-bucket
scoop install zhitongblog/unterm
scoop update unterm                 # later; quit Unterm first (unterm-cli quit --all)
```

Per-user, no admin, `unterm-cli` on the PATH, a Start-menu shortcut for
Unterm. No Explorer context-menu entries (those come with the MSI).

## Per release

After the GitHub release has all of its assets (CI uploads the Windows and
Linux ones; `make release-mac` / `ci/release-mac.sh` uploads the dmg):

```bash
bash ci/publish-packages.sh vX.Y.Z --dry-run   # renders + lints, touches nothing remote
bash ci/publish-packages.sh vX.Y.Z
```

The real run:

1. checks that the release is published and has the dmg, both Windows zips
   and both MSIs; downloads them and checks each sha256 against the digest
   GitHub recorded;
2. renders and lints the manifests (JSON parse, `brew style` through a
   throwaway local tap);
3. pushes `Casks/unterm.rb` to `zhitongblog/homebrew-tap` and
   `bucket/unterm.json` to `zhitongblog/scoop-bucket`, each as a commit
   `unterm X.Y.Z` (skipped when the file is already identical, so a re-run is
   harmless);
4. runs `komac update zhitongblog.Unterm --version X.Y.Z --urls <x64 msi>
   <arm64 msi> --release-notes-url ... --submit`, which opens a PR against
   `microsoft/winget-pkgs` from your fork. If komac is missing it prints that
   command instead; if the package is not in winget-pkgs yet it prints the
   first-submission command below. Neither case fails the script.

Needs `gh` (logged in with push access to the two repos), `git`, `unzip`,
`shasum`. Optional: `msiinfo` (`brew install msitools`) or `python3` to read
the MSI ProductCodes (without either they are left out of the winget
manifest, which is allowed), `brew` for the cask lint, `komac` for winget.

Environment knobs: `UNTERM_PUBLISH_ALLOW_PRERELEASE=1`,
`UNTERM_PUBLISH_SKIP_BREW_STYLE=1`, `UNTERM_TAP_REPO` / `UNTERM_BUCKET_REPO` /
`UNTERM_SOURCE_REPO` to point at other repos, and `UNTERM_PUBLISH_GIT_BASE` (a
directory of bare repos named `homebrew-tap` and `scoop-bucket`) to rehearse
the push step locally.

If this script is ever skipped, both repos can still be bumped with the
managers' own tooling: the cask's `livecheck` follows GitHub's latest release
(`brew livecheck --cask zhitongblog/tap/unterm` reports a new version), and the
Scoop manifest's `checkver`/`autoupdate` drive `checkver.ps1 unterm -u`.

## One-time setup

### Homebrew tap and Scoop bucket

Both repos already exist (they also carry Ziplark and unflick). Had they not,
this is how they were made:

```bash
gh repo create zhitongblog/homebrew-tap --public \
  --description "Homebrew tap for zhitongblog apps"
gh repo create zhitongblog/scoop-bucket --public \
  --description "Scoop bucket for zhitongblog apps"
```

with this layout (the tap must be named `homebrew-*` for `zhitongblog/tap`
to resolve to it):

```
homebrew-tap/              scoop-bucket/
  README.md                  README.md
  Casks/unterm.rb            bucket/unterm.json
```

The first `bash ci/publish-packages.sh vX.Y.Z` creates `Casks/unterm.rb` and
`bucket/unterm.json`. Add an Unterm section to each repo's README by hand
(install commands from "For users" above).

### komac (winget)

```bash
brew install komac
komac token update        # a classic GitHub token with the public_repo scope
```

komac forks `microsoft/winget-pkgs` into the token's account on first use and
opens PRs from there. `GITHUB_TOKEN` in the environment works too.

### First winget submission

`komac update` only works once `zhitongblog.Unterm` exists in winget-pkgs. The
first version goes in from the rendered manifests:

```bash
bash ci/publish-packages.sh vX.Y.Z --dry-run
# prints .../rendered/winget/manifests/z/zhitongblog/Unterm/X.Y.Z
komac submit /path/printed/above        # review, confirm; opens the PR
```

Alternatives: `wingetcreate submit <that dir>` on Windows, or copy the four
YAML files into `manifests/z/zhitongblog/Unterm/X.Y.Z/` of a winget-pkgs fork
and open the PR by hand. On Windows, `winget validate <dir>` and
`winget install --manifest <dir>` (with `winget settings --enable
LocalManifestFiles`) test them before submitting. A Microsoft moderator
reviews new packages; that takes days. Every later release is
`komac update`, which copies the locales and metadata forward from the
previous version and recomputes hashes and ProductCodes itself.

## What the manifests rely on

- **macOS minimum.** The app bundle sets no `LSMinimumSystemVersion`; the
  arm64 slices and the Finder extension are built for macOS 11.0 (x86_64 for
  10.12). Homebrew itself needs a newer macOS than 11, so `brew style`
  requires the bare `depends_on :macos`.
- **Universal binaries.** `unterm`, `unterm-cli` and `unterm-core` in the dmg
  are x86_64 + arm64 (`lipo -info`), so the cask has no `arch` block.
- **`unterm-cli` wrapper.** The cask links a two-line `exec` wrapper rather
  than the binary. `unterm-cli` starts `unterm-core` and `unterm` from the
  directory of `current_exe()`, which on macOS is the symlink's directory
  (`/opt/homebrew/bin`), where neither exists.
- **No `auto_updates`.** The app only checks for updates; Homebrew does the
  upgrading.
- **Zip layout.** The Windows zips put everything under
  `unterm-release-stage/unterm/` (`ci/deploy.sh`), which is the Scoop
  `extract_dir`. The script refuses to publish if that changes.
- **MSI identity.** `installer/Unterm.wxs`: `Scope="perMachine"` (winget
  `Scope: machine`), Manufacturer `Alex`, fixed UpgradeCode
  `{2B17738E-D6E7-44BB-9B55-1D6AF31404A2}` (in `AppsAndFeaturesEntries`), and
  no ProductCode, so WiX generates a new one per build and arch. The script
  reads it back from each MSI (`msiinfo export <msi> Property`, or the
  `pymsi` Python package in a throwaway venv).
- **State directories** (cask `zap`): `~/.unterm` (`state_dir()` in
  `unterm-protocol`), `~/Library/Application Support/Unterm`
  (`core_state_dir()`), `~/.config/unterm` (user Lua config), plus the
  `ai.unzoo.unterm` preferences, caches and saved state.
