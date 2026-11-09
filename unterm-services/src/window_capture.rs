//! Taking a picture of one of this machine's windows.
//!
//! The terminal's own window, normally: an agent that can read the screen as
//! text still cannot see what a person sees -- which pane is focused, whether
//! the tab bar is showing, what the selection looks like. `capture.window` is
//! how it looks, and `selftest.run` checks that it works.
//!
//! Two ways to take it, because the obvious one is not reliable here.
//! `PrintWindow` asks the window to draw itself into a bitmap, which is exact
//! and works when the window is covered -- but this window is drawn on the GPU
//! through a compositor, and a window like that commonly answers with nothing.
//! So the result is checked rather than trusted, and a picture of the screen
//! where the window is falls in behind it. Which one was used is reported, not
//! hidden, because they are not equivalent: the second one sees whatever is on
//! top of the window as well.

use anyhow::Context as _;

/// A captured window.
#[derive(Debug)]
pub struct WindowImage {
    pub width: usize,
    pub height: usize,
    /// RGBA, top row first, ready for an encoder.
    pub pixels: Vec<u8>,
    /// How it was taken: `print_window` or `focused_screen`.
    pub mode: &'static str,
}

/// Capture a window of this machine's, chosen by title and/or owning process.
///
/// With neither filter there is nothing to choose between windows, so the
/// caller must name one -- guessing would hand back some other application.
#[cfg(windows)]
pub fn capture_window(title: Option<&str>, pid: Option<u32>) -> anyhow::Result<WindowImage> {
    use anyhow::{anyhow, Context};

    if title.is_none() && pid.is_none() {
        return Err(anyhow!("name a window by title or pid to capture it"));
    }
    let window = find_window(title, pid).context("find the window to capture")?;
    let (width, height) = window_size(window)?;
    if width == 0 || height == 0 {
        return Err(anyhow!("the window has no area to capture"));
    }

    // The window drawing itself, first. It is the honest answer: exact, and
    // unaffected by anything sitting on top.
    if let Ok(image) = print_window(window, width, height) {
        if has_content(&image) {
            return Ok(WindowImage {
                width,
                height,
                pixels: image,
                mode: "print_window",
            });
        }
    }

    // A GPU-composited window that declined to draw. What is on the screen
    // where it is remains true, and is what a person would see.
    let pixels = screen_under(window, width, height)?;
    Ok(WindowImage {
        width,
        height,
        pixels,
        mode: "focused_screen",
    })
}

/// Capture a window by asking `screencapture -l <CGWindowID>` to write a PNG
/// and reading the pixels back.
///
/// The same route 0.57.4 took, kept for the same reasons: the tool renders
/// exactly what the compositor shows, works on every macOS this product
/// supports, and triggers the one Screen Recording permission prompt a person
/// has already answered for the old version.
#[cfg(target_os = "macos")]
pub fn capture_window(title: Option<&str>, pid: Option<u32>) -> anyhow::Result<WindowImage> {
    use anyhow::anyhow;

    if title.is_none() && pid.is_none() {
        return Err(anyhow!("name a window by title or pid to capture it"));
    }
    let target_pid = pid.unwrap_or_else(std::process::id);
    let is_self = pid.map_or(true, |wanted| wanted == std::process::id());

    // Self-capture race: right after window creation the NSWindow exists but
    // CGWindowList may not yet flag it onScreen. A few short retries cover
    // every startup measured; an external pid stays single-shot so a wrong
    // pid fails fast instead of blocking.
    let attempts = if is_self { 5 } else { 1 };
    let mut window_id = None;
    for attempt in 0..attempts {
        window_id = macos::find_window_id(target_pid, title)?;
        if window_id.is_some() {
            break;
        }
        if attempt + 1 < attempts {
            std::thread::sleep(std::time::Duration::from_millis(120));
        }
    }
    let window_id = window_id.ok_or_else(|| anyhow!("no visible window matched"))?;

    let path = macos::scratch_png("window");
    let status = std::process::Command::new("/usr/sbin/screencapture")
        .args(["-x", "-o", "-t", "png", "-l", &window_id.to_string()])
        .arg(&path)
        .status()
        .map_err(|err| anyhow!("invoke /usr/sbin/screencapture: {err}"))?;
    let image = macos::read_png(&path, status, "screencapture_window");
    let _ = std::fs::remove_file(&path);
    image
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn capture_window(_title: Option<&str>, _pid: Option<u32>) -> anyhow::Result<WindowImage> {
    anyhow::bail!("capturing a window is not implemented on this platform yet")
}

/// A rectangle on the desktop, in physical pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    pub left: i32,
    pub top: i32,
    pub width: usize,
    pub height: usize,
}

impl Region {
    /// The rectangle between two corners, whichever way round they were
    /// dragged.
    ///
    /// People drag up and to the left as often as down and to the right, and a
    /// capture that only works one way is a capture that fails half the time
    /// with nothing to say why.
    pub fn between(from: (i32, i32), to: (i32, i32)) -> Self {
        let left = from.0.min(to.0);
        let top = from.1.min(to.1);
        Self {
            left,
            top,
            width: (from.0 - to.0).unsigned_abs() as usize,
            height: (from.1 - to.1).unsigned_abs() as usize,
        }
    }

    /// Whether there is anything to capture.
    ///
    /// A click without a drag is how anyone cancels; a one-pixel PNG is not
    /// what they meant by it.
    pub fn is_usable(&self) -> bool {
        self.width >= MIN_REGION && self.height >= MIN_REGION
    }
}

/// Below this, a drag was a click.
const MIN_REGION: usize = 8;

/// Copy a rectangle of the desktop.
#[cfg(windows)]
pub fn capture_region(region: Region) -> anyhow::Result<WindowImage> {
    use anyhow::Context;
    use winapi::um::wingdi::{BitBlt, CAPTUREBLT, SRCCOPY};
    use winapi::um::winuser::{GetDC, ReleaseDC};

    if !region.is_usable() {
        anyhow::bail!("the region is too small to capture");
    }
    // SAFETY: the DC is released on every path out.
    unsafe {
        let screen_dc = GetDC(std::ptr::null_mut());
        if screen_dc.is_null() {
            anyhow::bail!("no device context for the screen");
        }
        let copied = into_bitmap(screen_dc, region.width, region.height, |memory_dc| {
            BitBlt(
                memory_dc,
                0,
                0,
                region.width as i32,
                region.height as i32,
                screen_dc,
                region.left,
                region.top,
                // CAPTUREBLT includes layered/composited windows. Without it
                // BitBlt can fail outright on current Windows desktops rather
                // than merely omitting translucent windows from the result.
                SRCCOPY | CAPTUREBLT,
            ) != 0
        });
        ReleaseDC(std::ptr::null_mut(), screen_dc);
        match copied {
            Ok(pixels) => Ok(WindowImage {
                width: region.width,
                height: region.height,
                pixels,
                mode: "region",
            }),
            Err(screen_error) => capture_region_from_own_window(region).with_context(|| {
                format!(
                    "desktop capture failed ({screen_error}); \
                     the selected rectangle was not available from Unterm's own window"
                )
            }),
        }
    }
}

/// Copy a rectangle of the desktop: capture the screen at native resolution,
/// then crop.
///
/// `screencapture -R` takes points while this API speaks physical pixels, and
/// the conversion depends on which display the rectangle is on. A full-screen
/// grab is already in physical pixels, so cropping it needs no conversion at
/// all.
#[cfg(target_os = "macos")]
pub fn capture_region(region: Region) -> anyhow::Result<WindowImage> {
    use anyhow::anyhow;

    if !region.is_usable() {
        anyhow::bail!("the region is too small to capture");
    }
    if region.left < 0 || region.top < 0 {
        // The full-screen grab below is of the main display, whose physical
        // origin is 0,0; a rectangle on a display left of or above it cannot
        // be cut from that picture.
        anyhow::bail!("capturing a region outside the main display is not supported on macOS yet");
    }

    let path = macos::scratch_png("region");
    let status = std::process::Command::new("/usr/sbin/screencapture")
        .args(["-x", "-t", "png"])
        .arg(&path)
        .status()
        .map_err(|err| anyhow!("invoke /usr/sbin/screencapture: {err}"))?;
    let screen = macos::read_png(&path, status, "region");
    let _ = std::fs::remove_file(&path);
    crop_image(
        screen?,
        region.left as usize,
        region.top as usize,
        region.width,
        region.height,
        "region",
    )
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn capture_region(_region: Region) -> anyhow::Result<WindowImage> {
    anyhow::bail!("capturing a region is not implemented on this platform yet")
}

/// The macOS half of the platform work: window lookup and PNG readback.
#[cfg(target_os = "macos")]
mod macos {
    use super::WindowImage;
    use anyhow::{anyhow, Context as _};

    /// A path for `screencapture` to write, unique enough for concurrent
    /// captures from one process.
    pub fn scratch_png(kind: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "unterm_capture_{kind}_{}_{}.png",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed),
        ))
    }

    /// What `screencapture` wrote, as pixels.
    pub fn read_png(
        path: &std::path::Path,
        status: std::process::ExitStatus,
        mode: &'static str,
    ) -> anyhow::Result<WindowImage> {
        if !status.success() {
            anyhow::bail!("screencapture exited with {status}");
        }
        let file = std::fs::File::open(path)
            .with_context(|| format!("screencapture did not produce {}", path.display()))?;
        let decoder = png::Decoder::new(std::io::BufReader::new(file));
        let mut reader = decoder.read_info().context("read screencapture PNG")?;
        let mut buffer = vec![0u8; reader.output_buffer_size().unwrap_or_default()];
        let info = reader
            .next_frame(&mut buffer)
            .context("decode screencapture PNG")?;
        buffer.truncate(info.buffer_size());

        // screencapture writes 8-bit RGB or RGBA; either way the answer here
        // is RGBA with the alpha squashed to opaque, which is the truth for a
        // picture of a screen.
        let pixels = match (info.color_type, info.bit_depth) {
            (png::ColorType::Rgba, png::BitDepth::Eight) => {
                let mut pixels = buffer;
                for pixel in pixels.chunks_exact_mut(4) {
                    pixel[3] = 255;
                }
                pixels
            }
            (png::ColorType::Rgb, png::BitDepth::Eight) => {
                let mut pixels = Vec::with_capacity(info.width as usize * info.height as usize * 4);
                for pixel in buffer.chunks_exact(3) {
                    pixels.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]);
                }
                pixels
            }
            (color, depth) => {
                anyhow::bail!("screencapture wrote an unexpected {color:?}/{depth:?} PNG")
            }
        };
        Ok(WindowImage {
            width: info.width as usize,
            height: info.height as usize,
            pixels,
            mode,
        })
    }

    /// The first on-screen CGWindowID owned by `pid`, optionally with `title`
    /// in its name.
    pub fn find_window_id(pid: u32, title: Option<&str>) -> anyhow::Result<Option<u32>> {
        use core_foundation::array::CFArray;
        use core_foundation::base::{CFType, TCFType};
        use core_foundation::dictionary::CFDictionary;
        use core_foundation::number::CFNumber;
        use core_foundation::string::CFString;
        use core_graphics::display::{
            kCGNullWindowID, kCGWindowListExcludeDesktopElements, kCGWindowListOptionOnScreenOnly,
            CGWindowListCopyWindowInfo,
        };

        let info: CFArray<CFDictionary<CFString, CFType>> = unsafe {
            let raw = CGWindowListCopyWindowInfo(
                kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements,
                kCGNullWindowID,
            );
            if raw.is_null() {
                return Err(anyhow!(
                    "the window list was not available; Screen Recording permission may be missing"
                ));
            }
            CFArray::wrap_under_create_rule(raw)
        };

        let pid_key = CFString::from_static_string("kCGWindowOwnerPID");
        let id_key = CFString::from_static_string("kCGWindowNumber");
        let name_key = CFString::from_static_string("kCGWindowName");

        for entry in info.iter() {
            let owner: i64 = entry
                .find(&pid_key)
                .and_then(|value| value.downcast::<CFNumber>())
                .and_then(|number| number.to_i64())
                .unwrap_or(-1);
            if owner != pid as i64 {
                continue;
            }
            if let Some(needle) = title {
                let name = entry
                    .find(&name_key)
                    .and_then(|value| value.downcast::<CFString>())
                    .map(|name| name.to_string())
                    .unwrap_or_default();
                if !name.to_lowercase().contains(&needle.to_lowercase()) {
                    continue;
                }
            }
            let id: i64 = entry
                .find(&id_key)
                .and_then(|value| value.downcast::<CFNumber>())
                .and_then(|number| number.to_i64())
                .unwrap_or(0);
            if id > 0 {
                return Ok(Some(id as u32));
            }
        }
        Ok(None)
    }
}

/// Windows can deny reads from the desktop DC in protected or remote
/// sessions even though the app is allowed to draw and capture its own
/// window. The interactive selector lives inside that window, so an exact
/// crop of a clean `PrintWindow` frame is an equivalent fallback there.
#[cfg(windows)]
fn capture_region_from_own_window(region: Region) -> anyhow::Result<WindowImage> {
    use winapi::shared::windef::RECT;
    use winapi::um::winuser::GetWindowRect;

    let window = find_window(None, Some(std::process::id()))?;
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    // SAFETY: `rect` is ours and `find_window` returned a live HWND.
    if unsafe { GetWindowRect(window, &mut rect) } == 0 {
        anyhow::bail!("could not measure Unterm's window: {}", last_error());
    }
    let right = region
        .left
        .checked_add(region.width as i32)
        .context("selected region exceeds Windows coordinates")?;
    let bottom = region
        .top
        .checked_add(region.height as i32)
        .context("selected region exceeds Windows coordinates")?;
    if region.left < rect.left
        || region.top < rect.top
        || right > rect.right
        || bottom > rect.bottom
    {
        anyhow::bail!("the selected rectangle extends outside Unterm's window");
    }

    let image = capture_window(None, Some(std::process::id()))?;
    crop_image(
        image,
        (region.left - rect.left) as usize,
        (region.top - rect.top) as usize,
        region.width,
        region.height,
        "region_window_fallback",
    )
}

fn crop_image(
    image: WindowImage,
    left: usize,
    top: usize,
    width: usize,
    height: usize,
    mode: &'static str,
) -> anyhow::Result<WindowImage> {
    let right = left.checked_add(width).context("crop width overflow")?;
    let bottom = top.checked_add(height).context("crop height overflow")?;
    if right > image.width || bottom > image.height {
        anyhow::bail!(
            "crop {left},{top} {width}x{height} exceeds image {}x{}",
            image.width,
            image.height
        );
    }
    let mut pixels = Vec::with_capacity(width.saturating_mul(height).saturating_mul(4));
    let source_stride = image.width * 4;
    let row_bytes = width * 4;
    for row in top..bottom {
        let start = row * source_stride + left * 4;
        pixels.extend_from_slice(&image.pixels[start..start + row_bytes]);
    }
    Ok(WindowImage {
        width,
        height,
        pixels,
        mode,
    })
}

/// Whether a capture is a picture of something rather than one flat colour.
///
/// `PrintWindow` on a compositor-drawn window succeeds and hands back a blank
/// bitmap. Taking that at its word is how a screenshot tool ends up reporting
/// success and returning a black rectangle, so the pixels are looked at.
#[cfg(windows)]
fn has_content(pixels: &[u8]) -> bool {
    let Some(first) = pixels.chunks_exact(4).next() else {
        return false;
    };
    pixels
        .chunks_exact(4)
        .any(|pixel| pixel[0] != first[0] || pixel[1] != first[1] || pixel[2] != first[2])
}

/// The window's size in real pixels.
///
/// `GetWindowRect` is in screen coordinates, which are physical. The client
/// rect is not: on a scaled display a caller that reads it as physical
/// captures the top-left corner of the window and calls it the whole thing.
#[cfg(windows)]
fn window_size(window: winapi::shared::windef::HWND) -> anyhow::Result<(usize, usize)> {
    use winapi::shared::windef::RECT;
    use winapi::um::winuser::GetWindowRect;

    let mut rect = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    // SAFETY: the rect is ours and the handle was just validated.
    if unsafe { GetWindowRect(window, &mut rect) } == 0 {
        anyhow::bail!("could not measure the window: {}", last_error());
    }
    Ok((
        (rect.right - rect.left).max(0) as usize,
        (rect.bottom - rect.top).max(0) as usize,
    ))
}

/// The first visible top-level window matching the filters.
#[cfg(windows)]
fn find_window(
    title: Option<&str>,
    pid: Option<u32>,
) -> anyhow::Result<winapi::shared::windef::HWND> {
    use winapi::shared::minwindef::{BOOL, DWORD, LPARAM, TRUE};
    use winapi::shared::windef::{HWND, RECT};
    use winapi::um::dwmapi::{DwmGetWindowAttribute, DWMWA_CLOAKED};
    use winapi::um::winuser::{
        EnumWindows, GetWindowRect, GetWindowTextW, GetWindowThreadProcessId, IsIconic,
        IsWindowVisible,
    };

    struct Search {
        title: Option<String>,
        pid: Option<u32>,
        found: Option<HWND>,
        fallback: Option<HWND>,
    }

    unsafe extern "system" fn visit(window: HWND, param: LPARAM) -> BOOL {
        let search = &mut *(param as *mut Search);
        if IsWindowVisible(window) == 0 {
            return TRUE;
        }
        let mut cloaked: DWORD = 0;
        let _ = DwmGetWindowAttribute(
            window,
            DWMWA_CLOAKED,
            &mut cloaked as *mut DWORD as *mut _,
            std::mem::size_of::<DWORD>() as DWORD,
        );
        if cloaked != 0 {
            return TRUE;
        }
        if let Some(wanted) = search.pid {
            let mut owner = 0u32;
            GetWindowThreadProcessId(window, &mut owner);
            if owner != wanted {
                return TRUE;
            }
        }
        if let Some(wanted) = &search.title {
            let mut text = [0u16; 512];
            let length = GetWindowTextW(window, text.as_mut_ptr(), text.len() as i32);
            let text = String::from_utf16_lossy(&text[..length.max(0) as usize]);
            if !text.to_lowercase().contains(&wanted.to_lowercase()) {
                return TRUE;
            }
        }
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        let has_area = GetWindowRect(window, &mut rect) != 0
            && (rect.right - rect.left) >= MIN_REGION as i32
            && (rect.bottom - rect.top) >= MIN_REGION as i32;
        if !has_area || IsIconic(window) != 0 {
            if search.fallback.is_none() && has_area {
                search.fallback = Some(window);
            }
            return TRUE;
        }
        search.found = Some(window);
        0 // FALSE: stop, we have one.
    }

    let mut search = Search {
        title: title.map(str::to_string),
        pid,
        found: None,
        fallback: None,
    };
    // SAFETY: the callback only touches `search`, which outlives the call.
    unsafe {
        EnumWindows(Some(visit), &mut search as *mut Search as LPARAM);
    }
    search
        .found
        .or(search.fallback)
        .ok_or_else(|| anyhow::anyhow!("no visible window matched"))
}

/// Ask the window to draw itself into a bitmap.
#[cfg(windows)]
fn print_window(
    window: winapi::shared::windef::HWND,
    width: usize,
    height: usize,
) -> anyhow::Result<Vec<u8>> {
    use winapi::um::winuser::{GetWindowDC, PrintWindow, ReleaseDC};

    /// Draw the whole window, layered and composited parts included. Without
    /// it a window like this one comes back empty.
    const PW_RENDERFULLCONTENT: u32 = 0x0000_0002;

    // SAFETY: the DC is released on every path out.
    unsafe {
        let window_dc = GetWindowDC(window);
        if window_dc.is_null() {
            anyhow::bail!("no device context for the window");
        }
        let drawn = into_bitmap(window_dc, width, height, |memory_dc| {
            PrintWindow(window, memory_dc, PW_RENDERFULLCONTENT) != 0
        });
        ReleaseDC(window, window_dc);
        drawn
    }
}

/// Copy the screen where the window is.
#[cfg(windows)]
fn screen_under(
    window: winapi::shared::windef::HWND,
    width: usize,
    height: usize,
) -> anyhow::Result<Vec<u8>> {
    use winapi::shared::windef::RECT;
    use winapi::um::wingdi::{BitBlt, CAPTUREBLT, SRCCOPY};
    use winapi::um::winuser::{GetDC, GetWindowRect, ReleaseDC};

    let mut rect = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    // SAFETY: the DC is released on every path out.
    unsafe {
        if GetWindowRect(window, &mut rect) == 0 {
            anyhow::bail!("could not locate the window: {}", last_error());
        }
        let screen_dc = GetDC(std::ptr::null_mut());
        if screen_dc.is_null() {
            anyhow::bail!("no device context for the screen");
        }
        let copied = into_bitmap(screen_dc, width, height, |memory_dc| {
            BitBlt(
                memory_dc,
                0,
                0,
                width as i32,
                height as i32,
                screen_dc,
                rect.left,
                rect.top,
                SRCCOPY | CAPTUREBLT,
            ) != 0
        });
        ReleaseDC(std::ptr::null_mut(), screen_dc);
        copied
    }
}

/// Run `draw` against a fresh top-down 32-bit bitmap and read it back as RGBA.
///
/// Every GDI object made here is freed here, including on the failure paths --
/// a leaked DC or bitmap is a handle the process never gets back, and this
/// runs whenever an agent asks for a screenshot.
#[cfg(windows)]
unsafe fn into_bitmap(
    source_dc: winapi::shared::windef::HDC,
    width: usize,
    height: usize,
    draw: impl FnOnce(winapi::shared::windef::HDC) -> bool,
) -> anyhow::Result<Vec<u8>> {
    use winapi::um::wingdi::{
        CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, SelectObject, BITMAPINFO,
        BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
    };

    let memory_dc = CreateCompatibleDC(source_dc);
    if memory_dc.is_null() {
        anyhow::bail!("could not make a bitmap to draw into");
    }

    let mut info: BITMAPINFO = std::mem::zeroed();
    info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
    info.bmiHeader.biWidth = width as i32;
    // Negative: rows top-down, the order an image encoder wants. A positive
    // height gives them bottom-up and the picture comes out upside down.
    info.bmiHeader.biHeight = -(height as i32);
    info.bmiHeader.biPlanes = 1;
    info.bmiHeader.biBitCount = 32;
    info.bmiHeader.biCompression = BI_RGB;

    let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
    let bitmap = CreateDIBSection(
        source_dc,
        &info,
        DIB_RGB_COLORS,
        &mut bits,
        std::ptr::null_mut(),
        0,
    );
    if bitmap.is_null() || bits.is_null() {
        DeleteDC(memory_dc);
        anyhow::bail!("could not allocate the bitmap");
    }

    let previous = SelectObject(memory_dc, bitmap as *mut _);
    let drawn = draw(memory_dc);
    let draw_error = (!drawn).then(last_error);

    let mut pixels = Vec::new();
    if drawn {
        let raw = std::slice::from_raw_parts(bits as *const u8, width * height * 4);
        pixels.reserve_exact(raw.len());
        // GDI hands back BGRA, and with an alpha channel that the drawing may
        // never have touched. Opaque is the truth for a window's picture.
        for pixel in raw.chunks_exact(4) {
            pixels.extend_from_slice(&[pixel[2], pixel[1], pixel[0], 255]);
        }
    }

    SelectObject(memory_dc, previous);
    DeleteObject(bitmap as *mut _);
    DeleteDC(memory_dc);

    if drawn {
        Ok(pixels)
    } else {
        anyhow::bail!(
            "the source did not draw into the capture bitmap: {}",
            draw_error
                .map(|error| error.to_string())
                .unwrap_or_else(|| "unknown Windows error".to_string())
        )
    }
}

#[cfg(windows)]
fn last_error() -> std::io::Error {
    std::io::Error::last_os_error()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// People drag up and to the left as often as down and to the right.
    #[test]
    fn a_region_is_the_same_whichever_way_it_was_dragged() {
        let forward = Region::between((10, 20), (110, 220));
        let backward = Region::between((110, 220), (10, 20));
        assert_eq!(forward, backward);
        assert_eq!(forward.left, 10);
        assert_eq!(forward.top, 20);
        assert_eq!((forward.width, forward.height), (100, 200));
    }

    /// A click without a drag is how anyone cancels, and a one-pixel PNG is
    /// not what they meant by it.
    #[test]
    fn a_click_is_not_a_region() {
        assert!(!Region::between((10, 10), (10, 10)).is_usable());
        assert!(!Region::between((10, 10), (13, 40)).is_usable());
        assert!(Region::between((10, 10), (110, 110)).is_usable());
    }

    #[test]
    fn a_region_too_small_to_capture_is_refused_rather_than_attempted() {
        let err = capture_region(Region::between((0, 0), (2, 2)))
            .expect_err("a three-pixel drag is a cancelled one");
        // Where regions are unimplemented the refusal is the platform's, and
        // says so instead.
        #[cfg(any(windows, target_os = "macos"))]
        assert!(err.to_string().contains("too small"), "{err}");
        #[cfg(not(any(windows, target_os = "macos")))]
        assert!(err.to_string().contains("not implemented"), "{err}");
    }

    /// Negative coordinates are ordinary: a second monitor to the left of the
    /// first has them, and a capture that clamps to zero grabs the wrong
    /// screen.
    #[test]
    fn a_region_on_a_monitor_left_of_the_first_keeps_its_position() {
        let region = Region::between((-1900, 100), (-1400, 500));
        assert_eq!(region.left, -1900);
        assert_eq!(region.width, 500);
    }

    #[test]
    fn cropping_keeps_the_requested_rows_and_columns() {
        let mut pixels = Vec::new();
        for index in 0..12u8 {
            pixels.extend_from_slice(&[index, index, index, 255]);
        }
        let cropped = crop_image(
            WindowImage {
                width: 4,
                height: 3,
                pixels,
                mode: "source",
            },
            1,
            1,
            2,
            2,
            "crop",
        )
        .expect("in-bounds crop");

        assert_eq!((cropped.width, cropped.height), (2, 2));
        assert_eq!(cropped.mode, "crop");
        assert_eq!(
            cropped
                .pixels
                .chunks_exact(4)
                .map(|pixel| pixel[0])
                .collect::<Vec<_>>(),
            vec![5, 6, 9, 10]
        );
    }

    #[test]
    fn cropping_refuses_a_rectangle_past_the_source_edge() {
        let err = crop_image(
            WindowImage {
                width: 2,
                height: 2,
                pixels: vec![0; 2 * 2 * 4],
                mode: "source",
            },
            1,
            1,
            2,
            2,
            "crop",
        )
        .expect_err("out-of-bounds crops must not index the pixel buffer");

        assert!(err.to_string().contains("exceeds image"));
    }

    /// Naming nothing would mean picking somebody's window at random.
    #[test]
    fn a_capture_has_to_say_which_window() {
        let err = capture_window(None, None).expect_err("no filter is not a request");
        assert!(
            err.to_string().contains("title or pid") || !cfg!(windows),
            "{err}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_process_with_no_window_is_told_so_rather_than_handed_one() {
        // The test harness has no window of its own; the failure must name
        // that, not quietly return whatever else was on screen.
        let err = capture_window(None, Some(std::process::id()))
            .expect_err("a console process has no window");
        // The whole chain: the outermost message says what was being done,
        // and the reason it failed is underneath it.
        assert!(
            format!("{err:#}").contains("no visible window matched"),
            "{err:#}"
        );
    }

    /// One flat colour is what a compositor-drawn window returns when it
    /// declines to draw, and taking that as a picture is how a capture tool
    /// ends up reporting success and handing back a black rectangle.
    #[cfg(windows)]
    #[test]
    fn a_blank_bitmap_is_not_mistaken_for_a_picture() {
        let blank = vec![17u8; 64 * 4];
        assert!(!has_content(&blank));

        let mut drawn = blank.clone();
        drawn[20] = 200;
        assert!(has_content(&drawn));
    }

    #[cfg(windows)]
    #[test]
    fn nothing_is_content_when_there_are_no_pixels() {
        assert!(!has_content(&[]));
    }

    /// Alpha is ignored on purpose: `PrintWindow` leaves it zero over most of
    /// the window, and a capture that respects that is fully transparent.
    #[cfg(windows)]
    #[test]
    fn transparency_alone_does_not_count_as_content() {
        let mut pixels = vec![9u8; 16 * 4];
        for (index, byte) in pixels.iter_mut().enumerate() {
            if index % 4 == 3 {
                *byte = (index as u8).wrapping_mul(7);
            }
        }
        assert!(!has_content(&pixels), "alpha differences are not a picture");
    }
}
