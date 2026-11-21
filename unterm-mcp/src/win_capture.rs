//! Screen and window capture on Windows, through GDI directly.
//!
//! These used to be PowerShell scripts passed as `-EncodedCommand` with the
//! execution policy bypassed, one of them compiling C# with `Add-Type` to
//! reach `PrintWindow`. That is exactly the shape antivirus engines look for
//! in malware (a hidden, encoded PowerShell that P/Invokes user32), so an
//! ordinary screenshot made the whole program look suspicious. The calls are
//! the same ones the scripts made; they are just made from here.

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::path::Path;
use std::ptr::null_mut;
use winapi::shared::minwindef::{BOOL, LPARAM, TRUE};
use winapi::shared::windef::{HDC, HWND, RECT};
use winapi::um::wingdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDIBits,
    SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CAPTUREBLT, DIB_RGB_COLORS, SRCCOPY,
};
use winapi::um::winuser::{
    EnumWindows, GetDC, GetForegroundWindow, GetSystemMetrics, GetWindow, GetWindowRect,
    GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
    PrintWindow, ReleaseDC, SetForegroundWindow, ShowWindowAsync, GW_OWNER, SM_CXVIRTUALSCREEN,
    SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SW_RESTORE, SW_SHOW,
};

/// Asks DWM for a composited window's full surface, even when covered.
const PW_RENDERFULLCONTENT: u32 = 2;

/// Pixels in physical units, whatever this process's DPI awareness: without
/// it, on a scaled display the rectangles come back shrunk and the capture
/// is cut off.
fn physical_pixels() {
    use winapi::um::winuser::SetThreadDpiAwarenessContext;
    use winapi::shared::windef::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2;
    unsafe {
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

/// A memory bitmap of `width` x `height`, filled by `draw`, read out as RGBA.
fn grab(width: i32, height: i32, draw: impl FnOnce(HDC) -> bool) -> Result<(bool, Vec<u8>)> {
    if width <= 0 || height <= 0 {
        bail!("Invalid capture bounds");
    }
    unsafe {
        let screen = GetDC(null_mut());
        if screen.is_null() {
            bail!("GetDC failed");
        }
        let memory = CreateCompatibleDC(screen);
        let bitmap = CreateCompatibleBitmap(screen, width, height);
        if memory.is_null() || bitmap.is_null() {
            if !bitmap.is_null() {
                DeleteObject(bitmap as _);
            }
            if !memory.is_null() {
                DeleteDC(memory);
            }
            ReleaseDC(null_mut(), screen);
            bail!("could not create a {width}x{height} bitmap");
        }
        let previous = SelectObject(memory, bitmap as _);
        let drawn = draw(memory);

        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height, // top-down rows
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..std::mem::zeroed()
        };
        let mut pixels = vec![0u8; width as usize * height as usize * 4];
        SelectObject(memory, previous);
        let rows = GetDIBits(
            memory,
            bitmap,
            0,
            height as u32,
            pixels.as_mut_ptr() as _,
            &mut info,
            DIB_RGB_COLORS,
        );
        DeleteObject(bitmap as _);
        DeleteDC(memory);
        ReleaseDC(null_mut(), screen);
        if rows == 0 {
            bail!("GetDIBits failed");
        }
        // BGRA -> RGBA, fully opaque (GDI leaves alpha undefined).
        for pixel in pixels.chunks_exact_mut(4) {
            pixel.swap(0, 2);
            pixel[3] = 255;
        }
        Ok((drawn, pixels))
    }
}

/// Copies the screen rectangle at (`left`, `top`) into the bitmap.
fn from_screen(left: i32, top: i32, width: i32, height: i32) -> impl FnOnce(HDC) -> bool {
    move |memory| unsafe {
        let screen = GetDC(null_mut());
        let ok = BitBlt(memory, 0, 0, width, height, screen, left, top, SRCCOPY | CAPTUREBLT) != 0;
        ReleaseDC(null_mut(), screen);
        ok
    }
}

fn save_png(path: &Path, width: i32, height: i32, rgba: Vec<u8>) -> Result<()> {
    image::RgbaImage::from_raw(width as u32, height as u32, rgba)
        .ok_or_else(|| anyhow!("capture buffer has the wrong size"))?
        .save_with_format(path, image::ImageFormat::Png)
        .with_context(|| format!("write {}", path.display()))
}

/// The whole virtual screen (every monitor).
pub fn capture_screen(path: &Path) -> Result<Value> {
    physical_pixels();
    let (left, top, width, height) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    };
    let (ok, rgba) = grab(width, height, from_screen(left, top, width, height))?;
    if !ok {
        bail!("BitBlt from the screen failed");
    }
    save_png(path, width, height, rgba)?;
    Ok(json!({
        "path": path.display().to_string(),
        "width": width,
        "height": height,
        "left": left,
        "top": top,
    }))
}

struct Found {
    hwnd: HWND,
    pid: u32,
    title: String,
}

fn window_title(hwnd: HWND) -> String {
    unsafe {
        let len = GetWindowTextLengthW(hwnd);
        if len <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; len as usize + 1];
        let got = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
        String::from_utf16_lossy(&buf[..got.max(0) as usize])
    }
}

/// The first visible top-level window without an owner -- what .NET calls a
/// process's main window -- that belongs to `pid`, or whose title contains
/// `title` (ignoring case).
fn find_window(title: Option<&str>, pid: u32) -> Option<Found> {
    struct Search {
        title: Option<String>,
        pid: u32,
        found: Option<Found>,
    }
    unsafe extern "system" fn each(hwnd: HWND, data: LPARAM) -> BOOL {
        let search = &mut *(data as *mut Search);
        if IsWindowVisible(hwnd) == 0 || !GetWindow(hwnd, GW_OWNER).is_null() {
            return TRUE;
        }
        // Programs keep 0x0 and 1x1 "visible" windows for messages; nothing
        // in them is worth a picture.
        let mut rect: RECT = std::mem::zeroed();
        if GetWindowRect(hwnd, &mut rect) == 0 || rect.right - rect.left < 2 || rect.bottom - rect.top < 2 {
            return TRUE;
        }
        let mut owner_pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut owner_pid);
        let title = window_title(hwnd);
        let matches = match &search.title {
            Some(wanted) => !title.is_empty() && title.to_lowercase().contains(wanted.as_str()),
            None => owner_pid == search.pid,
        };
        if matches {
            search.found = Some(Found { hwnd, pid: owner_pid, title });
            return 0; // stop
        }
        TRUE
    }
    let mut search = Search { title: title.map(str::to_lowercase), pid, found: None };
    unsafe {
        EnumWindows(Some(each), &mut search as *mut Search as LPARAM);
    }
    search.found
}

/// A frame where a 5x5 grid of samples is all one colour is what some GPU
/// drivers return from `PrintWindow` while reporting success.
fn looks_blank(rgba: &[u8], width: i32, height: i32) -> bool {
    let mut seen = std::collections::HashSet::new();
    for xf in [0.1, 0.3, 0.5, 0.7, 0.9] {
        for yf in [0.1, 0.3, 0.5, 0.7, 0.9] {
            let x = ((width as f64 * xf) as i32).clamp(0, width - 1) as usize;
            let y = ((height as f64 * yf) as i32).clamp(0, height - 1) as usize;
            let at = (y * width as usize + x) * 4;
            seen.insert([rgba[at], rgba[at + 1], rgba[at + 2]]);
        }
    }
    seen.len() < 2
}

/// One window: the first whose title contains `title`, else the main window
/// of `pid`. Rendered with `PrintWindow`, so a covered window is captured as
/// it is; when that yields nothing usable, the window is brought forward for
/// a moment, copied from the screen, and the previous foreground restored.
pub fn capture_window(path: &Path, title: Option<&str>, pid: u32) -> Result<Value> {
    physical_pixels();
    let window = find_window(title, pid).ok_or_else(|| anyhow!("No matching window found"))?;
    let hwnd = window.hwnd;
    unsafe {
        if IsIconic(hwnd) != 0 {
            ShowWindowAsync(hwnd, SW_RESTORE);
            std::thread::sleep(std::time::Duration::from_millis(120));
        }
    }
    // The real window rectangle, not DWM's extended frame bounds: those can
    // be a few pixels smaller, and GPU-backed windows then return a blank
    // frame for the smaller bitmap.
    let mut rect: RECT = unsafe { std::mem::zeroed() };
    if unsafe { GetWindowRect(hwnd, &mut rect) } == 0 {
        bail!("GetWindowRect failed");
    }
    let (width, height) = (rect.right - rect.left, rect.bottom - rect.top);
    if width <= 0 || height <= 0 {
        bail!("Invalid window bounds");
    }
    let (printed, mut rgba) =
        grab(width, height, |memory| unsafe { PrintWindow(hwnd, memory, PW_RENDERFULLCONTENT) != 0 })?;
    let mut mode = "print_window";
    if !printed || looks_blank(&rgba, width, height) {
        unsafe {
            let previous = GetForegroundWindow();
            ShowWindowAsync(hwnd, SW_SHOW);
            SetForegroundWindow(hwnd);
            std::thread::sleep(std::time::Duration::from_millis(220));
            let copied = grab(width, height, from_screen(rect.left, rect.top, width, height));
            if !previous.is_null() && previous != hwnd {
                SetForegroundWindow(previous);
            }
            let (ok, pixels) = copied?;
            if !ok {
                bail!("BitBlt from the screen failed");
            }
            rgba = pixels;
        }
        mode = "focused_screen";
    }
    save_png(path, width, height, rgba)?;
    Ok(json!({
        "path": path.display().to_string(),
        "width": width,
        "height": height,
        "left": rect.left,
        "top": rect.top,
        "pid": window.pid,
        "title": window.title,
        "mode": mode,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_of_one_colour_is_blank_and_a_varied_one_is_not() {
        let (w, h) = (20, 10);
        let flat = vec![7u8; (w * h * 4) as usize];
        assert!(looks_blank(&flat, w, h));
        let mut varied = flat.clone();
        let at = ((h as usize / 2) * w as usize + w as usize / 2) * 4;
        varied[at] = 200;
        assert!(!looks_blank(&varied, w, h));
    }

    /// Touches the real desktop: `cargo test -p unterm-mcp win_capture -- --ignored`.
    #[test]
    #[ignore]
    fn captures_the_screen_and_a_window_without_powershell() {
        let dir = std::env::temp_dir().join("unterm-capture-test");
        std::fs::create_dir_all(&dir).unwrap();
        let screen = capture_screen(&dir.join("screen.png")).unwrap();
        let (w, h) = (screen["width"].as_i64().unwrap(), screen["height"].as_i64().unwrap());
        assert!(w > 0 && h > 0, "{screen}");
        let png = image::open(dir.join("screen.png")).unwrap();
        assert_eq!((png.width() as i64, png.height() as i64), (w, h));
        // Whatever is in front now -- the terminal running this test.
        let front = unsafe { GetForegroundWindow() };
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(front, &mut pid) };
        let title = window_title(front);
        let found = find_window(Some(&title), 0).expect("the foreground window");
        assert!(!found.title.is_empty());
        let shot = capture_window(&dir.join("window.png"), Some(&title), 0).unwrap();
        assert_eq!(shot["pid"].as_u64().unwrap() as u32, found.pid);
        assert!(shot["width"].as_i64().unwrap() > 0, "{shot}");
        eprintln!("screen {screen}\nwindow {shot}");
    }
}
