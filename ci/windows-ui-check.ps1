# Drive a built Unterm on a real Windows desktop and record what it does:
# the window, its caption buttons (hit-testing, hover, maximise), and the
# administrator window. Screenshots and logs land in $OutDir for a person --
# or an agent -- to look at; the checks that can be decided here fail the run.
param(
    [string]$Bin = "target\release",
    [string]$OutDir = "ui-check"
)
$ErrorActionPreference = "Stop"
New-Item -ItemType Directory -Force $OutDir | Out-Null
Add-Type -AssemblyName System.Drawing, System.Windows.Forms
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class U {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern IntPtr SendMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern void mouse_event(uint f, int x, int y, uint d, UIntPtr e);
    [DllImport("user32.dll")] public static extern bool IsZoomed(IntPtr h);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int c);
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr a, int x, int y, int w, int hh, uint f);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    public delegate bool EnumProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc f, IntPtr l);
    // The largest visible top-level window a process owns: winit also makes
    // small hidden helper windows, which is what MainWindowHandle can name.
    public static IntPtr Largest(uint want) {
        IntPtr best = IntPtr.Zero; long area = 0;
        EnumWindows((h, l) => {
            uint pid; GetWindowThreadProcessId(h, out pid);
            RECT r;
            if (pid == want && IsWindowVisible(h) && GetWindowRect(h, out r)) {
                long a = (long)(r.R - r.L) * (r.B - r.T);
                if (a > area) { area = a; best = h; }
            }
            return true;
        }, IntPtr.Zero);
        return best;
    }
}
"@
[U]::SetProcessDPIAware() | Out-Null
$failures = New-Object System.Collections.Generic.List[string]

function Shot([string]$name) {
    $b = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
    $bmp = New-Object System.Drawing.Bitmap $b.Width, $b.Height
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($b.Location, [System.Drawing.Point]::Empty, $b.Size)
    $bmp.Save((Join-Path $OutDir "$name.png"), [System.Drawing.Imaging.ImageFormat]::Png)
    $g.Dispose(); $bmp.Dispose()
}

function WaitWindow($process, [int]$seconds = 60) {
    $deadline = (Get-Date).AddSeconds($seconds)
    while ((Get-Date) -lt $deadline) {
        $process.Refresh()
        if ($process.HasExited) { return [IntPtr]::Zero }
        $h = [U]::Largest([uint32]$process.Id)
        if ($h -ne [IntPtr]::Zero) { return $h }
        Start-Sleep -Milliseconds 500
    }
    return [IntPtr]::Zero
}

function HitRow([IntPtr]$hwnd, [int]$y, [int]$left, [int]$right) {
    $codes = @()
    for ($x = $left; $x -lt $right; $x++) {
        $l = [IntPtr](($y -shl 16) -bor ($x -band 0xFFFF))
        $codes += [int][U]::SendMessage($hwnd, 0x84, [IntPtr]::Zero, $l)
    }
    return , $codes
}

$exe = Resolve-Path (Join-Path $Bin "unterm.exe")
$app = Start-Process $exe -PassThru
$hwnd = WaitWindow $app
if ($hwnd -eq [IntPtr]::Zero) {
    $failures.Add("no window appeared")
} else {
    Start-Sleep 6
    [U]::ShowWindow($hwnd, 9) | Out-Null
    # On screen and a known size, so every button is somewhere a pointer can go.
    [U]::SetWindowPos($hwnd, [IntPtr]::Zero, 40, 30, 900, 600, 0x0004) | Out-Null
    [U]::SetForegroundWindow($hwnd) | Out-Null
    Start-Sleep 2
    Shot "01-window"
    $r = New-Object U+RECT
    [U]::GetWindowRect($hwnd, [ref]$r) | Out-Null
    "window rect: $($r.L),$($r.T) - $($r.R),$($r.B)" | Tee-Object -Append (Join-Path $OutDir "report.txt")

    # Snap layouts come from the system asking where the maximise button is.
    $y = $r.T + 12
    $codes = HitRow $hwnd $y ($r.R - 220) ($r.R - 1)
    $max = @(for ($i = 0; $i -lt $codes.Count; $i++) { if ($codes[$i] -eq 9) { $r.R - 220 + $i } })
    "hit codes along the caption (right 220px): $(($codes | Group-Object | ForEach-Object { "$($_.Name)x$($_.Count)" }) -join ' ')" |
        Tee-Object -Append (Join-Path $OutDir "report.txt")
    if ($max.Count -lt 20) {
        $failures.Add("maximise button does not answer WM_NCHITTEST with HTMAXBUTTON ($($max.Count) px)")
    } else {
        $cx = [int](($max[0] + $max[-1]) / 2)
        "maximise button spans x=$($max[0])..$($max[-1])" | Tee-Object -Append (Join-Path $OutDir "report.txt")
        [U]::SetCursorPos($cx, $y) | Out-Null
        Start-Sleep -Milliseconds 700
        Shot "02-hover-maximise"
        [U]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)
        Start-Sleep -Milliseconds 120
        [U]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
        Start-Sleep 2
        $zoomed = [U]::IsZoomed($hwnd)
        "maximised after click: $zoomed" | Tee-Object -Append (Join-Path $OutDir "report.txt")
        if (-not $zoomed) { $failures.Add("clicking the maximise button did not maximise") }
        Shot "03-maximised"
        [U]::SetCursorPos(400, 400) | Out-Null
        [U]::ShowWindow($hwnd, 9) | Out-Null
        Start-Sleep 1
    }
    # The close button, hovered: the one red fill.
    [U]::SetCursorPos($r.R - 20, $y) | Out-Null
    Start-Sleep -Milliseconds 700
    Shot "04-hover-close"
    [U]::SetCursorPos(400, 400) | Out-Null
}
$app.Refresh()
if ($app.HasExited) { $failures.Add("unterm exited (code $($app.ExitCode))") }

# The administrator window: a second process, its own state, no agent surface.
$admin = Start-Process $exe -ArgumentList "--admin" -PassThru
$adminHwnd = WaitWindow $admin
if ($adminHwnd -eq [IntPtr]::Zero) {
    $failures.Add("the administrator window did not appear")
} else {
    Start-Sleep 5
    [U]::SetWindowPos($adminHwnd, [IntPtr]::Zero, 90, 70, 900, 600, 0x0004) | Out-Null
    [U]::SetForegroundWindow($adminHwnd) | Out-Null
    Start-Sleep 1
    Shot "05-admin-window"
    $admin.Refresh()
    Start-Sleep 1
    "admin window title: $($admin.MainWindowTitle)" | Tee-Object -Append (Join-Path $OutDir "report.txt")
    if ($admin.MainWindowTitle -notmatch "Administrator") {
        $failures.Add("administrator window title does not say so: '$($admin.MainWindowTitle)'")
    }
    if (-not (Test-Path (Join-Path $env:USERPROFILE ".unterm\admin"))) {
        $failures.Add("administrator window did not keep its own state directory")
    }
}

foreach ($log in "panic.log", "stall.log") {
    $path = Join-Path $env:USERPROFILE ".unterm\$log"
    if (Test-Path $path) { Copy-Item $path (Join-Path $OutDir $log) }
}
Get-Process unterm, unterm-core -ErrorAction SilentlyContinue | Stop-Process -Force

if ($failures.Count) {
    $failures | ForEach-Object { Write-Host "FAIL: $_" }
    $failures | Out-File -Append (Join-Path $OutDir "report.txt")
    exit 1
}
Write-Host "windows ui check ok"
