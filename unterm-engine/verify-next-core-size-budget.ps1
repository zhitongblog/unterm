param(
    # The 0.60 feature-parity tranche added bounded per-pane scrollback,
    # semantic-prompt navigation, ordered tabs and interactive split resizing,
    # bringing the pre-parity 11,998-line core to 12,203. The follow-up
    # runtime/tab tranche (visible tab order everywhere, prompt-row trimming,
    # runtime pump metrics) measured 12,445, blowing the 12,300 gate that the
    # earlier tranche had set. Recalibrate to the measured size plus about a
    # hundred lines of headroom, as before, instead of turning this into an
    # unbounded "new features" allowance.
    # 12550 -> 12800 (2026-08-02): font collections gained per-face
    # enumeration and indexed opens, rasterization gained the macOS
    # smoothing curve, and search gained its mode plumbing -- capability,
    # not sprawl. The regex crate that tried to ride along was evicted to
    # the front end the same day; dependencies stay at ten.
    # 12800 -> 12950 (2026-08-10): launch gained locale synthesis --
    # a Finder-launched GUI inherits no LANG, the shell fell into the C
    # locale, and zle shredded every CJK character with a 0x80-0x9F
    # continuation byte. Measured 12847 after trimming the fix to its
    # essentials; recalibrated to measured plus the usual headroom.
    # 12950 -> 13320, probe 2500 -> 2600 (2026-08-12): the settings/layout
    # tranche landed argv launches, split-layout persistence across GUI
    # restarts, startup-session draw, and Core-first discovery hardening.
    # Measured 13219 core / 2529 probe; recalibrated to measured plus the
    # usual headroom.
    # 13560 -> 13720 (2026-08-28): wide-character pairs are now repaired
    # after every operation that can separate them, not just the ones that
    # overwrite -- ICH/DCH, insert mode, and SL/SR all shift cells, and a
    # shift leaves the orphan in the middle of the row rather than at its
    # edge, so the point checks became a sweep. SL/SR also stopped being
    # "DCH/ICH applied to the cursor's row" and now move every row of the
    # scroll region, which is what ECMA-48 says they do and what stops a
    # boxed TUI tearing. The cell got smaller at the same time (80 -> 48
    # bytes), which is why this is not larger. Measured 13614; recalibrated
    # to measured plus the usual headroom.
    # 13320 -> 13560, probe 2600 -> 2800 (2026-08-26): the core gained
    # wide-cell splitting -- overwriting either half of a CJK cell now
    # releases the other, across writes, `ESC[K` and `ESC[nX`, with the
    # regression tests that pin it -- and a cwd that is re-read rather than
    # recorded once. The probe gained a shell dialect, because its workloads
    # were cmd.exe-only and every throughput benchmark had been passing
    # without running away from Windows. Measured 13445 core / 2700 probe;
    # recalibrated to measured plus the usual headroom.
    # 13720 -> 13960 (2026-09-23): resize stopped being able to wreck a
    # pane. A window's shrink now waits until it has held (a new settle
    # module), because a shrink taken back before the program read its size
    # left a full-screen program drawing diffs onto a screen that was no
    # longer there; a real size change resets the scroll region, as in xterm;
    # and leaving the alternate screen fits the saved main screen to the
    # current size. Measured 13858 after trimming the comments to their
    # essentials; recalibrated to measured plus the usual headroom.
    [int]$MaxCoreSourceLines = 13960,
    [int]$MaxProbeSourceLines = 2800,
    [int]$MaxDirectDependencies = 10,
    # A debug binary carries its debug info, so this tracks the toolchain and
    # the C libraries far more than it tracks next-core. The real size control
    # is MaxCoreSourceLines; this one only catches a sudden jump.
    [int]$MaxDebugBinaryBytes = 8000000,
    [switch]$SkipBinarySizeCheck
)

$ErrorActionPreference = "Stop"

$EngineDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$RepoRoot = Resolve-Path (Join-Path $EngineDir "..")
$SourceRoot = Join-Path $EngineDir "src"
$CoreRoot = Join-Path $SourceRoot "next_core"
$MainCoreFile = Join-Path $SourceRoot "next_core.rs"
$ProbeFile = Join-Path $SourceRoot "bin" "unterm-next-core.rs"
$IsWindowsPlatform = $env:OS -eq "Windows_NT"
$DebugBinaryName = if ($IsWindowsPlatform) { "unterm-next-core.exe" } else { "unterm-next-core" }
$TargetRoot = if ([string]::IsNullOrWhiteSpace($env:CARGO_TARGET_DIR)) {
    Join-Path $RepoRoot "target"
} elseif ([System.IO.Path]::IsPathRooted($env:CARGO_TARGET_DIR)) {
    $env:CARGO_TARGET_DIR
} else {
    Join-Path $RepoRoot $env:CARGO_TARGET_DIR
}
$DebugBinary = Join-Path $TargetRoot "debug" $DebugBinaryName

function Count-Lines {
    param([string[]]$Paths)

    $total = 0
    foreach ($path in $Paths) {
        if (-not (Test-Path $path)) {
            throw "missing source file: $path"
        }
        $total += @((Get-Content -Path $path)).Count
    }
    return $total
}

function Count-ProductionRustLines {
    param([string[]]$Paths)

    $total = 0
    foreach ($path in $Paths) {
        if (-not (Test-Path $path)) {
            throw "missing source file: $path"
        }

        $fileName = [System.IO.Path]::GetFileName($path)
        if ($fileName -eq "test_support.rs" -or $fileName -eq "test_facade.rs") {
            continue
        }

        $skipNextTestModule = $false
        $inTestModule = $false
        $braceDepth = 0
        foreach ($line in Get-Content -Path $path) {
            if (-not $inTestModule -and $line -match '^\s*#\[cfg\(test\)\]\s*$') {
                $skipNextTestModule = $true
                continue
            }

            if ($skipNextTestModule) {
                # Any test module, not only one named `tests`. A rule that
                # counted `mod palette_tests` as production would report the
                # kernel growing when what grew was its test suite -- and
                # would quietly discourage writing the tests.
                if ($line -match '^\s*mod\s+\w+\s*\{') {
                    $inTestModule = $true
                    $braceDepth = ([regex]::Matches($line, '\{').Count - [regex]::Matches($line, '\}').Count)
                    if ($braceDepth -le 0) {
                        $inTestModule = $false
                    }
                    $skipNextTestModule = $false
                    continue
                }

                $skipNextTestModule = $false
            }

            if ($inTestModule) {
                $braceDepth += ([regex]::Matches($line, '\{').Count - [regex]::Matches($line, '\}').Count)
                if ($braceDepth -le 0) {
                    $inTestModule = $false
                }
                continue
            }

            $total += 1
        }
    }
    return $total
}

$coreFiles = @($MainCoreFile)
if (Test-Path $CoreRoot) {
    $coreFiles += @(Get-ChildItem -Path $CoreRoot -Recurse -File -Filter "*.rs" | ForEach-Object { $_.FullName })
}
$coreSourceLines = Count-ProductionRustLines $coreFiles
$probeSourceLines = Count-Lines @($ProbeFile)

# Invoke cargo directly rather than through `cmd /c`: the shim made this
# whole gate Windows-only, so a change that broke the budget could not be
# caught anywhere but CI.
$treeLines = @(& cargo tree -p unterm-engine --depth 1 --prefix depth 2>&1 | ForEach-Object { $_.ToString() })
if ($LASTEXITCODE -ne 0) {
    throw "cargo tree failed:`n$($treeLines -join "`n")"
}
# The product's own crates are decomposition, not dependency: splitting
# shared types into unterm-protocol must not read as the kernel growing
# an appetite. Everything else -- including the vendored freetype and
# harfbuzz trees -- stays counted, because vendoring a library does not
# make it stop being one.
$directDependencies = @($treeLines | Where-Object { $_ -match "^1\S" -and $_ -notmatch "^1unterm-" }).Count

$debugBinaryBytes = $null
if (-not $SkipBinarySizeCheck) {
    # Always rebuild. Measuring whatever binary happened to be lying around let
    # this check pass for a long time against an artifact that no longer
    # matched the source.
    & cargo build -p unterm-engine --bin unterm-next-core
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build failed for unterm-engine --bin unterm-next-core"
    }
    if (-not (Test-Path $DebugBinary)) {
        throw "missing debug binary after build: $DebugBinary"
    }
    $debugBinaryBytes = (Get-Item $DebugBinary).Length
}

$failures = New-Object System.Collections.Generic.List[string]
if ($coreSourceLines -gt $MaxCoreSourceLines) {
    $failures.Add("core_source_lines=$coreSourceLines exceeds max=$MaxCoreSourceLines")
}
if ($probeSourceLines -gt $MaxProbeSourceLines) {
    $failures.Add("probe_source_lines=$probeSourceLines exceeds max=$MaxProbeSourceLines")
}
if ($directDependencies -gt $MaxDirectDependencies) {
    $failures.Add("direct_dependencies=$directDependencies exceeds max=$MaxDirectDependencies")
}
if ((-not $SkipBinarySizeCheck) -and $debugBinaryBytes -gt $MaxDebugBinaryBytes) {
    $failures.Add("debug_binary_bytes=$debugBinaryBytes exceeds max=$MaxDebugBinaryBytes")
}

if ($failures.Count -gt 0) {
    throw "next-core size budget failed: $($failures -join '; ')"
}

$binarySummary = if ($SkipBinarySizeCheck) { "skipped" } else { $debugBinaryBytes }
Write-Host "next-core size budget ok: core_source_lines=$coreSourceLines/$MaxCoreSourceLines probe_source_lines=$probeSourceLines/$MaxProbeSourceLines direct_dependencies=$directDependencies/$MaxDirectDependencies debug_binary_bytes=$binarySummary/$MaxDebugBinaryBytes"
