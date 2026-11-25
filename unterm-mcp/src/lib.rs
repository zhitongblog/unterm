//! Unterm MCP Server — bridges JSON-RPC requests to WezTerm's Mux API.
//!
//! Runs a TCP server on 127.0.0.1:19876 that exposes terminal session
//! management via the MCP protocol. AI agents and unterm-cli can connect
//! to read screen content, send input, and manage sessions.

pub mod approvals;
pub mod providers;
pub mod records;
pub mod handler;
pub mod meta;
mod server;
pub mod upload;
#[cfg(windows)]
mod win_capture;

pub use server::{start_headless_mcp_server, start_mcp_server, start_mcp_server_with_version};
