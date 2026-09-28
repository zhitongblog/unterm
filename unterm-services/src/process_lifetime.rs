//! Keeping Unterm's processes from outliving the reason they exist.
//!
//! Three pieces, all small, all about the same failure: a quit that leaves
//! something behind.
//!
//! - [`SessionJob`]: the Core puts itself in a kill-on-close Job Object, so
//!   whatever it spawns -- the conpty host (`OpenConsole.exe`), the shell and
//!   the shell's own children -- dies with it, however it dies. Force-killing
//!   the Core used to leave every shell running with nobody to show it.
//! - [`ProcessWatch`]: a held handle on another process, so "is the window
//!   that started me still there?" cannot be answered wrongly by a reused pid.
//! - [`listen_for_quit`] / [`signal_quit`]: a per-process named event
//!   (`Local\UntermQuit-<pid>`) that `unterm-cli quit` sets to ask a window --
//!   including one parked in the tray, and the elevated administrator window
//!   -- to quit the ordinary way.
//!
//! Off Windows the job is a no-op, the watch falls back to `kill(pid, 0)`,
//! and quitting is asked with SIGTERM.

/// Which Unterm executable a process is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UntermProcessKind {
    /// `unterm.exe`: a window, a tray-resident front end, or the elevated
    /// administrator window.
    Gui,
    /// `unterm-core.exe`: the process that owns the sessions.
    Core,
}

/// An Unterm process found running in this logon session.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct UntermProcess {
    pub pid: u32,
    pub kind: UntermProcessKind,
    /// The executable's full path, when it could be read. Two installs --
    /// a development build next to the installed one -- differ only here.
    pub path: Option<std::path::PathBuf>,
}

/// The full path of a running process's executable.
#[cfg(windows)]
pub fn executable_path(pid: u32) -> Option<std::path::PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use winapi::um::handleapi::CloseHandle;
    use winapi::um::processthreadsapi::OpenProcess;
    use winapi::um::winbase::QueryFullProcessImageNameW;
    use winapi::um::winnt::PROCESS_QUERY_LIMITED_INFORMATION;
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut buffer = vec![0u16; 1024];
        let mut len = buffer.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut len) != 0;
        CloseHandle(handle);
        ok.then(|| std::ffi::OsString::from_wide(&buffer[..len as usize]).into())
    }
}

#[cfg(not(windows))]
pub fn executable_path(pid: u32) -> Option<std::path::PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/exe")).ok()
}

/// Classify an executable file name. Case-insensitive, with or without the
/// `.exe` suffix, because that is how the two platforms spell it.
pub fn classify_executable(name: &str) -> Option<UntermProcessKind> {
    let lowered = name.to_ascii_lowercase();
    let stem = lowered.strip_suffix(".exe").unwrap_or(&lowered);
    match stem {
        "unterm" => Some(UntermProcessKind::Gui),
        "unterm-core" => Some(UntermProcessKind::Core),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Job Object
// ---------------------------------------------------------------------------

/// A kill-on-close Job Object holding the current process.
///
/// Why the Core joins it itself rather than assigning each session's
/// processes: `OpenConsole.exe` is started from inside `CreatePseudoConsole`,
/// which hands back no process handle to assign. Membership is inherited, so
/// with the Core inside, the conpty host, the shell and everything the shell
/// starts land in the job. The one exception -- a shell Windows starts in a
/// job of its own -- is caught per process by [`SessionJob::contain`], which
/// the Core hooks into the pty layer's spawn.
///
/// Children may still leave on purpose (`JOB_OBJECT_LIMIT_BREAKAWAY_OK` is
/// set, so `CREATE_BREAKAWAY_FROM_JOB` works); they are not pushed out
/// silently.
///
/// The handle is never inheritable and is held only here, so the job closes
/// exactly when this process ends -- by `exit`, by a crash, or by Task
/// Manager -- and the kernel then kills every member left.
pub struct SessionJob {
    #[cfg(windows)]
    handle: winapi::um::winnt::HANDLE,
    /// Jobs of their own for session processes that did not inherit this
    /// one -- see [`SessionJob::contain`]. Raw handles, held for the life of
    /// the process like the main one.
    #[cfg(windows)]
    escaped: std::sync::Mutex<Vec<usize>>,
}

// The handle is a kernel object handle; using it from any thread is fine.
unsafe impl Send for SessionJob {}
unsafe impl Sync for SessionJob {}

impl SessionJob {
    /// Create the job and put this process in it.
    ///
    /// Fails when the process is already in a job that refuses nesting (the
    /// kernel supports nested jobs since Windows 8, but a parent job may still
    /// deny assignment). The caller should log and carry on: without the job
    /// an orderly quit still ends every session, only a hard kill does not.
    #[cfg(windows)]
    pub fn adopt_current_process() -> std::io::Result<Self> {
        use std::ptr::null_mut;
        use winapi::um::handleapi::CloseHandle;
        use winapi::um::jobapi2::{AssignProcessToJobObject, CreateJobObjectW};
        use winapi::um::processthreadsapi::GetCurrentProcess;

        unsafe {
            let handle = CreateJobObjectW(null_mut(), null_mut());
            if handle.is_null() {
                return Err(std::io::Error::last_os_error());
            }
            let job = Self {
                handle,
                escaped: std::sync::Mutex::new(Vec::new()),
            };
            if let Err(err) = set_job_limits(handle, true) {
                CloseHandle(handle);
                return Err(err);
            }
            if AssignProcessToJobObject(handle, GetCurrentProcess()) == 0 {
                let err = std::io::Error::last_os_error();
                CloseHandle(handle);
                return Err(err);
            }
            Ok(job)
        }
    }

    #[cfg(not(windows))]
    pub fn adopt_current_process() -> std::io::Result<Self> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "job objects are a Windows feature",
        ))
    }

    /// Make sure a session process just spawned dies with this process.
    ///
    /// Usually a no-op: it inherited the job from us. Not so for a packaged
    /// app -- the Store build of PowerShell is one -- which Windows starts in
    /// a job of its own, with breakaway for its children. Killing the Core
    /// then took down the conpty host and left the shell and everything in it
    /// running, verified with `pwsh` from WindowsApps and a `ping -t` in it.
    /// Such a process gets a kill-on-close job of its own, nested under the
    /// one Windows gave it (a fresh, empty job may nest anywhere), held here.
    #[cfg(windows)]
    pub fn contain(&self, process: std::os::windows::io::RawHandle) {
        use winapi::um::handleapi::CloseHandle;
        use winapi::um::jobapi::IsProcessInJob;
        use winapi::um::jobapi2::{AssignProcessToJobObject, CreateJobObjectW};

        let process = process as winapi::um::winnt::HANDLE;
        unsafe {
            let mut inside = 0;
            if IsProcessInJob(process, self.handle, &mut inside) != 0 && inside != 0 {
                return;
            }
            let job = CreateJobObjectW(std::ptr::null_mut(), std::ptr::null_mut());
            if job.is_null() {
                log::warn!(
                    "could not create a job for an escaped session process: {}",
                    std::io::Error::last_os_error()
                );
                return;
            }
            if let Err(err) = set_job_limits(job, true) {
                log::warn!("could not configure a session process job: {err}");
                CloseHandle(job);
                return;
            }
            if AssignProcessToJobObject(job, process) == 0 {
                log::warn!(
                    "could not contain a session process: {}",
                    std::io::Error::last_os_error()
                );
                CloseHandle(job);
                return;
            }
            if let Ok(mut escaped) = self.escaped.lock() {
                escaped.push(job as usize);
            }
        }
    }

    #[cfg(not(windows))]
    pub fn contain(&self, _process: i32) {}

    /// Stop the job from killing its members when it closes.
    ///
    /// For an orderly exit only, after every session has been ended the
    /// ordinary way. What is still in the job then is what the user started
    /// *from* a shell and meant to keep -- an editor window, a browser -- and
    /// quitting the terminal must not take those with it, any more than it
    /// did before the job existed. The job is for the exits that skip the
    /// orderly part.
    pub fn disarm(&self) {
        #[cfg(windows)]
        {
            if let Err(err) = set_job_limits(self.handle, false) {
                log::warn!("could not relax the session job: {err}");
            }
            if let Ok(escaped) = self.escaped.lock() {
                for &job in escaped.iter() {
                    let _ = set_job_limits(job as winapi::um::winnt::HANDLE, false);
                }
            }
        }
    }
}

#[cfg(windows)]
fn set_job_limits(handle: winapi::um::winnt::HANDLE, kill_on_close: bool) -> std::io::Result<()> {
    {
        use winapi::um::jobapi2::SetInformationJobObject;
        use winapi::um::winnt::{
            JobObjectExtendedLimitInformation, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_BREAKAWAY_OK, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };
        unsafe {
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_BREAKAWAY_OK
                | if kill_on_close {
                    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
                } else {
                    0
                };
            if SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &mut limits as *mut _ as *mut _,
                std::mem::size_of_val(&limits) as u32,
            ) == 0
            {
                return Err(std::io::Error::last_os_error());
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Watching another process
// ---------------------------------------------------------------------------

/// A handle held on another process, to ask later whether it is still alive.
///
/// Held rather than re-opened by pid: a pid is recycled once its process is
/// gone, and "the window that started me is still running" must not come
/// true again because something else got its number.
pub struct ProcessWatch {
    #[cfg(windows)]
    handle: winapi::um::winnt::HANDLE,
    #[cfg(not(windows))]
    pid: u32,
}

unsafe impl Send for ProcessWatch {}
unsafe impl Sync for ProcessWatch {}

impl ProcessWatch {
    /// Start watching `pid`. `None` when it is not running (or cannot be
    /// opened at all, which for our own user's process means the same).
    pub fn open(pid: u32) -> Option<Self> {
        if pid == 0 {
            return None;
        }
        #[cfg(windows)]
        unsafe {
            use winapi::um::processthreadsapi::OpenProcess;
            use winapi::um::winnt::{PROCESS_QUERY_LIMITED_INFORMATION, SYNCHRONIZE};
            let handle = OpenProcess(SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return None;
            }
            let watch = Self { handle };
            watch.is_alive().then_some(watch)
        }
        #[cfg(not(windows))]
        {
            crate::server_info::pid_alive(pid).then_some(Self { pid })
        }
    }

    pub fn is_alive(&self) -> bool {
        #[cfg(windows)]
        unsafe {
            use winapi::um::synchapi::WaitForSingleObject;
            use winapi::um::winbase::WAIT_OBJECT_0;
            WaitForSingleObject(self.handle, 0) != WAIT_OBJECT_0
        }
        #[cfg(not(windows))]
        {
            crate::server_info::pid_alive(self.pid)
        }
    }
}

impl Drop for ProcessWatch {
    fn drop(&mut self) {
        #[cfg(windows)]
        unsafe {
            winapi::um::handleapi::CloseHandle(self.handle);
        }
    }
}

// ---------------------------------------------------------------------------
// Asking a window to quit
// ---------------------------------------------------------------------------

/// The event a process listens on for "please quit".
pub fn quit_event_name(pid: u32) -> String {
    format!("Local\\UntermQuit-{pid}")
}

/// Listen for a quit request addressed to this process, and call `on_quit`
/// once when it arrives. Returns false when the listener could not be set up.
///
/// The event is readable and settable by the current user at medium
/// integrity, deliberately including from the elevated administrator window:
/// `unterm-cli quit` runs unelevated and must be able to reach it. The one
/// thing that access grants is asking the window to quit -- the same thing
/// the user could do by clicking its close button -- and it lives in the
/// `Local\` namespace of this logon session only.
#[cfg(windows)]
pub fn listen_for_quit(on_quit: impl FnOnce() + Send + 'static) -> bool {
    let event = match create_quit_event(std::process::id()) {
        Ok(event) => event,
        Err(err) => {
            log::warn!("could not set up the quit listener: {err}");
            return false;
        }
    };
    let event = event as usize;
    std::thread::Builder::new()
        .name("quit-listener".into())
        .spawn(move || unsafe {
            use winapi::um::synchapi::WaitForSingleObject;
            use winapi::um::winbase::{INFINITE, WAIT_OBJECT_0};
            // The handle is kept open for the life of the process -- it is
            // what keeps the name reachable.
            if WaitForSingleObject(event as winapi::um::winnt::HANDLE, INFINITE) == WAIT_OBJECT_0 {
                on_quit();
            }
        })
        .is_ok()
}

#[cfg(not(windows))]
pub fn listen_for_quit(_on_quit: impl FnOnce() + Send + 'static) -> bool {
    // SIGTERM is the request off Windows; the default action already ends
    // the process.
    false
}

#[cfg(windows)]
fn create_quit_event(pid: u32) -> std::io::Result<winapi::um::winnt::HANDLE> {
    use std::os::windows::ffi::OsStrExt;
    use winapi::shared::sddl::ConvertStringSecurityDescriptorToSecurityDescriptorW;
    use winapi::um::minwinbase::SECURITY_ATTRIBUTES;
    use winapi::um::synchapi::CreateEventW;
    use winapi::um::winbase::LocalFree;

    let wide = |text: &str| -> Vec<u16> {
        std::ffi::OsStr::new(text)
            .encode_wide()
            .chain(Some(0))
            .collect()
    };
    let sid = current_user_sid()?;
    // Full access for SYSTEM and this user; a medium mandatory label with
    // no-write-up, so an unelevated `unterm-cli` may set an event the
    // elevated window created, and nothing below medium may.
    let sddl = wide(&format!("D:P(A;;GA;;;SY)(A;;GA;;;{sid})S:(ML;;NW;;;ME)"));
    unsafe {
        let mut descriptor = std::ptr::null_mut();
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1, // SDDL_REVISION_1
            &mut descriptor,
            std::ptr::null_mut(),
        ) == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        let mut attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        let name = wide(&quit_event_name(pid));
        let event = CreateEventW(&mut attributes, 1, 0, name.as_ptr());
        let err = std::io::Error::last_os_error();
        LocalFree(descriptor);
        if event.is_null() {
            return Err(err);
        }
        Ok(event)
    }
}

/// This process's user SID as a string, e.g. `S-1-5-21-...`.
#[cfg(windows)]
fn current_user_sid() -> std::io::Result<String> {
    use winapi::shared::sddl::ConvertSidToStringSidW;
    use winapi::um::handleapi::CloseHandle;
    use winapi::um::processthreadsapi::{GetCurrentProcess, OpenProcessToken};
    use winapi::um::securitybaseapi::GetTokenInformation;
    use winapi::um::winbase::LocalFree;
    use winapi::um::winnt::{TokenUser, TOKEN_QUERY, TOKEN_USER};

    unsafe {
        let mut token = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut buffer = vec![0u8; 256];
        let mut needed = 0u32;
        let mut ok = GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr() as *mut _,
            buffer.len() as u32,
            &mut needed,
        );
        if ok == 0 && needed as usize > buffer.len() {
            buffer.resize(needed as usize, 0);
            ok = GetTokenInformation(
                token,
                TokenUser,
                buffer.as_mut_ptr() as *mut _,
                buffer.len() as u32,
                &mut needed,
            );
        }
        let err = std::io::Error::last_os_error();
        CloseHandle(token);
        if ok == 0 {
            return Err(err);
        }
        let user = &*(buffer.as_ptr() as *const TOKEN_USER);
        let mut raw: *mut u16 = std::ptr::null_mut();
        if ConvertSidToStringSidW(user.User.Sid, &mut raw) == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let len = (0..).take_while(|&i| *raw.add(i) != 0).count();
        let text = String::from_utf16_lossy(std::slice::from_raw_parts(raw, len));
        LocalFree(raw as *mut _);
        Ok(text)
    }
}

/// Ask process `pid` to quit. `Ok(false)` when it is not listening -- an
/// older build, or not an Unterm window at all.
#[cfg(windows)]
pub fn signal_quit(pid: u32) -> std::io::Result<bool> {
    use std::os::windows::ffi::OsStrExt;
    use winapi::um::handleapi::CloseHandle;
    use winapi::um::synchapi::{OpenEventW, SetEvent};
    use winapi::um::winnt::EVENT_MODIFY_STATE;

    let name: Vec<u16> = std::ffi::OsStr::new(&quit_event_name(pid))
        .encode_wide()
        .chain(Some(0))
        .collect();
    unsafe {
        let event = OpenEventW(EVENT_MODIFY_STATE, 0, name.as_ptr());
        if event.is_null() {
            let err = std::io::Error::last_os_error();
            // ERROR_FILE_NOT_FOUND: nobody by that name is listening.
            if err.raw_os_error() == Some(2) {
                return Ok(false);
            }
            return Err(err);
        }
        let ok = SetEvent(event) != 0;
        let err = std::io::Error::last_os_error();
        CloseHandle(event);
        if ok {
            Ok(true)
        } else {
            Err(err)
        }
    }
}

#[cfg(not(windows))]
pub fn signal_quit(pid: u32) -> std::io::Result<bool> {
    if unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) } == 0 {
        Ok(true)
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// End a process without asking. The last resort of `unterm-cli quit --all`.
pub fn terminate(pid: u32) -> bool {
    #[cfg(windows)]
    unsafe {
        use winapi::um::handleapi::CloseHandle;
        use winapi::um::processthreadsapi::{OpenProcess, TerminateProcess};
        use winapi::um::winnt::PROCESS_TERMINATE;
        let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if handle.is_null() {
            return false;
        }
        let ok = TerminateProcess(handle, 1) != 0;
        CloseHandle(handle);
        ok
    }
    #[cfg(not(windows))]
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGKILL) == 0
    }
}

/// End a process and everything below it, children first found, then the
/// process itself. For a Core too old to hold a session job: terminating it
/// alone would orphan its conpty hosts and shells, which is the very thing
/// being cleaned up. Returns whether the root was terminated.
#[cfg(windows)]
pub fn terminate_tree(pid: u32) -> bool {
    use winapi::um::handleapi::{CloseHandle, INVALID_HANDLE_VALUE};
    use winapi::um::tlhelp32::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    let mut edges = Vec::new();
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot != INVALID_HANDLE_VALUE {
            let mut entry: PROCESSENTRY32W = std::mem::zeroed();
            entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            let mut more = Process32FirstW(snapshot, &mut entry);
            while more != 0 {
                edges.push((entry.th32ParentProcessID, entry.th32ProcessID));
                more = Process32NextW(snapshot, &mut entry);
            }
            CloseHandle(snapshot);
        }
    }
    let descendants = descendants_of(pid, &edges);
    let root = terminate(pid);
    for child in descendants {
        terminate(child);
    }
    root
}

#[cfg(not(windows))]
pub fn terminate_tree(pid: u32) -> bool {
    terminate(pid)
}

/// Every process below `root` in a (parent, child) edge list, breadth first.
/// A pid never appears twice, so a cycle from a recycled parent id cannot
/// loop.
pub fn descendants_of(root: u32, edges: &[(u32, u32)]) -> Vec<u32> {
    let mut found = Vec::new();
    let mut seen = std::collections::HashSet::from([root]);
    let mut frontier = vec![root];
    while let Some(parent) = frontier.pop() {
        for &(from, child) in edges {
            if from == parent && child != 0 && seen.insert(child) {
                found.push(child);
                frontier.push(child);
            }
        }
    }
    found
}

/// Every Unterm window and Core running in this logon session, except the
/// calling process.
#[cfg(windows)]
pub fn running_unterm_processes() -> Vec<UntermProcess> {
    use winapi::um::handleapi::{CloseHandle, INVALID_HANDLE_VALUE};
    use winapi::um::processthreadsapi::ProcessIdToSessionId;
    use winapi::um::tlhelp32::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };

    let own = std::process::id();
    let mut own_session = 0u32;
    let mut found = Vec::new();
    unsafe {
        if ProcessIdToSessionId(own, &mut own_session) == 0 {
            return found;
        }
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return found;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut more = Process32FirstW(snapshot, &mut entry);
        while more != 0 {
            let len = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            let name = String::from_utf16_lossy(&entry.szExeFile[..len]);
            let pid = entry.th32ProcessID;
            if let Some(kind) = classify_executable(&name) {
                let mut session = u32::MAX;
                if pid != own
                    && ProcessIdToSessionId(pid, &mut session) != 0
                    && session == own_session
                {
                    found.push(UntermProcess {
                        pid,
                        kind,
                        path: executable_path(pid),
                    });
                }
            }
            more = Process32NextW(snapshot, &mut entry);
        }
        CloseHandle(snapshot);
    }
    found
}

/// Off Windows: the windows the instance registry knows about. The Core is
/// found through its discovery record by the caller.
#[cfg(not(windows))]
pub fn running_unterm_processes() -> Vec<UntermProcess> {
    crate::server_info::list_live_instances()
        .into_iter()
        .filter(|instance| instance.pid != std::process::id())
        .map(|instance| UntermProcess {
            pid: instance.pid,
            kind: UntermProcessKind::Gui,
            path: executable_path(instance.pid),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn executables_are_classified_by_name_alone() {
        assert_eq!(
            classify_executable("unterm.exe"),
            Some(UntermProcessKind::Gui)
        );
        assert_eq!(
            classify_executable("Unterm.EXE"),
            Some(UntermProcessKind::Gui)
        );
        assert_eq!(classify_executable("unterm"), Some(UntermProcessKind::Gui));
        assert_eq!(
            classify_executable("unterm-core.exe"),
            Some(UntermProcessKind::Core)
        );
        // The CLI is how quitting is asked; it is never a target.
        assert_eq!(classify_executable("unterm-cli.exe"), None);
        assert_eq!(classify_executable("OpenConsole.exe"), None);
    }

    #[test]
    fn descendants_are_found_through_every_generation_and_cycles_end() {
        let edges = [(1, 2), (2, 3), (3, 4), (9, 10), (4, 2), (1, 5)];
        let mut found = descendants_of(1, &edges);
        found.sort();
        assert_eq!(found, vec![2, 3, 4, 5]);
        assert!(descendants_of(10, &edges).is_empty());
    }

    #[test]
    fn a_process_watch_sees_its_own_process_alive() {
        let watch = ProcessWatch::open(std::process::id()).expect("own process");
        assert!(watch.is_alive());
        assert!(ProcessWatch::open(0).is_none());
    }

    #[test]
    fn a_process_watch_sees_a_child_exit() {
        let mut child = if cfg!(windows) {
            std::process::Command::new("cmd")
                .args(["/C", "exit 0"])
                .spawn()
        } else {
            std::process::Command::new("true").spawn()
        }
        .expect("spawn a short-lived child");
        let watch = ProcessWatch::open(child.id());
        child.wait().unwrap();
        // It may have exited before we opened it; either way it is not alive.
        assert!(watch.map(|watch| !watch.is_alive()).unwrap_or(true));
    }

    #[cfg(windows)]
    #[test]
    fn a_quit_request_reaches_the_listener_by_pid() {
        // Nobody listens under a pid that is not running.
        assert!(!signal_quit(u32::MAX - 3).unwrap());
        let (tx, rx) = std::sync::mpsc::channel();
        assert!(listen_for_quit(move || {
            let _ = tx.send(());
        }));
        assert!(signal_quit(std::process::id()).unwrap());
        rx.recv_timeout(std::time::Duration::from_secs(5))
            .expect("the listener heard the request");
    }
}
