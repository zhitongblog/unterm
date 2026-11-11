//! Built-in HTTP/1.1 server that exposes Unterm's settings to a browser SPA.
//!
//! Same data path as MCP — the routes call directly into
//! `unterm_mcp::handler::McpHandler` and the recording / theme primitives,
//! the SPA is just a different transport. No external runtime, no extra
//! workspace deps; ~/.unterm/server.json carries the bound port and the
//! shared auth token.

mod agent_run;
mod console_tasks;
mod local_agents;

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
