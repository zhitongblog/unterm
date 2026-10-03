//! `meta.surface` — single-call inventory of everything an agent can drive
//! Unterm with: MCP methods, CLI subcommands, and current keybindings.
//!
//! Designed for two consumers:
//!   1. AI agents that just connected and want one round-trip discovery
//!      of "what can I do here?" without scraping --help output.
//!   2. The Reference tab in Web Settings — the same JSON renders into a
//!      searchable in-app cheatsheet, so users discover the surface area
//!      without having to read external docs.
//!
//! The MCP method list is the source of truth: dispatch in `handler.rs`
//! reads from `MCP_METHODS` so adding a new method in one place is
//! enough. The CLI list is currently a separate maintained array because
//! the GUI binary can't directly introspect the CLI binary's clap tree;
//! discipline (and the matching `unterm-cli reference` self-test) keeps
//! it honest. Keybindings are read live from the current `InputMap`,
//! so they never drift.
//!
//! Keep `summary` strings short and imperative — they show up in the
//! Reference UI as a one-line description next to each entry.
use anyhow::Result;
use serde_json::{json, Value};

pub use unterm_agents::mcp_meta::{CLI_COMMANDS, MCP_METHODS};

const WEZTERM_UNSUPPORTED_METHODS: &[&str] = &["session.env", "session.set_env"];
const NEXT_CORE_UNSUPPORTED_METHODS: &[&str] = &[];
const MCP_TOOL_EXCLUSIONS: &[&str] = &["meta.surface"];

fn engine_unsupported_methods(engine: &str) -> Vec<&'static str> {
    let mut methods = if engine == "next-core" {
        NEXT_CORE_UNSUPPORTED_METHODS.to_vec()
    } else {
        WEZTERM_UNSUPPORTED_METHODS.to_vec()
    };
    methods.sort_unstable();
    methods.dedup();
    methods
}

pub fn engine_capabilities(engine: &str) -> Value {
    let unsupported_methods = engine_unsupported_methods(engine);
    let supported_methods: Vec<_> = MCP_METHODS
        .iter()
        .map(|method| method.name)
        .filter(|name| !unsupported_methods.contains(name))
        .collect();
    let engine_limited_methods = if engine == "next-core" {
        vec![
            json!({
                "name": "screen.search",
                "limitation": "goto updates next-core's logical viewport; real GUI viewport integration comes with the next-core renderer",
            }),
            json!({
                "name": "screen.scroll",
                "limitation": "goto/apply updates next-core's logical viewport; real GUI viewport integration comes with the next-core renderer",
            }),
            json!({
                "name": "cockpit.inbox",
                "limitation": "pane location metadata is synthetic until next-core owns real GUI tabs/windows",
            }),
            json!({
                "name": "capture.scrollback",
                "limitation": "renders styled cell PNGs from next-core scrollback through a standalone headless renderer and reports renderer parity metadata; real GUI renderer integration comes with the next-core renderer",
            }),
            json!({
                "name": "instance.focus",
                "limitation": "focuses the current host GUI window; next-core does not own native window lifecycle yet",
            }),
            json!({
                "name": "instance.set_title",
                "limitation": "persists server_info title metadata; native window title ownership stays with the host GUI until next-core owns windows",
            }),
            json!({
                "name": "instance.lifecycle",
                "limitation": "reports server_info registration and shutdown dry-run ownership; native window close remains host-owned until next-core owns windows",
            }),
            json!({
                "name": "instance.close",
                "limitation": "can explicitly unregister the current server_info entry; native window close remains host-owned until next-core owns windows",
            }),
        ]
    } else {
        Vec::new()
    };
    let health_metrics = if engine == "next-core" {
        vec![
            "input_writes",
            "input_bytes",
            "output_chunks",
            "output_bytes",
            "paste_count",
            "paste_text_bytes",
        ]
    } else {
        Vec::new()
    };
    let runtime_pump_metrics = if engine == "next-core" {
        vec![
            "drain_calls",
            "dispatched_commands",
            "dispatched_lifecycle_commands",
            "dispatched_input_commands",
            "dispatched_render_commands",
            "dispatched_screen_commands",
            "dispatched_background_commands",
            "waited_for_response",
            "completed_without_wait",
            "total_dispatch_elapsed_micros",
            "max_dispatch_elapsed_micros",
            "total_drain_elapsed_micros",
            "max_drain_elapsed_micros",
        ]
    } else {
        Vec::new()
    };

    json!({
        "engine": engine,
        "supported_methods": supported_methods,
        "unsupported_methods": unsupported_methods,
        "engine_limited_methods": engine_limited_methods,
        "diagnostics": {
            "health_io_summary": engine == "next-core",
            "runtime_pump_summary": engine == "next-core",
            "launch_context": engine == "next-core",
            "launch_policy_decisions": engine == "next-core",
            "default_shell_launch_decision": true,
            "session_create_launch_decision": true,
            "workspace_restore_launch_plan": true,
            // Rendering needs a front end's font stack, so the answer is
            // whether one is hosting us, not which engine is selected.
            "styled_scrollback_png": engine == "next-core"
                && unterm_engine::mcp_host().is_some(),
            "styled_scrollback_renderer_metadata": true,
            "pty_write_confirmation": true,
            "recording_block_markdown": engine == "next-core",
            "recording_osc133_command_blocks": engine == "next-core",
            "validated_capture_scrollback_pane_ids": true,
            "host_window_bridge": true,
            "instance_title_bridge": true,
            "instance_lifecycle_observability": true,
            "instance_registry_diagnostics": true,
            "instance_shutdown_dry_run": true,
            "instance_registry_unregister": true,
            "native_window_lifecycle": false,
            "health_metrics": health_metrics,
            "runtime_pump_metrics": runtime_pump_metrics,
        },
    })
}

/// Read the effective keybindings from the current config and return them
/// as serializable rows, as the hosting front end reports them.
pub fn keybindings_inventory() -> Vec<Value> {
    // The key table belongs to whichever front end is hosting us: it decides
    // what its keys do, and a headless surface has none.
    unterm_engine::mcp_host()
        .map(|host| host.key_assignments())
        .unwrap_or_default()
}

/// `meta.surface` MCP handler.
pub fn surface(_params: &Value) -> Result<Value> {
    let engine = unterm_engine::engine_provider()
        .map(|provider| provider().name())
        .unwrap_or("next-core");
    let tool_count = MCP_METHODS
        .iter()
        .filter(|method| !MCP_TOOL_EXCLUSIONS.contains(&method.name))
        .count();
    Ok(json!({
        "version": unterm_protocol::PRODUCT_VERSION,
        "engine": engine,
        "engine_capabilities": engine_capabilities(engine),
        "mcp_method_count": MCP_METHODS.len(),
        "mcp_tool_count": tool_count,
        "mcp_tool_exclusions": MCP_TOOL_EXCLUSIONS,
        "mcp_methods": MCP_METHODS,
        "cli_commands": CLI_COMMANDS,
        "keybindings": keybindings_inventory(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings_at<'a>(value: &'a Value, key: &str) -> Vec<&'a str> {
        value[key]
            .as_array()
            .expect("expected array")
            .iter()
            .map(|v| v.as_str().expect("expected string"))
            .collect()
    }

    #[test]
    fn wezterm_capabilities_mark_only_explicit_stubs_unsupported() {
        let caps = engine_capabilities("wezterm");
        let unsupported = strings_at(&caps, "unsupported_methods");

        assert!(unsupported.contains(&"session.env"));
        assert!(unsupported.contains(&"session.set_env"));
        assert!(!unsupported.contains(&"capture.scrollback"));

        let supported = strings_at(&caps, "supported_methods");
        assert!(supported.contains(&"session.input"));
        assert!(supported.contains(&"capture.scrollback"));
        assert_eq!(caps["diagnostics"]["health_io_summary"], false);
        assert_eq!(caps["diagnostics"]["runtime_pump_summary"], false);
        assert_eq!(caps["diagnostics"]["launch_context"], false);
        assert_eq!(caps["diagnostics"]["launch_policy_decisions"], false);
        assert_eq!(caps["diagnostics"]["default_shell_launch_decision"], true);
        assert_eq!(caps["diagnostics"]["session_create_launch_decision"], true);
        assert_eq!(caps["diagnostics"]["workspace_restore_launch_plan"], true);
        assert_eq!(caps["diagnostics"]["pty_write_confirmation"], true);
        assert_eq!(caps["diagnostics"]["recording_block_markdown"], false);
        assert_eq!(
            caps["diagnostics"]["recording_osc133_command_blocks"],
            false
        );
        assert_eq!(
            caps["diagnostics"]["validated_capture_scrollback_pane_ids"],
            true
        );
        assert_eq!(
            caps["diagnostics"]["styled_scrollback_renderer_metadata"],
            true
        );
        assert_eq!(caps["diagnostics"]["host_window_bridge"], true);
        assert_eq!(caps["diagnostics"]["instance_title_bridge"], true);
        assert_eq!(
            caps["diagnostics"]["instance_lifecycle_observability"],
            true
        );
        assert_eq!(caps["diagnostics"]["instance_registry_diagnostics"], true);
        assert_eq!(caps["diagnostics"]["instance_shutdown_dry_run"], true);
        assert_eq!(caps["diagnostics"]["instance_registry_unregister"], true);
        assert_eq!(caps["diagnostics"]["native_window_lifecycle"], false);
        assert!(strings_at(&caps["diagnostics"], "runtime_pump_metrics").is_empty());
    }

    #[test]
    fn next_core_capabilities_expose_styled_scrollback_png_renderer() {
        let caps = engine_capabilities("next-core");
        let unsupported = strings_at(&caps, "unsupported_methods");

        assert!(!unsupported.contains(&"session.set_env"));
        assert!(!unsupported.contains(&"session.env"));
        assert!(!unsupported.contains(&"capture.scrollback"));

        let supported = strings_at(&caps, "supported_methods");
        assert!(supported.contains(&"session.input"));
        assert!(supported.contains(&"session.env"));
        assert!(supported.contains(&"session.set_env"));
        assert!(supported.contains(&"screen.text"));
        assert!(supported.contains(&"capture.scrollback"));
        assert!(supported.contains(&"cockpit.inbox"));
        assert!(supported.contains(&"orchestrate.launch"));
        assert!(supported.contains(&"orchestrate.broadcast"));
        assert!(supported.contains(&"orchestrate.wait"));
        assert!(supported.contains(&"review.diff"));
        assert!(supported.contains(&"review.verify"));
        assert!(supported.contains(&"review.merge"));

        let limited = caps["engine_limited_methods"]
            .as_array()
            .expect("expected limited method array");
        assert!(limited
            .iter()
            .any(|item| item["name"].as_str() == Some("screen.search")));
        let search = limited
            .iter()
            .find(|item| item["name"].as_str() == Some("screen.search"))
            .expect("screen search limitation");
        assert!(search["limitation"]
            .as_str()
            .expect("limitation text")
            .contains("logical viewport"));
        let scroll = limited
            .iter()
            .find(|item| item["name"].as_str() == Some("screen.scroll"))
            .expect("screen scroll limitation");
        assert!(scroll["limitation"]
            .as_str()
            .expect("limitation text")
            .contains("goto/apply"));
        let inbox = limited
            .iter()
            .find(|item| item["name"].as_str() == Some("cockpit.inbox"))
            .expect("cockpit inbox limitation");
        assert!(inbox["limitation"]
            .as_str()
            .expect("limitation text")
            .contains("synthetic"));
        let capture = limited
            .iter()
            .find(|item| item["name"].as_str() == Some("capture.scrollback"))
            .expect("capture scrollback limitation");
        let capture_limitation = capture["limitation"].as_str().expect("limitation text");
        assert!(capture_limitation.contains("styled cell PNGs"));
        assert!(capture_limitation.contains("standalone headless renderer"));
        let focus = limited
            .iter()
            .find(|item| item["name"].as_str() == Some("instance.focus"))
            .expect("instance focus limitation");
        assert!(focus["limitation"]
            .as_str()
            .expect("limitation text")
            .contains("host GUI window"));
        let title = limited
            .iter()
            .find(|item| item["name"].as_str() == Some("instance.set_title"))
            .expect("instance title limitation");
        assert!(title["limitation"]
            .as_str()
            .expect("limitation text")
            .contains("server_info title metadata"));

        assert_eq!(caps["diagnostics"]["health_io_summary"], true);
        assert_eq!(caps["diagnostics"]["runtime_pump_summary"], true);
        assert_eq!(caps["diagnostics"]["launch_context"], true);
        assert_eq!(caps["diagnostics"]["launch_policy_decisions"], true);
        assert_eq!(caps["diagnostics"]["default_shell_launch_decision"], true);
        assert_eq!(caps["diagnostics"]["session_create_launch_decision"], true);
        assert_eq!(caps["diagnostics"]["workspace_restore_launch_plan"], true);
        // No front end is hosting a test binary, so nothing can render.
        assert_eq!(caps["diagnostics"]["styled_scrollback_png"], false);
        assert_eq!(
            caps["diagnostics"]["styled_scrollback_renderer_metadata"],
            true
        );
        assert_eq!(caps["diagnostics"]["pty_write_confirmation"], true);
        assert_eq!(caps["diagnostics"]["recording_block_markdown"], true);
        assert_eq!(caps["diagnostics"]["recording_osc133_command_blocks"], true);
        assert_eq!(
            caps["diagnostics"]["validated_capture_scrollback_pane_ids"],
            true
        );
        assert_eq!(caps["diagnostics"]["host_window_bridge"], true);
        assert_eq!(caps["diagnostics"]["instance_title_bridge"], true);
        assert_eq!(
            caps["diagnostics"]["instance_lifecycle_observability"],
            true
        );
        assert_eq!(caps["diagnostics"]["instance_registry_diagnostics"], true);
        assert_eq!(caps["diagnostics"]["instance_shutdown_dry_run"], true);
        assert_eq!(caps["diagnostics"]["instance_registry_unregister"], true);
        assert_eq!(caps["diagnostics"]["native_window_lifecycle"], false);
        let metrics = strings_at(&caps["diagnostics"], "health_metrics");
        assert!(metrics.contains(&"input_writes"));
        assert!(metrics.contains(&"output_bytes"));
        assert!(metrics.contains(&"paste_count"));
        let pump_metrics = strings_at(&caps["diagnostics"], "runtime_pump_metrics");
        assert!(pump_metrics.contains(&"drain_calls"));
        assert!(pump_metrics.contains(&"dispatched_commands"));
        assert!(pump_metrics.contains(&"dispatched_screen_commands"));
        assert!(pump_metrics.contains(&"waited_for_response"));
        assert!(pump_metrics.contains(&"completed_without_wait"));
        assert!(pump_metrics.contains(&"max_dispatch_elapsed_micros"));
    }

    #[test]
    fn surface_reports_mcp_tool_count_contract() {
        let value = surface(&json!({})).expect("meta.surface");

        assert_eq!(value["mcp_method_count"], 152);
        assert_eq!(value["mcp_tool_count"], 151);
        assert_eq!(value["mcp_tool_exclusions"], json!(["meta.surface"]));
        assert_eq!(
            value["mcp_methods"].as_array().expect("mcp methods").len(),
            152
        );
    }
}
