//! The administrator window.
//!
//! An elevated shell cannot come from the Core: it runs as the user, not as
//! an administrator, and anything it starts inherits that. So an elevated
//! window is a second Unterm process, started through UAC, holding its
//! sessions itself -- the arrangement Windows Terminal uses, where an elevated
//! window and a normal one never share tabs.
//!
//! It also shares nothing else. A Core, an MCP server or a settings page
//! reachable from the unelevated side would let any program running as the
//! user type into an administrator shell -- a UAC bypass with extra steps.
//! The elevated process therefore keeps its sessions in-process, serves no
//! agent surface, registers no instance and never hands its window to the
//! normal process. It reads the user's config and theme, and writes its own
//! state somewhere else.

/// Whether this process runs with an administrator token.
pub fn is_elevated() -> bool {
    static ANSWER: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ANSWER.get_or_init(elevated)
}

#[cfg(windows)]
fn elevated() -> bool {
    use winapi::um::handleapi::CloseHandle;
    use winapi::um::processthreadsapi::{GetCurrentProcess, OpenProcessToken};
    use winapi::um::securitybaseapi::GetTokenInformation;
    use winapi::um::winnt::{TokenElevation, HANDLE, TOKEN_ELEVATION, TOKEN_QUERY};
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut returned = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            &mut elevation as *mut _ as *mut std::ffi::c_void,
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        );
        CloseHandle(token);
        ok != 0 && elevation.TokenIsElevated != 0
    }
}

#[cfg(not(windows))]
fn elevated() -> bool {
    false
}

/// Whether this platform can open an administrator window at all.
pub fn available() -> bool {
    cfg!(windows)
}

/// What became of a request to open one.
#[derive(Debug)]
pub enum Launch {
    /// UAC accepted; the new window is on its way.
    Started,
    /// The user said no at the UAC prompt.
    Declined,
}

/// Ask Windows, through UAC, for an elevated Unterm in `directory`.
#[cfg(windows)]
pub fn launch(directory: Option<&std::path::Path>) -> anyhow::Result<Launch> {
    use std::os::windows::ffi::OsStrExt;
    use winapi::um::shellapi::{ShellExecuteExW, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW};
    use winapi::um::winuser::SW_SHOWNORMAL;

    let wide = |text: &std::ffi::OsStr| -> Vec<u16> { text.encode_wide().chain(Some(0)).collect() };
    let program = std::env::current_exe()?;
    let mut parameters = std::ffi::OsString::from("--admin");
    if let Some(directory) = directory {
        parameters.push(" --cwd \"");
        parameters.push(directory.as_os_str());
        parameters.push("\"");
    }
    let verb = wide(std::ffi::OsStr::new("runas"));
    let file = wide(program.as_os_str());
    let parameters = wide(&parameters);
    let working = directory.map(|directory| wide(directory.as_os_str()));
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOASYNC;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = parameters.as_ptr();
    info.lpDirectory = working.as_ref().map_or(std::ptr::null(), |w| w.as_ptr());
    info.nShow = SW_SHOWNORMAL;
    if unsafe { ShellExecuteExW(&mut info) } != 0 {
        return Ok(Launch::Started);
    }
    let error = std::io::Error::last_os_error();
    // ERROR_CANCELLED: the UAC prompt was answered "No".
    if error.raw_os_error() == Some(1223) {
        return Ok(Launch::Declined);
    }
    Err(anyhow::anyhow!("could not start an elevated Unterm: {error}"))
}

#[cfg(not(windows))]
pub fn launch(_directory: Option<&std::path::Path>) -> anyhow::Result<Launch> {
    anyhow::bail!("an administrator window is a Windows feature")
}

/// Set this process up as the administrator window, before anything reads
/// the environment. Returns the config file to load: the user's own.
///
/// Called only for `--admin`, and only honoured when the token really is
/// elevated -- a normal process that was merely passed the flag gets a normal
/// window, not one that believes it is an administrator.
pub fn prepare(requested_config: Option<std::path::PathBuf>) -> Option<std::path::PathBuf> {
    // The user's config and theme, read from where they live...
    let config = requested_config.or_else(|| unterm_protocol::state_path("unterm.conf"));
    let theme = unterm_protocol::state_path("theme.json");
    // ...and this process's own state somewhere else, so nothing it records
    // is found by -- or finds -- the normal Unterm.
    if let Some(home) = unterm_protocol::state_dir() {
        let own = home.join("admin");
        let _ = std::fs::create_dir_all(&own);
        if let Some(theme) = theme.filter(|theme| theme.exists()) {
            let _ = std::fs::copy(theme, own.join("theme.json"));
        }
        std::env::set_var("UNTERM_STATE_DIR", &own);
    }
    // Sessions in this process: no Core for the unelevated side to reach.
    std::env::set_var("UNTERM_CORE_CLIENT", "0");
    std::env::set_var("UNTERM_ADMIN_WINDOW", "1");
    config
}

/// Whether this process is the administrator window.
pub fn is_admin_window() -> bool {
    std::env::var_os("UNTERM_ADMIN_WINDOW").is_some() && is_elevated()
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_normal_test_process_is_not_the_administrator_window() {
        assert!(!super::is_admin_window());
    }

    #[test]
    fn off_windows_there_is_nothing_to_launch() {
        if !super::available() {
            assert!(super::launch(None).is_err());
            assert!(!super::is_elevated());
        }
    }
}
