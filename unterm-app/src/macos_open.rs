//! Answers macOS when it says "open these".
//!
//! Finder's right-click, a URL handed to the app, a folder dropped on the
//! Dock icon: they all arrive as `application:openURLs:` on the app
//! delegate — a method winit's delegate does not implement, so for a while
//! the extension asked, the app activated, and nothing happened. The method
//! is added to winit's own delegate class at runtime; the paths land in a
//! queue the event loop drains on its next tick.

#![cfg(target_os = "macos")]

use objc2::runtime::{AnyClass, AnyObject, Sel};
use objc2::{class, msg_send, sel};
use std::path::PathBuf;
use std::sync::Mutex;

static PENDING: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// Paths macOS asked us to open since the last look.
pub fn drain() -> Vec<PathBuf> {
    std::mem::take(&mut *PENDING.lock().unwrap())
}

extern "C" fn open_urls(_this: *mut AnyObject, _cmd: Sel, _app: *mut AnyObject, urls: *mut AnyObject) {
    // SAFETY: AppKit hands us an NSArray<NSURL>; we only read it, on the
    // main thread, for the duration of this call.
    unsafe {
        let count: usize = msg_send![urls, count];
        let mut pending = PENDING.lock().unwrap();
        for index in 0..count {
            let url: *mut AnyObject = msg_send![urls, objectAtIndex: index];
            let is_file: bool = msg_send![url, isFileURL];
            if !is_file {
                // The Finder Sync extension cannot launch us with file URLs —
                // its sandbox refuses NSWorkspace's open-with-application —
                // so it deep-links instead: unterm://open?path=<encoded>.
                let absolute: *mut AnyObject = msg_send![url, absoluteString];
                if !absolute.is_null() {
                    let utf8: *const std::os::raw::c_char = msg_send![absolute, UTF8String];
                    if !utf8.is_null() {
                        let text =
                            std::ffi::CStr::from_ptr(utf8).to_string_lossy().into_owned();
                        if let Some(path) = scheme_path(&text) {
                            pending.push(PathBuf::from(path));
                        }
                    }
                }
                continue;
            }
            let path: *mut AnyObject = msg_send![url, path];
            if path.is_null() {
                continue;
            }
            let utf8: *const std::os::raw::c_char = msg_send![path, UTF8String];
            if utf8.is_null() {
                continue;
            }
            let text = std::ffi::CStr::from_ptr(utf8).to_string_lossy().into_owned();
            pending.push(PathBuf::from(text));
        }
        let waiting = !pending.is_empty();
        drop(pending);
        // A path to open is also a request for a window to open it in. The
        // queue is drained by a loop that only has a window some of the
        // time: parked in the tray, or closed on a Mac, where the process
        // outlives its last window. Without asking, the paths sat in the
        // queue waiting for a click the user had no reason to make.
        if waiting {
            crate::tray::request_wake();
        }
    }
}

/// The Dock, Finder or Spotlight asking an already-running Unterm for a
/// window. Only interesting when there is none to show: with a window up,
/// AppKit's own behaviour -- raise it -- is the right one.
extern "C" fn should_handle_reopen(
    _this: *mut AnyObject,
    _cmd: Sel,
    _app: *mut AnyObject,
    has_visible_windows: bool,
) -> bool {
    if !has_visible_windows {
        crate::tray::request_wake();
    }
    true
}

/// The folder a `unterm://open?path=<percent-encoded>` deep link points at.
fn scheme_path(url: &str) -> Option<String> {
    let rest = url.strip_prefix("unterm://")?;
    let query = rest.split_once('?')?.1;
    let value = query
        .split('&')
        .find_map(|pair| pair.strip_prefix("path="))?;
    let decoded = percent_decode(value);
    if decoded.is_empty() {
        return None;
    }
    Some(decoded)
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if let Some(hex) = bytes.get(i + 1..i + 3) {
                if let Ok(byte) = u8::from_str_radix(&String::from_utf8_lossy(hex), 16) {
                    out.push(byte);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Folder paths a Services invocation put on the pasteboard. Finder sends
/// NSFilenamesPboardType (a plist array of paths); a plain string arrives
/// from text-selection contexts.
unsafe fn pasteboard_paths(pboard: *mut AnyObject) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if pboard.is_null() {
        return paths;
    }
    let filenames_type: *mut AnyObject = ns_string("NSFilenamesPboardType");
    let list: *mut AnyObject = msg_send![pboard, propertyListForType: filenames_type];
    if !list.is_null() {
        let responds: bool = msg_send![list, isKindOfClass: class!(NSArray)];
        if responds {
            let count: usize = msg_send![list, count];
            for index in 0..count {
                let item: *mut AnyObject = msg_send![list, objectAtIndex: index];
                if let Some(text) = ns_string_to_rust(item) {
                    paths.push(PathBuf::from(text));
                }
            }
        }
    }
    if paths.is_empty() {
        let string_type: *mut AnyObject = ns_string("public.utf8-plain-text");
        let text: *mut AnyObject = msg_send![pboard, stringForType: string_type];
        if let Some(text) = ns_string_to_rust(text) {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                paths.push(PathBuf::from(trimmed));
            }
        }
    }
    paths
}

unsafe fn ns_string(text: &str) -> *mut AnyObject {
    let cstr = std::ffi::CString::new(text).unwrap();
    msg_send![class!(NSString), stringWithUTF8String: cstr.as_ptr()]
}

unsafe fn ns_string_to_rust(string: *mut AnyObject) -> Option<String> {
    if string.is_null() {
        return None;
    }
    let utf8: *const std::os::raw::c_char = msg_send![string, UTF8String];
    if utf8.is_null() {
        return None;
    }
    Some(std::ffi::CStr::from_ptr(utf8).to_string_lossy().into_owned())
}

/// Services target "New Unterm Tab Here": the paths join the same queue the
/// deep link feeds, so the tab opens in this window on the next tick.
extern "C" fn service_tab_here(
    _this: *mut AnyObject,
    _cmd: Sel,
    pboard: *mut AnyObject,
    _user_data: *mut AnyObject,
    _error: *mut *mut AnyObject,
) {
    let paths = unsafe { pasteboard_paths(pboard) };
    trace(&format!("service tab-here with {paths:?}"));
    if paths.is_empty() {
        return;
    }
    PENDING.lock().unwrap().extend(paths);
    // The same as a deep link: delivering a folder is asking for a window to
    // put its tab in.
    crate::tray::request_wake();
}

/// Services target "New Unterm Window Here": each path gets a window of its
/// own, the same way the in-app New Window command makes one.
extern "C" fn service_window_here(
    _this: *mut AnyObject,
    _cmd: Sel,
    pboard: *mut AnyObject,
    _user_data: *mut AnyObject,
    _error: *mut *mut AnyObject,
) {
    let paths = unsafe { pasteboard_paths(pboard) };
    trace(&format!("service window-here with {paths:?}"));
    let Ok(program) = std::env::current_exe() else {
        return;
    };
    for path in paths {
        let _ = std::process::Command::new(&program)
            .arg("start")
            .arg("--cwd")
            .arg(&path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
}

/// Teach the running application delegate to take openURLs. Call once, on
/// the main thread, after the event loop (and so the delegate) exists.
pub fn install() {
    // SAFETY: NSApp and its delegate exist once the event loop is built; we
    // add one method to the delegate's class, which is winit's own private
    // delegate class, not a shared framework one.
    unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let delegate: *mut AnyObject = msg_send![app, delegate];
        if delegate.is_null() {
            log::warn!("no application delegate to teach openURLs to");
            return;
        }
        let class: *const AnyClass = msg_send![delegate, class];
        let class = &*class;
        let sel = sel!(application:openURLs:);
        let types = std::ffi::CString::new("v@:@@").unwrap();
        let added = objc2::ffi::class_addMethod(
            class as *const _ as *mut _,
            sel,
            std::mem::transmute::<
                extern "C" fn(*mut AnyObject, Sel, *mut AnyObject, *mut AnyObject),
                unsafe extern "C-unwind" fn(),
            >(open_urls),
            types.as_ptr(),
        );
        if !added.as_bool() {
            log::warn!("could not add openURLs handler (already present?)");
        }
        // A window parked in the tray leaves the app running with no window
        // and, under the Accessory activation policy, no Dock tile either.
        // Clicking Unterm in Finder or Spotlight then reaches an app macOS
        // considers already open: it sends this instead of launching
        // anything, and without a handler the click does nothing at all.
        let reopen_types = std::ffi::CString::new("B@:@B").unwrap();
        let reopened = objc2::ffi::class_addMethod(
            class as *const _ as *mut _,
            sel!(applicationShouldHandleReopen:hasVisibleWindows:),
            std::mem::transmute::<
                extern "C" fn(*mut AnyObject, Sel, *mut AnyObject, bool) -> bool,
                unsafe extern "C-unwind" fn(),
            >(should_handle_reopen),
            reopen_types.as_ptr(),
        );
        if !reopened.as_bool() {
            log::warn!("could not add reopen handler (already present?)");
        }
        // The Info.plist has promised "New Unterm Tab Here" / "New Unterm
        // Window Here" in the Services menu since v0.40; nothing ever
        // registered a provider, so both were dead buttons in every
        // right-click menu. The delegate grows the two selectors the plist
        // names, and becomes that provider.
        let service_types = std::ffi::CString::new("v@:@@^@").unwrap();
        for (sel, imp) in [
            (
                sel!(openUntermTabHere:userData:error:),
                service_tab_here
                    as extern "C" fn(
                        *mut AnyObject,
                        Sel,
                        *mut AnyObject,
                        *mut AnyObject,
                        *mut *mut AnyObject,
                    ),
            ),
            (sel!(openInUnterm:userData:error:), service_window_here as _),
        ] {
            let _ = objc2::ffi::class_addMethod(
                class as *const _ as *mut _,
                sel,
                std::mem::transmute::<
                    extern "C" fn(
                        *mut AnyObject,
                        Sel,
                        *mut AnyObject,
                        *mut AnyObject,
                        *mut *mut AnyObject,
                    ),
                    unsafe extern "C-unwind" fn(),
                >(imp),
                service_types.as_ptr(),
            );
        }
        let () = msg_send![app, setServicesProvider: delegate];
        // AppKit decided what this delegate can answer when the delegate was
        // set -- before our method existed. A launch delivery still finds it,
        // a delivery to the running app does not. Setting the delegate again
        // makes AppKit look again.
        let () = msg_send![app, setDelegate: std::ptr::null_mut::<AnyObject>()];
        let () = msg_send![app, setDelegate: delegate];
    }
}

#[cfg(test)]
mod tests {
    use super::{percent_decode, scheme_path};

    #[test]
    fn deep_link_yields_its_path() {
        assert_eq!(
            scheme_path("unterm://open?path=/Users/alexlee").as_deref(),
            Some("/Users/alexlee")
        );
        assert_eq!(
            scheme_path("unterm://open?path=%2FUsers%2Falex%20lee%2F%E6%A1%8C%E9%9D%A2").as_deref(),
            Some("/Users/alex lee/桌面")
        );
        assert_eq!(
            scheme_path("unterm://open?other=1&path=%2Ftmp").as_deref(),
            Some("/tmp")
        );
    }

    #[test]
    fn junk_is_refused_not_misread() {
        assert_eq!(scheme_path("https://example.com?path=/tmp"), None);
        assert_eq!(scheme_path("unterm://open"), None);
        assert_eq!(scheme_path("unterm://open?path="), None);
        // A malformed escape decays to its literal bytes instead of panicking.
        assert_eq!(percent_decode("%zz%2F"), "%zz/");
    }
}

/// One line into `<state>/open.log`, so a wrong-folder report comes with
/// the folder that was actually delivered.
///
/// Capped: a debugging probe once left a per-frame trace in a draw path
/// and the file quietly grew to 8 GB. Ten megabytes holds weeks of the
/// event-rate lines this is for; past that, the old log gives way rather
/// than the disk.
pub fn trace(message: &str) {
    let Some(dir) = unterm_protocol::state_dir() else {
        return;
    };
    use std::io::Write as _;
    let path = dir.join("open.log");
    if std::fs::metadata(&path).map_or(false, |meta| meta.len() > 10 * 1024 * 1024) {
        let _ = std::fs::remove_file(&path);
    }
    let stamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{stamp} {message}");
    }
}
