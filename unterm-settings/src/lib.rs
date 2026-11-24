//! Built-in HTTP/1.1 server that exposes Unterm's settings to a browser SPA.
//!
//! Same data path as MCP — the routes call directly into
//! `unterm_mcp::handler::McpHandler` and the recording / theme primitives,
//! the SPA is just a different transport. No external runtime, no extra
//! workspace deps; ~/.unterm/server.json carries the bound port and the
//! shared auth token.

mod agent_run;
mod browser;
pub mod clients;
mod console_tasks;
pub use console_tasks::{run_settings, set_run_settings, RunSettings};
pub mod helpers;
mod local_agents;
mod skills;

/// Whether the console has work that needs this process to stay: a task run
/// in progress, or a scheduled task that is not paused. A Core started by a
/// window outlives its windows while this holds.
pub fn console_has_work() -> bool {
    console_tasks::has_work().unwrap_or(false)
}
mod agents;
mod assets;
mod console;
pub use console::console_dir;
pub mod harness;
pub mod qrcode_svg;
pub mod remote;
pub mod server;

pub use server::start_web_settings_server;
pub mod update_check;

#[cfg(test)]
pub(crate) mod test_env {
    use std::ffi::OsString;
    use std::sync::{Mutex, MutexGuard};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    const TRACKED_VARS: &[&str] = &[
        "UNTERM_STATE_DIR",
        "UNTERM_TASKS_DB",
        "UNTERM_CLI",
        "UNZOO_SERVICE",
        "UNTERM_CONSOLE_DIR",
        "UNTERM_WORK_ROOT",
        "UNTERM_CLIENTS_HOME",
        "UNTERM_PRODUCT_NAME",
        "UNTERM_BUILTIN_SKILLS",
    ];

    pub(crate) fn lock() -> MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) struct EnvGuard {
        _guard: MutexGuard<'static, ()>,
        saved: Vec<(&'static str, Option<OsString>)>,
    }

    impl EnvGuard {
        pub(crate) fn new() -> Self {
            let guard = lock();
            let saved = TRACKED_VARS
                .iter()
                .map(|&k| (k, std::env::var_os(k)))
                .collect();
            Self {
                _guard: guard,
                saved,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (k, v) in self.saved.drain(..) {
                match v {
                    Some(val) => std::env::set_var(k, val),
                    None => std::env::remove_var(k),
                }
            }
        }
    }
}
