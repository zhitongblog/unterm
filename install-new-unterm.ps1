# Installs the freshly-built unterm binaries over the Program Files copy.
# Self-elevates (you'll get ONE UAC prompt — click Yes).
$ErrorActionPreference = 'Stop'

$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltinRole]::Administrator)
if (-not $isAdmin) {
  Write-Host "Requesting administrator rights (accept the UAC prompt)..."
  Start-Process -Verb RunAs -FilePath powershell -ArgumentList "-NoProfile -ExecutionPolicy Bypass -File `"$PSCommandPath`""
  return
}

$binaries = @('unterm.exe', 'unterm-cli.exe', 'unterm-core.exe')
foreach ($bin in $binaries) {
  $src = "D:\code\unterm\target\release\$bin"
  $dst = "C:\Program Files\Unterm\$bin"
  $bak = "C:\Program Files\Unterm\$bin.bak"

  if (-not (Test-Path $src)) {
    Write-Host "ERROR: build not found at $src"
    continue
  }

  if (Test-Path $bak) { Remove-Item $bak -Force -ErrorAction SilentlyContinue }
  if (Test-Path $dst) { Rename-Item $dst $bak -Force -ErrorAction SilentlyContinue }
  Copy-Item $src $dst -Force

  $new = Get-Item $dst
  Write-Host ("Installed -> {0}  ({1} bytes, {2})" -f $dst, $new.Length, $new.LastWriteTime)
}

Write-Host "Done. Close any open Unterm window and reopen it."
Start-Sleep 3
