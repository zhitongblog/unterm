# Unterm one-shot installer for Windows.
#
# Usage:
#   irm https://unterm.app/install.ps1 | iex
#
# What it does:
#   - Resolves the latest release tag via the GitHub API.
#   - Downloads Unterm-<version>-x64.msi to %TEMP%.
#   - Runs `msiexec /i` with a small UI so the user can see progress
#     and click Finish — unlike the silent default we shipped in 0.5.1.
#   - Verifies install by looking for unterm.exe under Program Files.
#
# Re-running upgrades in place. Set $env:UNTERM_VERSION = "v0.5.2"
# before piping to pin a specific tag.

$ErrorActionPreference = 'Stop'

$repo    = 'zhitongblog/unterm'
$version = $env:UNTERM_VERSION

function Say  ($m) { Write-Host "» $m" -ForegroundColor Blue }
function Ok   ($m) { Write-Host "✓ $m" -ForegroundColor Green }
function Warn ($m) { Write-Host "! $m" -ForegroundColor Yellow }
function Die  ($m) { Write-Host "✗ $m" -ForegroundColor Red; exit 1 }

# --- Resolve target tag ----------------------------------------------------
if (-not $version) {
  Say 'looking up latest Unterm release...'
  $api = "https://api.github.com/repos/$repo/releases/latest"
  try {
    $version = (Invoke-RestMethod -Uri $api -Headers @{ 'User-Agent'='unterm-installer' }).tag_name
  } catch {
    # The API allows 60 anonymous requests an hour per address, and an office
    # or a VPN shares one. The release page's redirect names the tag too.
    Warn "GitHub API unavailable ($($_.Exception.Message)); reading the release page instead"
    try {
      $page = Invoke-WebRequest -Uri "https://github.com/$repo/releases/latest" -MaximumRedirection 0 -UseBasicParsing -ErrorAction SilentlyContinue
      $location = $page.Headers.Location
    } catch {
      # PowerShell 7 throws on the 302 and carries an HttpResponseMessage;
      # Windows PowerShell 5.1 carries an HttpWebResponse.
      $response = $_.Exception.Response
      if ($response -and $response.Headers.Location) { $location = $response.Headers.Location }
      elseif ($response) { $location = $response.Headers['Location'] }
    }
    if ($location) { $version = ($location.ToString() -split '/tag/')[-1] }
    if (-not $version) { Die "couldn't resolve the latest release from GitHub ($_)" }
  }
}
if (-not $version) { Die 'tag_name was empty in API response' }
Ok "Unterm $version"

# --- Architecture check ----------------------------------------------------
# Native ARM64 and x64 builds both ship. PROCESSOR_ARCHITECTURE is the
# *process* arch; under x64 emulation on an ARM device PowerShell reports
# AMD64 but sets PROCESSOR_ARCHITEW6432=ARM64 — prefer that so an ARM machine
# gets the native arm64 MSI even when the shell is emulated.
$nativeArch = $env:PROCESSOR_ARCHITEW6432
if (-not $nativeArch) { $nativeArch = $env:PROCESSOR_ARCHITECTURE }
if ($nativeArch -eq 'ARM64') { $msiArch = 'arm64' } else { $msiArch = 'x64' }

# --- Download MSI ----------------------------------------------------------
# Strip leading 'v' for the Debian-style filename (Unterm-0.5.2-x64.msi),
# but keep it on the GitHub-style download URL component.
$verNoV = $version -replace '^v',''
$asset  = "Unterm-$verNoV-$msiArch.msi"
$url    = "https://github.com/$repo/releases/download/$version/$asset"
$dest   = Join-Path $env:TEMP $asset

Say "downloading $asset"
try {
  # `Invoke-WebRequest -OutFile` is fastest on PS 5.1; PS 7 has parallel
  # but we want broad compat.
  $oldProgress = $ProgressPreference
  $ProgressPreference = 'Continue'
  Invoke-WebRequest -Uri $url -OutFile $dest -UseBasicParsing
  $ProgressPreference = $oldProgress
} catch {
  Die "download failed: $_"
}
$size = (Get-Item $dest).Length
Ok ("downloaded {0:N1} MB → {1}" -f ($size/1MB), $dest)

# --- Install ---------------------------------------------------------------
# /qb! — basic UI with no Cancel button (so a user can't abort halfway and
# leave a half-installed state). /norestart — never auto-reboot.
# /l*v ...log — verbose log so when something fails we can read it.
# msiexec returns 0 on success, 1602 if user cancelled, 1603 on generic
# failure. Translate to friendly text.
$log = Join-Path $env:TEMP 'unterm-install.log'
Say 'launching installer (UAC may prompt for elevation)...'
$proc = Start-Process -FilePath 'msiexec.exe' `
  -ArgumentList @('/i', "`"$dest`"", '/qb!', '/norestart', '/l*v', "`"$log`"") `
  -Wait -PassThru
switch ($proc.ExitCode) {
  0     { Ok 'installer reported success.' }
  1602  { Die 'installer was cancelled by the user (UAC denied or you closed it).' }
  1603  { Die "installer failed with code 1603 (fatal error during installation). Log: $log" }
  3010  { Warn 'install succeeded but a reboot is recommended.' }
  default { Die ("installer exited with code {0}. Log: {1}" -f $proc.ExitCode, $log) }
}

# --- Verify ----------------------------------------------------------------
$expected = "$env:ProgramFiles\Unterm\unterm.exe"
if (Test-Path $expected) {
  Ok "installed to $expected"
  Say 'launch with: Start Menu → Unterm'
} else {
  Warn "couldn't find $expected — install may have gone elsewhere. Check log: $log"
}
