//! Unterm's product services, independent of any front end.
//!
//! These were inside the GUI crate for no reason other than history: the
//! instance registry, the agent cockpit, proxy control, command suggestion.
//! None of them draw anything or know what a window is.

pub mod agent_policy;
pub mod agent_session;
pub mod artifacts;
pub mod audit_store;
pub mod brain_tools;
pub mod bridge_registry;
pub mod clash_api;
pub mod cockpit;
pub mod diagnostics;
pub mod env_names;
pub mod evidence;
pub mod gateway;
pub mod hosted_grant;
pub mod ghost_text;
pub mod i18n;
pub mod install;
pub mod interrupt;
pub mod launch_env;
pub mod path_scope;
pub mod process_env;
pub mod power;
pub mod providers;
pub mod workspace_scope;
pub mod peer_mcp;
pub mod recording;
pub mod routing;
pub mod scrollback_options;
pub mod search_regex;
pub mod server_info;

/// The desktop work area: the screen minus the taskbar, in physical pixels
/// as (left, top, width, height). A borderless window maximised by the OS
/// hangs eight pixels off every edge; sizing to this instead does not.
pub fn work_area() -> Option<(i32, i32, u32, u32)> {
    #[cfg(windows)]
    // SAFETY: a POD rect filled by the system call.
    unsafe {
        let mut rect: winapi::shared::windef::RECT = std::mem::zeroed();
        let ok = winapi::um::winuser::SystemParametersInfoW(
            winapi::um::winuser::SPI_GETWORKAREA,
            0,
            &mut rect as *mut _ as *mut _,
            0,
        );
        if ok == 0 {
            return None;
        }
        let width = (rect.right - rect.left).max(1);
        let mut height = (rect.bottom - rect.top).max(1);
        let screen_w = winapi::um::winuser::GetSystemMetrics(winapi::um::winuser::SM_CXSCREEN);
        let screen_h = winapi::um::winuser::GetSystemMetrics(winapi::um::winuser::SM_CYSCREEN);
        if rect.left == 0 && rect.top == 0 && width == screen_w && height == screen_h {
            height = (height - 1).max(1);
        }
        return Some((
            rect.left,
            rect.top,
            width as u32,
            height as u32,
        ));
    }
    #[allow(unreachable_code)]
    None
}

/// The platform's alert sound, for the terminal bell.
pub fn system_beep() {
    #[cfg(windows)]
    // SAFETY: no arguments beyond the sound selector; fire and forget.
    unsafe {
        winapi::um::winuser::MessageBeep(0xFFFF_FFFF);
    }
}
pub mod settings;
pub mod terminal_provider;
pub mod tool_risk;
pub mod theme_state;
pub mod toast;
pub mod wake_watch;
pub mod window_capture;

/// What this build calls itself, stamped into recordings and reported to
/// agents. Read from the crate rather than from a config, which is where it
/// used to live for no reason other than history.
pub const VERSION: &str = unterm_protocol::PRODUCT_VERSION;
pub mod process_lifetime;
pub mod updater;
pub mod process_stats;
pub mod supervisor;
pub mod upgrade;
pub mod system_proxy;

#[cfg(test)]
pub(crate) mod test_env {
    use std::ffi::OsString;
    use std::ops::Deref;
    use std::sync::{Mutex, MutexGuard};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    const TRACKED_VARS: &[&str] = &[
        "UNTERM_STATE_DIR",
        "UNTERM_TASKS_DB",
        "UNTERM_SCOPE_DRIFT",
        "UNTERM_FLEETS_PATH",
        "WEZTERM_STATE_DIR",
    ];

    pub(crate) fn lock() -> MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) struct IsolatedEnv {
        _guard: MutexGuard<'static, ()>,
        dir: tempfile::TempDir,
        saved: Vec<(&'static str, Option<OsString>)>,
    }

    impl IsolatedEnv {
        pub(crate) fn new() -> Self {
            let guard = lock();
            let saved = TRACKED_VARS
                .iter()
                .map(|&k| (k, std::env::var_os(k)))
                .collect();
            let dir = tempfile::tempdir().expect("create isolated test tempdir");
            std::env::set_var("UNTERM_STATE_DIR", dir.path());
            std::env::set_var("UNTERM_TASKS_DB", dir.path().join("tasks.db"));
            std::env::remove_var("UNTERM_SCOPE_DRIFT");
            std::env::remove_var("UNTERM_FLEETS_PATH");
            crate::cockpit::fleet_store::reset_for_tests();
            crate::providers::reset_for_tests();
            Self {
                _guard: guard,
                dir,
                saved,
            }
        }
    }

    impl Deref for IsolatedEnv {
        type Target = tempfile::TempDir;

        fn deref(&self) -> &Self::Target {
            &self.dir
        }
    }

    impl Drop for IsolatedEnv {
        fn drop(&mut self) {
            for (k, v) in self.saved.drain(..) {
                match v {
                    Some(val) => std::env::set_var(k, val),
                    None => std::env::remove_var(k),
                }
            }
            crate::cockpit::fleet_store::reset_for_tests();
            crate::providers::reset_for_tests();
        }
    }
}
