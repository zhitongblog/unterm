//! What Windows 11 draws for a window, asked of it rather than imitated.
//!
//! A window that paints its own title bar keeps looking like a port unless
//! the compositor is told what the window is: that it is dark, what colour
//! its edge is when it has focus and when it does not, and where its maximise
//! button is. The last is what the Snap Layouts flyout hangs off -- Windows
//! shows it when the pointer rests on whatever `WM_NCHITTEST` calls
//! `HTMAXBUTTON`, and an app that draws its own buttons has to say where that
//! is. WezTerm does exactly this; Warp does not, and its issue #6638 is the
//! complaint that follows.
//!
//! Everything here is a no-op off Windows, so callers need no `cfg`.

use winit::window::Window;

/// Where a window's maximise button is, in physical client pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ButtonRect {
    pub left: i32,
    pub top: i32,
    pub width: i32,
    pub height: i32,
}

impl ButtonRect {
    #[cfg_attr(not(windows), allow(dead_code))]
    fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.left + self.width && y >= self.top && y < self.top + self.height
    }
}

#[cfg(windows)]
mod imp {
    use super::ButtonRect;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use winapi::shared::minwindef::{BOOL, DWORD, LPARAM, LRESULT, UINT, WPARAM};
    use winapi::shared::windef::{HWND, POINT};
    use winapi::um::winuser;
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use winit::window::Window;

    const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
    const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
    const DWMWA_BORDER_COLOR: u32 = 34;
    const DWMWA_SYSTEMBACKDROP_TYPE: u32 = 38;
    const DWMWCP_ROUND: u32 = 2;
    const DWMSBT_TABBEDWINDOW: u32 = 4;
    const SUBCLASS_ID: usize = 0x756e_7465; // "unte"

    /// Per-window caption state, written by the window procedure and read by
    /// the event loop.
    #[derive(Default)]
    struct Caption {
        maximise: Option<ButtonRect>,
        hovered: bool,
        pressed: bool,
        clicked: bool,
    }

    fn captions() -> &'static Mutex<HashMap<isize, Caption>> {
        static CAPTIONS: std::sync::OnceLock<Mutex<HashMap<isize, Caption>>> =
            std::sync::OnceLock::new();
        CAPTIONS.get_or_init(Default::default)
    }

    pub fn hwnd(window: &Window) -> Option<HWND> {
        let handle = window.window_handle().ok()?;
        match handle.as_raw() {
            RawWindowHandle::Win32(win32) => Some(win32.hwnd.get() as HWND),
            _ => None,
        }
    }

    unsafe fn set_u32(hwnd: HWND, attribute: u32, value: u32) -> bool {
        winapi::um::dwmapi::DwmSetWindowAttribute(
            hwnd,
            attribute,
            &value as *const u32 as *const std::ffi::c_void,
            std::mem::size_of::<u32>() as u32,
        ) >= 0
    }

    pub fn round_corners(window: &Window) {
        if let Some(hwnd) = hwnd(window) {
            // Windows 10 does not know the attribute and says so; square
            // corners there are the platform's own look.
            unsafe { set_u32(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND) };
        }
    }

    pub fn set_dark(window: &Window, dark: bool) {
        if let Some(hwnd) = hwnd(window) {
            unsafe { set_u32(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, dark as BOOL as u32) };
        }
    }

    pub fn set_border(window: &Window, rgb: [f32; 4]) {
        let Some(hwnd) = hwnd(window) else { return };
        let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
        // COLORREF is 0x00BBGGRR.
        let colorref = byte(rgb[0]) | (byte(rgb[1]) << 8) | (byte(rgb[2]) << 16);
        unsafe { set_u32(hwnd, DWMWA_BORDER_COLOR, colorref) };
    }

    pub fn enable_mica_alt(window: &Window) -> bool {
        let Some(hwnd) = hwnd(window) else { return false };
        // The backdrop is drawn behind the frame; extending the frame over
        // the whole client area is what lets it show through the pixels we
        // leave transparent. Before 22H2 the attribute does not exist and
        // the call fails, which is the whole feature test.
        let margins = winapi::um::uxtheme::MARGINS {
            cxLeftWidth: -1,
            cxRightWidth: -1,
            cyTopHeight: -1,
            cyBottomHeight: -1,
        };
        unsafe {
            if !set_u32(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, DWMSBT_TABBEDWINDOW) {
                return false;
            }
            if winapi::um::dwmapi::DwmExtendFrameIntoClientArea(hwnd, &margins) < 0 {
                return false;
            }
            // With the frame extended over the client area, DWM draws its own
            // minimise/maximise/close into it -- behind the bar we leave
            // transparent for the backdrop, so every button showed twice. It
            // draws them for a window with a system menu; this window's
            // buttons are its own, so it goes without one while Mica is on.
            // The minimise and maximise styles stay: they are what Snap and
            // the taskbar act on.
            use winapi::um::winuser::{
                GetWindowLongPtrW, SetWindowLongPtrW, SetWindowPos, GWL_STYLE, SWP_FRAMECHANGED,
                SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, WS_SYSMENU,
            };
            let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
            SetWindowLongPtrW(hwnd, GWL_STYLE, style & !(WS_SYSMENU as isize));
            SetWindowPos(
                hwnd,
                std::ptr::null_mut(),
                0,
                0,
                0,
                0,
                SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
            true
        }
    }

    pub fn install_caption(window: &Window) {
        let Some(hwnd) = hwnd(window) else { return };
        captions().lock().unwrap().entry(hwnd as isize).or_default();
        unsafe {
            winapi::um::commctrl::SetWindowSubclass(hwnd, Some(caption_proc), SUBCLASS_ID, 0);
        }
    }

    pub fn forget(window: &Window) {
        if let Some(hwnd) = hwnd(window) {
            captions().lock().unwrap().remove(&(hwnd as isize));
        }
    }

    pub fn set_maximise_button(window: &Window, rect: Option<ButtonRect>) {
        if let Some(hwnd) = hwnd(window) {
            if let Some(caption) = captions().lock().unwrap().get_mut(&(hwnd as isize)) {
                caption.maximise = rect;
            }
        }
    }

    pub fn maximise_hovered(window: &Window) -> bool {
        hwnd(window)
            .and_then(|hwnd| {
                captions()
                    .lock()
                    .unwrap()
                    .get(&(hwnd as isize))
                    .map(|caption| caption.hovered)
            })
            .unwrap_or(false)
    }

    pub fn maximise_pressed(window: &Window) -> bool {
        hwnd(window)
            .and_then(|hwnd| {
                captions()
                    .lock()
                    .unwrap()
                    .get(&(hwnd as isize))
                    .map(|caption| caption.pressed)
            })
            .unwrap_or(false)
    }

    pub fn take_maximise_click(window: &Window) -> bool {
        hwnd(window)
            .and_then(|hwnd| {
                captions()
                    .lock()
                    .unwrap()
                    .get_mut(&(hwnd as isize))
                    .map(|caption| std::mem::take(&mut caption.clicked))
            })
            .unwrap_or(false)
    }

    /// Ask for a repaint: `WM_PAINT` reaches winit as `RedrawRequested`, so the
    /// hover the procedure just recorded is drawn without waiting for a tick.
    unsafe fn repaint(hwnd: HWND) {
        winuser::RedrawWindow(hwnd, std::ptr::null(), std::ptr::null_mut(), winuser::RDW_INTERNALPAINT);
    }

    /// A DirectComposition tree for one window: device, target, one visual.
    ///
    /// A swap chain bound straight to a window is opaque whatever alpha it
    /// is given; one presented through a composition visual keeps its alpha,
    /// which is what lets Mica show through the frame we leave transparent.
    pub struct Composition {
        device: *mut winapi::um::dcomp::IDCompositionDevice,
        target: *mut winapi::um::dcomp::IDCompositionTarget,
        visual: *mut winapi::um::dcomp::IDCompositionVisual,
    }

    impl Composition {
        pub fn new(window: &Window) -> Option<Self> {
            use winapi::Interface;
            let hwnd = hwnd(window)?;
            unsafe {
                let mut device: *mut std::ffi::c_void = std::ptr::null_mut();
                let created = winapi::um::dcomp::DCompositionCreateDevice2(
                    std::ptr::null_mut(),
                    &winapi::um::dcomp::IDCompositionDevice::uuidof(),
                    &mut device,
                );
                if created < 0 || device.is_null() {
                    return None;
                }
                let device = device as *mut winapi::um::dcomp::IDCompositionDevice;
                let mut target = std::ptr::null_mut();
                if (*device).CreateTargetForHwnd(hwnd, 1, &mut target) < 0 || target.is_null() {
                    (*device).Release();
                    return None;
                }
                let mut visual = std::ptr::null_mut();
                if (*device).CreateVisual(&mut visual) < 0 || visual.is_null() {
                    (*target).Release();
                    (*device).Release();
                    return None;
                }
                if (*target).SetRoot(visual as *mut _) < 0 {
                    (*visual).Release();
                    (*target).Release();
                    (*device).Release();
                    return None;
                }
                Some(Composition { device, target, visual })
            }
        }

        /// The visual, as wgpu's `SurfaceTargetUnsafe::CompositionVisual`
        /// takes it.
        pub fn visual(&self) -> *mut std::ffi::c_void {
            self.visual as *mut std::ffi::c_void
        }

        /// Make what was attached to the visual visible. wgpu sets the swap
        /// chain as the visual's content but does not commit the device.
        pub fn commit(&self) -> bool {
            unsafe { (*self.device).Commit() >= 0 }
        }
    }

    impl Drop for Composition {
        fn drop(&mut self) {
            unsafe {
                (*self.visual).Release();
                (*self.target).Release();
                (*self.device).Release();
            }
        }
    }

    unsafe extern "system" fn caption_proc(
        hwnd: HWND,
        message: UINT,
        wparam: WPARAM,
        lparam: LPARAM,
        _id: usize,
        _data: usize,
    ) -> LRESULT {
        let key = hwnd as isize;
        match message {
            winuser::WM_NCHITTEST => {
                let result = winapi::um::commctrl::DefSubclassProc(hwnd, message, wparam, lparam);
                if result != winuser::HTCLIENT as LRESULT {
                    return result;
                }
                let mut point = POINT {
                    x: (lparam & 0xFFFF) as i16 as i32,
                    y: ((lparam >> 16) & 0xFFFF) as i16 as i32,
                };
                winuser::ScreenToClient(hwnd, &mut point);
                let over = captions()
                    .lock()
                    .unwrap()
                    .get(&key)
                    .and_then(|caption| caption.maximise)
                    .is_some_and(|rect| rect.contains(point.x, point.y));
                if over {
                    return winuser::HTMAXBUTTON as LRESULT;
                }
                result
            }
            winuser::WM_NCMOUSEMOVE => {
                let over = wparam == winuser::HTMAXBUTTON as WPARAM;
                let changed = {
                    let mut map = captions().lock().unwrap();
                    let caption = map.entry(key).or_default();
                    let changed = caption.hovered != over;
                    caption.hovered = over;
                    if !over {
                        caption.pressed = false;
                    }
                    changed
                };
                if over {
                    let mut track = winuser::TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<winuser::TRACKMOUSEEVENT>() as DWORD,
                        dwFlags: winuser::TME_LEAVE | winuser::TME_NONCLIENT,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    winuser::TrackMouseEvent(&mut track);
                }
                if changed {
                    repaint(hwnd);
                }
                if over {
                    return 0;
                }
                winapi::um::commctrl::DefSubclassProc(hwnd, message, wparam, lparam)
            }
            winuser::WM_NCMOUSELEAVE => {
                if let Some(caption) = captions().lock().unwrap().get_mut(&key) {
                    caption.hovered = false;
                    caption.pressed = false;
                }
                repaint(hwnd);
                winapi::um::commctrl::DefSubclassProc(hwnd, message, wparam, lparam)
            }
            // The button is ours: the default procedure would press a classic
            // caption button that is not there, or start a move.
            winuser::WM_NCLBUTTONDOWN | winuser::WM_NCLBUTTONDBLCLK
                if wparam == winuser::HTMAXBUTTON as WPARAM =>
            {
                if let Some(caption) = captions().lock().unwrap().get_mut(&key) {
                    caption.pressed = true;
                }
                repaint(hwnd);
                0
            }
            winuser::WM_NCLBUTTONUP if wparam == winuser::HTMAXBUTTON as WPARAM => {
                if let Some(caption) = captions().lock().unwrap().get_mut(&key) {
                    if caption.pressed {
                        caption.clicked = true;
                    }
                    caption.pressed = false;
                }
                repaint(hwnd);
                0
            }
            _ => winapi::um::commctrl::DefSubclassProc(hwnd, message, wparam, lparam),
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::ButtonRect;
    use winit::window::Window;
    /// No composition off Windows; the type exists so callers need no `cfg`.
    pub struct Composition;
    impl Composition {
        pub fn new(_: &Window) -> Option<Self> {
            None
        }
        pub fn visual(&self) -> *mut std::ffi::c_void {
            std::ptr::null_mut()
        }
        pub fn commit(&self) -> bool {
            false
        }
    }
    pub fn round_corners(_: &Window) {}
    pub fn set_dark(_: &Window, _: bool) {}
    pub fn set_border(_: &Window, _: [f32; 4]) {}
    pub fn enable_mica_alt(_: &Window) -> bool {
        false
    }
    pub fn install_caption(_: &Window) {}
    pub fn forget(_: &Window) {}
    pub fn set_maximise_button(_: &Window, _: Option<ButtonRect>) {}
    pub fn maximise_hovered(_: &Window) -> bool {
        false
    }
    pub fn maximise_pressed(_: &Window) -> bool {
        false
    }
    pub fn take_maximise_click(_: &Window) -> bool {
        false
    }
}

pub use imp::Composition;

/// Rounded corners, as every other Windows 11 window has.
pub fn round_corners(window: &Window) {
    imp::round_corners(window)
}

/// Tell the compositor the window is dark, so the parts it draws -- the
/// resize border, the shadow's edge, the Alt-Tab thumbnail's frame -- match.
pub fn set_dark(window: &Window, dark: bool) {
    imp::set_dark(window, dark)
}

/// The one-pixel edge Windows 11 draws around a window, in the window's own
/// colours rather than the system's light grey.
pub fn set_border(window: &Window, rgb: [f32; 4]) {
    imp::set_border(window, rgb)
}

/// Put Mica Alt behind the window. False when this Windows has no such
/// backdrop (before 22H2) -- the caller keeps its solid frame.
pub fn enable_mica_alt(window: &Window) -> bool {
    imp::enable_mica_alt(window)
}

/// Start answering hit tests for the maximise button, so Snap Layouts appear.
pub fn install_caption(window: &Window) {
    imp::install_caption(window)
}

/// Drop what is kept about a window that is closing.
pub fn forget(window: &Window) {
    imp::forget(window)
}

/// Where the maximise button is now; `None` when the bar does not show one.
pub fn set_maximise_button(window: &Window, rect: Option<ButtonRect>) {
    imp::set_maximise_button(window, rect)
}

/// The pointer is on the maximise button. Over it, Windows sends the window
/// non-client messages rather than the client ones winit reports, so this is
/// the only way the bar learns about the hover.
pub fn maximise_hovered(window: &Window) -> bool {
    imp::maximise_hovered(window)
}

/// The maximise button is held down.
pub fn maximise_pressed(window: &Window) -> bool {
    imp::maximise_pressed(window)
}

/// A click on the maximise button finished since last asked.
pub fn take_maximise_click(window: &Window) -> bool {
    imp::take_maximise_click(window)
}

#[cfg(test)]
mod caption_tests {
    use super::ButtonRect;

    #[test]
    fn a_button_contains_its_own_pixels_and_no_others() {
        let rect = ButtonRect { left: 100, top: 0, width: 46, height: 32 };
        assert!(rect.contains(100, 0));
        assert!(rect.contains(145, 31));
        assert!(!rect.contains(146, 10), "the right edge belongs to the next button");
        assert!(!rect.contains(120, 32), "below the bar is the terminal");
        assert!(!rect.contains(99, 10));
    }
}
