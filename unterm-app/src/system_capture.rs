//! Native interactive screenshot flow carried over from the 0.57.4 UI.
//!
//! On Windows this deliberately uses the system snipping overlay.  Besides
//! feeling native, that route leaves the saved PNG, image data, file-drop
//! data and plain path on the clipboard together, which is the behaviour the
//! previous front end exposed.  macOS goes through `screencapture -i` and
//! Linux probes whichever region-capture tool is installed; both also copy
//! the PNG to the system clipboard for parity with the Windows path.

fn output_dir() -> anyhow::Result<std::path::PathBuf> {
    let dir = unterm_protocol::state_dir()
        .unwrap_or_else(|| std::path::PathBuf::from(".unterm"))
        .join("screenshots");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

#[cfg(not(windows))]
fn capture_file_name(hide_window: bool) -> String {
    let prefix = if hide_window {
        "region_hidden"
    } else {
        "region_visible"
    };
    format!(
        "{}_{}.png",
        prefix,
        chrono::Local::now().format("%Y%m%d_%H%M%S_%3f")
    )
}

#[cfg(windows)]
pub fn capture_selected_region(hide_window: bool) -> anyhow::Result<std::path::PathBuf> {
    use base64::Engine as _;

    let pid = std::process::id();
    let prefix = if hide_window {
        "region_hidden"
    } else {
        "region_visible"
    };
    let output_path = output_dir()?.join(format!(
        "{}_{}.png",
        prefix,
        chrono::Local::now().format("%Y%m%d_%H%M%S_%3f")
    ));
    let path = output_path.display().to_string().replace('\'', "''");
    // Each of these runs with however many windows were found, including
    // none: `foreach` over an empty array is a no-op, and the focus calls are
    // guarded. Hiding is the only one worth waiting for.
    let hide_script = if hide_window {
        "foreach ($win in $windows) { [UntermStatusCapture]::ShowWindow($win, 0) | Out-Null }\nif ($windows.Count -gt 0) { Start-Sleep -Milliseconds 350 }"
    } else {
        "if ($hwnd -ne [IntPtr]::Zero) { [UntermStatusCapture]::SetForegroundWindow($hwnd) | Out-Null; Start-Sleep -Milliseconds 120 }"
    };
    let restore_script = if hide_window {
        "foreach ($win in $windows) { [UntermStatusCapture]::ShowWindow($win, 5) | Out-Null }\n  if ($hwnd -ne [IntPtr]::Zero) { [UntermStatusCapture]::SetForegroundWindow($hwnd) | Out-Null }"
    } else {
        "if ($hwnd -ne [IntPtr]::Zero) { [UntermStatusCapture]::SetForegroundWindow($hwnd) | Out-Null }"
    };
    let script = format!(
        r#"
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms
Add-Type @"
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public class UntermStatusCapture {{
  public delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumWindowsProc lpEnumFunc, IntPtr lParam);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint processId);
  public static IntPtr[] WindowsForPid(uint pid) {{
    var windows = new List<IntPtr>();
    EnumWindows((hWnd, lParam) => {{
      uint windowPid;
      GetWindowThreadProcessId(hWnd, out windowPid);
      if (windowPid == pid && IsWindowVisible(hWnd)) windows.Add(hWnd);
      return true;
    }}, IntPtr.Zero);
    return windows.ToArray();
  }}
}}
"@
$proc = Get-Process -Id {pid} -ErrorAction Stop
$windows = @([UntermStatusCapture]::WindowsForPid([uint32]$proc.Id))
# Our own windows are for getting out of the way and for taking focus back
# afterwards. Neither is what the user asked for, so not finding them is not
# a reason to refuse: this used to throw before the picker was ever shown,
# and a capture that never starts reads as a button that does nothing.
# A parked window, a window on another desktop, or one the shell has not
# finished mapping all count as none.
$hwnd = if ($windows.Count -gt 0) {{ $windows[0] }} else {{ [IntPtr]::Zero }}
{hide_script}
try {{
  [System.Windows.Forms.Clipboard]::Clear()
  Start-Process "ms-screenclip:"
  $deadline = [DateTime]::Now.AddSeconds(90)
  $image = $null
  while ([DateTime]::Now -lt $deadline) {{
    Start-Sleep -Milliseconds 250
    if ([System.Windows.Forms.Clipboard]::ContainsImage()) {{
      $image = [System.Windows.Forms.Clipboard]::GetImage()
      break
    }}
  }}
  if ($image -eq $null) {{ throw "Screenshot canceled or timed out" }}
  $image.Save('{path}', [System.Drawing.Imaging.ImageFormat]::Png)
  $clipboardImage = [System.Drawing.Image]::FromFile('{path}')
  $pngBytes = [System.IO.File]::ReadAllBytes('{path}')
  $pngStream = New-Object System.IO.MemoryStream
  $pngStream.Write($pngBytes, 0, $pngBytes.Length)
  $pngStream.Position = 0
  # The image, and only the image. A file list and the path as text used to
  # go on beside it, and a chat window reads the clipboard by asking for the
  # flavours it wants in order -- a dropped file or a line of text long before
  # a bitmap, because that is what most pastes are. So the screenshot arrived
  # as an attachment or as a line of path, never as the picture. CF_BITMAP
  # plus the two PNG flavours is what a picture is here; the terminal's own
  # right-click paste needs nothing on the clipboard, because it answers an
  # image-only clipboard by writing the file itself.
  $data = New-Object System.Windows.Forms.DataObject
  $data.SetImage($clipboardImage)
  $data.SetData('PNG', $false, $pngStream)
  $data.SetData('image/png', $false, $pngStream)
  try {{
    $set = $false
    for ($i = 0; $i -lt 10 -and -not $set; $i++) {{
      try {{
        [System.Windows.Forms.Clipboard]::SetDataObject($data, $true)
        $set = $true
      }} catch {{ Start-Sleep -Milliseconds 120 }}
    }}
    if (-not $set) {{ throw "Clipboard is busy" }}
  }} finally {{
    $clipboardImage.Dispose()
    $pngStream.Dispose()
  }}
  $image.Dispose()
}} finally {{
  {restore_script}
}}
"#
    );

    let mut bytes = Vec::with_capacity(script.len() * 2);
    for unit in script.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    let mut command = std::process::Command::new("powershell.exe");
    command.args([
        "-NoProfile",
        "-STA",
        "-ExecutionPolicy",
        "Bypass",
        "-EncodedCommand",
        &encoded,
    ]);
    use std::os::windows::process::CommandExt;
    command.creation_flags(0x08000000);
    // `output`, not `status`: PowerShell says why it stopped, and throwing
    // that away is how a screenshot that never appears becomes "nothing
    // happened". The script gives up before the picker is ever shown when it
    // cannot find a visible window of ours, and that sentence is the whole
    // diagnosis -- it just had nowhere to go.
    let output = command.output()?;
    if !output.status.success() {
        let reason = String::from_utf8_lossy(&output.stderr);
        // PowerShell wraps a terminating error in several lines of position
        // and category; the first non-empty one carries the message.
        let first = reason
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or("");
        if first.is_empty() {
            anyhow::bail!("PowerShell screenshot returned {}", output.status);
        }
        anyhow::bail!("{first}");
    }
    if !output_path.exists() {
        anyhow::bail!(
            "the screen capture tool closed without producing an image ({})",
            output_path.display()
        );
    }
    Ok(output_path)
}

/// macOS region screenshot via `screencapture -i`.
///
/// `hide_window=true` hides our app first using `osascript` to ask System
/// Events to hide the frontmost process, runs the interactive picker, then
/// reactivates Unterm. ESC cancels the picker.
#[cfg(target_os = "macos")]
pub fn capture_selected_region(hide_window: bool) -> anyhow::Result<std::path::PathBuf> {
    let output_path = output_dir()?.join(capture_file_name(hide_window));

    if hide_window {
        let _ = std::process::Command::new("osascript")
            .args([
                "-e",
                "tell application \"System Events\" to set visible of process \"unterm\" to false",
            ])
            .status();
        // Brief delay so the window finishes hiding before the picker UI shows.
        std::thread::sleep(std::time::Duration::from_millis(250));
    }

    // -i = interactive selection, -t png = explicit format
    // We do NOT pass -x so the picker chrome and shutter sound stay (matches Win UX).
    let status = std::process::Command::new("/usr/sbin/screencapture")
        .args(["-i", "-t", "png"])
        .arg(&output_path)
        .status();

    if hide_window {
        // Always try to bring our window back, even on cancel/error.
        let _ = std::process::Command::new("osascript")
            .args(["-e", "tell application \"unterm\" to activate"])
            .status();
    }

    let status = status?;
    if !status.success() {
        anyhow::bail!("screencapture exited with {status}");
    }

    if !output_path.exists() {
        anyhow::bail!(
            "Screenshot canceled or file not created: {}",
            output_path.display()
        );
    }

    // The image, and only the image.
    //
    // The path used to go on as text too, so that a right-click paste in the
    // terminal -- which reads text -- had something to find. It does not need
    // it: the paste path already answers a clipboard holding an image alone
    // by writing it into the captures folder and pasting that path, which is
    // the same result by a route that does not touch the clipboard. The two
    // were written without knowing about each other.
    //
    // And the text was not free. Plenty of readers ask for the flavours they
    // want in order, text first, because most pastes are text -- that is what
    // a chat window does. Measured rather than assumed: with the text there,
    // asking for `[NSString, NSImage]` hands back the path string; with only
    // the image, the same call hands back the image. So a screenshot pasted
    // into a chat arrived as a line of gibberish path, which is what "the
    // screenshot will not paste into WeChat" was.
    if let Err(err) = clipboard_image(&output_path) {
        log::warn!("could not put the capture on the clipboard: {err:#}");
    }

    Ok(output_path)
}

/// The PNG, via NSPasteboard. Nothing else -- see the caller.
#[cfg(target_os = "macos")]
fn clipboard_image(path: &std::path::Path) -> anyhow::Result<()> {
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    let bytes = std::fs::read(path)?;
    // SAFETY: AppKit classes, main-thread-safe pasteboard calls, and every
    // object handed over is retained by the pasteboard before we return.
    unsafe {
        let pasteboard: *mut AnyObject = msg_send![class!(NSPasteboard), generalPasteboard];
        let _: isize = msg_send![pasteboard, clearContents];
        let data: *mut AnyObject = msg_send![
            class!(NSData),
            dataWithBytes: bytes.as_ptr() as *const std::ffi::c_void,
            length: bytes.len()
        ];
        // `public.tiff` comes free: the pasteboard derives it from the PNG,
        // so a reader that only knows the older flavour still finds a picture.
        let png_type = ns_string("public.png")?;
        let ok_image: bool = msg_send![pasteboard, setData: data, forType: png_type];
        if !ok_image {
            anyhow::bail!("pasteboard refused the capture");
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
unsafe fn ns_string(text: &str) -> anyhow::Result<*mut objc2::runtime::AnyObject> {
    use objc2::{class, msg_send};
    let cstr = std::ffi::CString::new(text)?;
    let object: *mut objc2::runtime::AnyObject =
        msg_send![class!(NSString), stringWithUTF8String: cstr.as_ptr()];
    if object.is_null() {
        anyhow::bail!("NSString refused {text:?}");
    }
    Ok(object)
}

/// Linux region screenshot. Probes available tools in order and uses the first
/// one that exists.
///
/// `hide_window=true` is best-effort — most Linux screenshot tools take a
/// short delay flag, but minimizing the window cleanly across X11/Wayland
/// without window-server-specific code is fragile, so we currently skip it
/// and just rely on the tool's own region picker UI.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn capture_selected_region(hide_window: bool) -> anyhow::Result<std::path::PathBuf> {
    let output_path = output_dir()?.join(capture_file_name(hide_window));

    // Try grim+slurp (Wayland), then gnome-screenshot, spectacle, scrot, maim.
    let path_str = output_path.display().to_string();
    let attempts: &[(&str, &[&str])] = &[
        ("grim", &[]), // grim handled specially below because slurp is piped
        ("gnome-screenshot", &["-a", "-f"]),
        ("spectacle", &["-bn", "-r", "-o"]),
        ("scrot", &["-s"]),
        ("maim", &["-s"]),
    ];

    let mut last_err: Option<String> = None;
    for (tool, args) in attempts {
        if !command_exists(tool) {
            continue;
        }

        let status = if *tool == "grim" {
            // grim -g "$(slurp)" <output>
            if !command_exists("slurp") {
                last_err = Some("grim found but slurp is required for region selection".into());
                continue;
            }
            // Run `slurp` to pick a region, capture stdout, pass to grim.
            let slurp = std::process::Command::new("slurp").output();
            let slurp = match slurp {
                Ok(o) if o.status.success() => o,
                Ok(o) => {
                    last_err = Some(format!(
                        "slurp exited with {} (selection cancelled?)",
                        o.status
                    ));
                    continue;
                }
                Err(e) => {
                    last_err = Some(format!("slurp failed: {e}"));
                    continue;
                }
            };
            let geom = String::from_utf8_lossy(&slurp.stdout).trim().to_string();
            std::process::Command::new("grim")
                .args(["-g", &geom])
                .arg(&output_path)
                .status()
        } else {
            let mut cmd = std::process::Command::new(tool);
            cmd.args(*args);
            cmd.arg(&path_str);
            cmd.status()
        };

        match status {
            Ok(s) if s.success() => {
                if output_path.exists() {
                    // Try to copy to clipboard via xclip / wl-copy — best effort.
                    let _ = copy_image_to_clipboard_unix(&output_path);
                    return Ok(output_path);
                } else {
                    last_err = Some(format!("{tool} reported success but no file was created"));
                }
            }
            Ok(s) => {
                last_err = Some(format!("{tool} exited with {s}"));
            }
            Err(e) => {
                last_err = Some(format!("failed to run {tool}: {e}"));
            }
        }
    }

    let _ = hide_window; // currently unused on Linux
    let msg = last_err.unwrap_or_else(|| {
        "No screenshot tool found. Install one of: grim+slurp, gnome-screenshot, spectacle, scrot, or maim".into()
    });
    anyhow::bail!("{}", msg)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn command_exists(name: &str) -> bool {
    let Some(path_var) = std::env::var_os("PATH") else {
        return false;
    };
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return true;
        }
    }
    false
}

#[cfg(all(unix, not(target_os = "macos")))]
fn copy_image_to_clipboard_unix(path: &std::path::Path) -> anyhow::Result<()> {
    use std::io::Write;
    if command_exists("wl-copy") {
        let mut child = std::process::Command::new("wl-copy")
            .args(["--type", "image/png"])
            .stdin(std::process::Stdio::piped())
            .spawn()?;
        let bytes = std::fs::read(path)?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(&bytes)?;
        }
        child.wait()?;
        return Ok(());
    }
    if command_exists("xclip") {
        let mut child = std::process::Command::new("xclip")
            .args(["-selection", "clipboard", "-t", "image/png", "-i"])
            .stdin(std::process::Stdio::piped())
            .spawn()?;
        let bytes = std::fs::read(path)?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(&bytes)?;
        }
        child.wait()?;
        return Ok(());
    }
    Ok(())
}

#[cfg(all(test, not(windows)))]
mod tests {
    use super::*;

    #[test]
    fn output_dir_is_under_home_unterm_screenshots() {
        let dir = output_dir().expect("output dir should be creatable");
        assert!(dir.ends_with(".unterm/screenshots"), "got {}", dir.display());
        assert!(dir.is_dir());
    }

    #[test]
    fn capture_file_name_prefix_tracks_hide_window() {
        assert!(capture_file_name(true).starts_with("region_hidden_"));
        assert!(capture_file_name(false).starts_with("region_visible_"));
    }

    #[test]
    fn capture_file_name_is_a_timestamped_png() {
        let name = capture_file_name(false);
        assert!(name.ends_with(".png"));
        // region_visible_YYYYMMDD_HHMMSS_mmm.png
        let stamp = name
            .strip_prefix("region_visible_")
            .and_then(|s| s.strip_suffix(".png"))
            .expect("prefix and suffix present");
        assert_eq!(stamp.len(), "YYYYMMDD_HHMMSS_mmm".len());
        assert!(stamp
            .chars()
            .all(|c| c.is_ascii_digit() || c == '_'));
    }
}

/// The clipboard rule, on every platform.
///
/// Its own module because the one above is `cfg(not(windows))`, and the
/// Windows half of what this checks is exactly the half that would go
/// unchecked there.
#[cfg(test)]
mod clipboard_tests {
    /// A capture goes on the clipboard as a picture and nothing else.
    ///
    /// The path used to ride along as text -- on Windows as a dropped file
    /// too -- so that a right-click paste in the terminal had something to
    /// find. It costs more than it gives: a reader asks for the flavours it
    /// wants in order, and text comes before pictures in almost every chat
    /// window, because almost every paste is text. Measured on macOS: with
    /// the text present, asking for `[NSString, NSImage]` returns the path
    /// string; with the picture alone, the same call returns the picture. So
    /// a screenshot pasted into a chat arrived as a line of path.
    ///
    /// Nothing is lost by dropping it. The paste path answers a clipboard
    /// holding a picture alone by writing it into the captures folder and
    /// pasting *that* path -- the same end, reached without spending the
    /// clipboard on it.
    ///
    /// Checked against the source: the call is one line on each platform,
    /// and putting the text back would look like a kindness.
    #[test]
    fn a_capture_puts_a_picture_on_the_clipboard_and_nothing_else() {
        let source = include_str!("system_capture.rs");
        let offenders: Vec<&str> = source
            .lines()
            .map(str::trim)
            // Comments describe the rule and this test names what it forbids;
            // a scan that reads itself reports itself. Lines carrying a
            // string are skipped for that reason, which is also why the file
            // is not simply cut at its first `cfg(test)`: that boundary is a
            // thing to get wrong, and getting it wrong leaves a test that
            // cannot fail.
            .filter(|line| {
                !line.starts_with("//") && !line.starts_with("#") && !line.contains('"')
            })
            .filter(|line| {
                // macOS: a second flavour beside the PNG.
                line.contains("setString:")
                    // Windows: the path as text, or as a file to drop.
                    || line.contains("$data.SetText(")
                    || line.contains("$data.SetFileDropList(")
            })
            .collect();
        assert!(
            offenders.is_empty(),
            "a capture must reach a chat window as a picture, not as its path:\n  {}",
            offenders.join("\n  ")
        );
    }
}
