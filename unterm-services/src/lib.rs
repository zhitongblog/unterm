//! Unterm's product services, independent of any front end.
//!
//! These were inside the GUI crate for no reason other than history: the
//! instance registry, the agent cockpit, proxy control, command suggestion.
//! None of them draw anything or know what a window is.

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
        return Some((
            rect.left,
            rect.top,
            (rect.right - rect.left).max(1) as u32,
            (rect.bottom - rect.top).max(1) as u32,
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
