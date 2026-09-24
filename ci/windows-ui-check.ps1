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
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, System.Text.StringBuilder s, int n);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetClassName(IntPtr h, System.Text.StringBuilder s, int n);
    public static string Text(IntPtr h) { var s = new System.Text.StringBuilder(512); GetWindowText(h, s, 512); return s.ToString(); }
    public static string Class(IntPtr h) { var s = new System.Text.StringBuilder(256); GetClassName(h, s, 256); return s.ToString(); }
    // Every visible top-level window a process owns, described.
    public static string Describe(uint want) {
        var lines = new System.Text.StringBuilder();
        EnumWindows((h, l) => {
            uint pid; GetWindowThreadProcessId(h, out pid);
            RECT r;
            if (pid == want && IsWindowVisible(h) && GetWindowRect(h, out r))
                lines.AppendLine(string.Format("  {0} class={1} title='{2}' rect={3},{4}-{5},{6}", h, Class(h), Text(h), r.L, r.T, r.R, r.B));
            return true;
        }, IntPtr.Zero);
        return lines.ToString();
    }
    // The window whose title names Unterm: winit also makes helper windows,
    // and MainWindowHandle can name one of those.
    public static IntPtr Main(uint want) {
        IntPtr best = IntPtr.Zero;
        EnumWindows((h, l) => {
            uint pid; GetWindowThreadProcessId(h, out pid);
            if (pid == want && IsWindowVisible(h) && Text(h).Contains("Unterm")) { best = h; return false; }
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
        $h = [U]::Main([uint32]$process.Id)
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
    "unterm windows:`n$([U]::Describe([uint32]$app.Id))" | Tee-Object -Append (Join-Path $OutDir "report.txt")
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
        # Unterm maximises by filling the work area itself, so the window's
        # rectangle is the evidence rather than IsZoomed.
        $work = [System.Windows.Forms.Screen]::PrimaryScreen.WorkingArea
        $m = New-Object U+RECT
        [U]::GetWindowRect($hwnd, [ref]$m) | Out-Null
        $filled = ([U]::IsZoomed($hwnd)) -or (($m.R - $m.L) -ge $work.Width - 16 -and ($m.B - $m.T) -ge $work.Height - 16)
        "after clicking maximise: rect $($m.L),$($m.T) - $($m.R),$($m.B), work area $($work.Width)x$($work.Height), filled=$filled" |
            Tee-Object -Append (Join-Path $OutDir "report.txt")
        if (-not $filled) { $failures.Add("clicking the maximise button did not maximise") }
        Shot "03-maximised"
        # The same button, now "restore", brings the old size back.
        $codes2 = HitRow $hwnd ($m.T + 12) ($m.R - 220) ($m.R - 1)
        $restore = @(for ($i = 0; $i -lt $codes2.Count; $i++) { if ($codes2[$i] -eq 9) { $m.R - 220 + $i } })
        if ($restore.Count -lt 20) {
            $failures.Add("the maximised window's restore button does not answer HTMAXBUTTON")
        } else {
            [U]::SetCursorPos([int](($restore[0] + $restore[-1]) / 2), $m.T + 12) | Out-Null
            Start-Sleep -Milliseconds 400
            [U]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)
            Start-Sleep -Milliseconds 120
            [U]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
            Start-Sleep 2
        }
        [U]::GetWindowRect($hwnd, [ref]$r) | Out-Null
        "after clicking restore: rect $($r.L),$($r.T) - $($r.R),$($r.B)" | Tee-Object -Append (Join-Path $OutDir "report.txt")
        if (($r.R - $r.L) -ge $work.Width - 16) { $failures.Add("clicking restore did not restore the window") }
        [U]::SetCursorPos(400, 400) | Out-Null
        Start-Sleep 1
    }
    # The close button, hovered: the one red fill.
    # A real input event after the jump, so the window hears the pointer.
    [U]::SetCursorPos($r.R - 22, $r.T + 12) | Out-Null
    [U]::mouse_event(0x0001, 2, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 700
    Shot "04-hover-close"
    [U]::SetCursorPos($r.R - 46 * 2 - 25, $r.T + 12) | Out-Null
    [U]::mouse_event(0x0001, 2, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 700
    Shot "04b-hover-minimise"
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

# Mica asked for where Windows has none (Server 2022 here): the window must
# still come up, opaque, rather than fail to draw.
Get-Process unterm, unterm-core -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep 2
$conf = Join-Path $env:USERPROFILE ".unterm\unterm.conf"
Add-Content $conf "`n[window]`nbackdrop = `"mica`"`n"
$mica = Start-Process $exe -PassThru
$micaHwnd = WaitWindow $mica
if ($micaHwnd -eq [IntPtr]::Zero) {
    $failures.Add("with window.backdrop = mica the window did not appear")
} else {
    Start-Sleep 6
    [U]::SetWindowPos($micaHwnd, [IntPtr]::Zero, 40, 30, 900, 600, 0x0004) | Out-Null
    [U]::SetForegroundWindow($micaHwnd) | Out-Null
    Start-Sleep 2
    Shot "06-mica-requested"
    $mica.Refresh()
    "with backdrop = mica: exited=$($mica.HasExited)" | Tee-Object -Append (Join-Path $OutDir "report.txt")
    if ($mica.HasExited) { $failures.Add("with window.backdrop = mica unterm exited") }
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
