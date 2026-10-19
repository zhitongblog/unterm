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
fn is_snipping_overlay_active() -> bool {
    use winapi::shared::minwindef::{BOOL, DWORD, LPARAM, TRUE};
    use winapi::shared::windef::HWND;
    use winapi::um::handleapi::{CloseHandle, INVALID_HANDLE_VALUE};
    use winapi::um::tlhelp32::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use winapi::um::winuser::{
        EnumWindows, GetForegroundWindow, GetWindowLongW, GetWindowThreadProcessId,
        IsWindowVisible, GWL_EXSTYLE, WS_EX_TOPMOST,
    };

    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return false;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;

        let mut host_running = false;
        let mut snipping_pids: Vec<DWORD> = Vec::new();
        if Process32FirstW(snapshot, &mut entry) != 0 {
            loop {
                let len = entry
                    .szExeFile
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(entry.szExeFile.len());
                let name = String::from_utf16_lossy(&entry.szExeFile[..len]);
                if name.eq_ignore_ascii_case("ScreenClippingHost.exe") {
                    host_running = true;
                    break;
                }
                if name.eq_ignore_ascii_case("SnippingTool.exe") {
                    snipping_pids.push(entry.th32ProcessID);
                }
                if Process32NextW(snapshot, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snapshot);

        if host_running {
            return true;
        }
        if snipping_pids.is_empty() {
            return false;
        }

        // On Windows 11, SnippingTool.exe stays resident in the background with no
        // visible windows when idle, and displays a foreground/topmost overlay window
        // while `ms-screenclip:` is active.
        let fg_hwnd = GetForegroundWindow();
        if !fg_hwnd.is_null() {
            let mut fg_pid: DWORD = 0;
            GetWindowThreadProcessId(fg_hwnd, &mut fg_pid);
            if snipping_pids.contains(&fg_pid) && IsWindowVisible(fg_hwnd) != 0 {
                return true;
            }
        }

        struct SearchCtx<'a> {
            pids: &'a [DWORD],
            active: bool,
        }
        unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
            let ctx = &mut *(lparam as *mut SearchCtx<'_>);
            if IsWindowVisible(hwnd) != 0 {
                let mut pid: DWORD = 0;
                GetWindowThreadProcessId(hwnd, &mut pid);
                if ctx.pids.contains(&pid) {
                    let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
                    if (ex_style & WS_EX_TOPMOST) != 0 {
                        ctx.active = true;
                        return 0;
                    }
                }
            }
            TRUE
        }

        let mut ctx = SearchCtx {
            pids: &snipping_pids,
            active: false,
        };
        EnumWindows(Some(enum_proc), &mut ctx as *mut SearchCtx<'_> as LPARAM);
        ctx.active
    }
}

#[cfg(windows)]
pub fn capture_selected_region(hide_window: bool) -> anyhow::Result<std::path::PathBuf> {
    use std::os::windows::process::CommandExt;

    let output_path = output_dir()?.join(capture_file_name(hide_window));

    // Brief pause if we cloaked the window so DWM composites the frame behind us
    if hide_window {
        std::thread::sleep(std::time::Duration::from_millis(60));
    }

    // Clear any stale Esc key state before launching the snipping overlay
    unsafe {
        let _ = winapi::um::winuser::GetAsyncKeyState(winapi::um::winuser::VK_ESCAPE);
    }

    // Get initial clipboard sequence number before triggering screenclip
    let mut last_seq = unsafe { winapi::um::winuser::GetClipboardSequenceNumber() };

    // Launch native Windows snipping overlay (Win+Shift+S experience)
    let mut cmd = std::process::Command::new("cmd");
    cmd.args(["/c", "start", "", "ms-screenclip:"]);
    cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    let status = cmd.status()?;
    if !status.success() {
        anyhow::bail!("failed to launch ms-screenclip: {status}");
    }

    let started = std::time::Instant::now();
    let deadline = started + std::time::Duration::from_secs(30);
    let mut captured_image = None;
    let mut overlay_seen = false;

    while std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(60));

        let current_seq = unsafe { winapi::um::winuser::GetClipboardSequenceNumber() };
        if current_seq != last_seq {
            last_seq = current_seq;
            if let Ok(mut board) = arboard::Clipboard::new() {
                if let Ok(image) = board.get_image() {
                    captured_image = Some(image);
                    break;
                }
            }
        }

        let esc_pressed = (unsafe {
            winapi::um::winuser::GetAsyncKeyState(winapi::um::winuser::VK_ESCAPE)
        } as u16
            & 0x8000)
            != 0;
        if esc_pressed && (overlay_seen || started.elapsed() > std::time::Duration::from_millis(250))
        {
            break;
        }

        let active = is_snipping_overlay_active();
        if active {
            overlay_seen = true;
        } else if overlay_seen {
            // Overlay closed (either selection completed, Esc pressed, or toolbar X clicked).
            // Poll briefly (up to 350ms) in case SnippingTool is still writing the bitmap to the clipboard.
            for _ in 0..7 {
                std::thread::sleep(std::time::Duration::from_millis(50));
                let final_seq = unsafe { winapi::um::winuser::GetClipboardSequenceNumber() };
                if final_seq != last_seq {
                    last_seq = final_seq;
                    if let Ok(mut board) = arboard::Clipboard::new() {
                        if let Ok(image) = board.get_image() {
                            captured_image = Some(image);
                            break;
                        }
                    }
                }
            }
            break;
        } else if started.elapsed() > std::time::Duration::from_millis(3000) {
            // Overlay never appeared within 3 seconds; do not leave the window cloaked.
            break;
        }
    }

    let Some(image) = captured_image else {
        anyhow::bail!("Screenshot canceled or timed out");
    };

    let rgba = image::RgbaImage::from_raw(
        image.width as u32,
        image.height as u32,
        image.bytes.into_owned(),
    )
    .ok_or_else(|| anyhow::anyhow!("image dimensions mismatch"))?;

    rgba.save_with_format(&output_path, image::ImageFormat::Png)?;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_dir_is_under_home_unterm_screenshots() {
        let dir = output_dir().expect("output dir should be creatable");
        let suffix = std::path::Path::new(".unterm").join("screenshots");
        assert!(dir.ends_with(suffix), "got {}", dir.display());
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
