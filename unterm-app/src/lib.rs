//! Everything the terminal is, as a library.
//!
//! `main.rs` next door is four lines that call [`main`]. The split is not
//! taste: a bin target's unit tests and the bin itself are both
//! `kind=bin, name=unterm` to cargo, and both uplift to
//! `target/debug/unterm.exe` -- so whichever linked last was the file
//! `CARGO_BIN_EXE_unterm` named, and `tests/version_exit.rs` spent half its
//! runs asking a libtest harness for `--version`. Unit tests of a lib target
//! are `kind=lib, name=unterm_app`, land in `deps/`, and are never uplifted.

mod admin;
mod args;
mod background;
mod brand;
mod charselect;
mod chrome;
mod chrome_font;
mod clipboard;
mod cockpit;
mod composer;
mod confirm;
mod copy_mode;
mod dir_jump;
mod directory;
mod engine_backend;
mod fleet;
mod fonts;
mod forward;
mod ghost;
mod git;
mod ime;
#[cfg(target_os = "macos")]
mod ime_watch;
#[cfg(target_os = "macos")]
mod macos_open;
mod keys;
mod links;
mod mcp_host;
mod mouse;
mod palette;
mod panes;
mod paneselect;
mod scroll;
mod scrollbar;
mod search;
mod select;
mod session_restore;
mod shape;
mod sidebar;
mod statsbar;
mod stallwatch;
mod startup_trace;
mod statusbar;
mod system_capture;
mod terminal;
mod theme;
mod topbar;
mod tray;
mod tree;
mod ui_tokens;
mod unicode_names;
mod window;
mod win_chrome;
mod window_buttons;
mod workspaces;

/// Say something the user can see, on the platform where nobody sees stderr.
///
/// unterm.exe is a GUI-subsystem binary: its stderr goes nowhere unless a
/// developer redirected it. v0.61.0 could die during startup -- a wgpu error
/// is fatal by default -- and all the user saw was a double-click that did
/// nothing. Everything fatal now goes through here: a message box on Windows,
/// stderr elsewhere, and a line in ~/.unterm/panic.log either way.
pub(crate) fn report_fatal(text: &str) {
    log::error!("{text}");
    if let Some(dir) = unterm_protocol::state_dir() {
        let _ = std::fs::create_dir_all(&dir);
        let stamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
        let line = format!("[{stamp}] {text}\n");
        use std::io::Write as _;
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("panic.log"))
        {
            let _ = file.write_all(line.as_bytes());
        }
    }
    #[cfg(windows)]
    {
        #[link(name = "user32")]
        extern "system" {
            fn MessageBoxW(hwnd: isize, text: *const u16, caption: *const u16, kind: u32) -> i32;
        }
        let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
        let text = wide(text);
        let caption = wide("Unterm");
        const MB_ICONERROR: u32 = 0x10;
        unsafe {
            MessageBoxW(0, text.as_ptr(), caption.as_ptr(), MB_ICONERROR);
        }
    }
}

/// Leave a trace of a panic where a user can find it.
///
/// The default hook prints to the invisible stderr and the process vanishes.
/// This one writes the file first (a background thread's panic is worth a
/// line too), and only interrupts with a dialog when the main thread -- and
/// so the terminal itself -- is going down.
fn install_panic_reporter() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let is_main = std::thread::current().name() == Some("main");
        let text = format!("unterm panicked: {info}");
        if is_main {
            report_fatal(&text);
        } else if let Some(dir) = unterm_protocol::state_dir() {
            let _ = std::fs::create_dir_all(&dir);
            let stamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
            use std::io::Write as _;
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join("panic.log"))
            {
                let _ = file.write_all(format!("[{stamp}] {text}\n").as_bytes());
            }
        }
        default_hook(info);
    }));
}

/// Reattach to the console this process was typed into, if there was one.
///
/// The `windows` subsystem detaches stdout at birth, which is right for an
/// Explorer launch and wrong for `unterm --version` in a shell: the version
/// probe would print nothing and release scripts would read that silence as
/// a broken binary. Attaching to the parent console (and only the parent --
/// never allocating a fresh one) restores printing exactly where a console
/// already existed.
#[cfg(windows)]
fn attach_parent_console() {
    use winapi::um::wincon::{AttachConsole, ATTACH_PARENT_PROCESS};
    // Attaching updates the process's standard handles, and Rust fetches
    // them per write, so println! simply works afterwards. Failure means
    // there is no parent console -- an Explorer launch -- and silence is
    // exactly what that case wants.
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

pub fn main() -> std::process::ExitCode {
    #[cfg(windows)]
    attach_parent_console();
    // This path must remain before logging, config migration, instance
    // registration, server creation and winit initialization. Release and
    // supervisor probes can therefore identify a binary without launching it.
    if version_requested(std::env::args_os().skip(1)) {
        println!("unterm {}", unterm_protocol::PRODUCT_VERSION);
        return std::process::ExitCode::SUCCESS;
    }
    if help_requested(std::env::args_os().skip(1)) {
        print!("{}", help_text());
        return std::process::ExitCode::SUCCESS;
    }
    env_logger::Builder::from_env(env_logger::Env::default().filter_or("UNTERM_LOG", "info"))
        .init();
    install_panic_reporter();
    startup_trace::init();
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            report_fatal(&format!("Unterm could not start: {err:#}"));
            std::process::ExitCode::FAILURE
        }
    }
}

fn version_requested(arguments: impl IntoIterator<Item = std::ffi::OsString>) -> bool {
    let mut arguments = arguments.into_iter();
    matches!(
        (arguments.next(), arguments.next()),
        (Some(argument), None) if argument == "--version" || argument == "-V"
    )
}

fn help_requested(arguments: impl IntoIterator<Item = std::ffi::OsString>) -> bool {
    let mut arguments = arguments.into_iter();
    matches!(
        (arguments.next(), arguments.next()),
        (Some(argument), None) if argument == "--help" || argument == "-h"
    )
}

fn help_text() -> String {
    format!("unterm {}", unterm_protocol::PRODUCT_VERSION) + "\n\n\
USAGE:\n\
    unterm [start] [--cwd <dir>] [--tab] [--profile <id>] [--config <file>] [-- <command>...]\n\n\
OPTIONS:\n\
    --cwd <dir>       Start the first shell in this directory\n\
    --tab             Open the directory as a tab in a live window when possible\n\
    --profile <id>    Bind the first pane to an Unterm profile\n\
    -c, --config <file>\n\
                      Read this config file instead of the default\n\
    -V, --version     Print version and exit before initialization\n\
    -h, --help        Print help and exit before initialization\n"
}

fn run() -> anyhow::Result<()> {
    let mut args = args::parse(std::env::args().skip(1));
    startup_trace::mark("args.parsed");
    for argument in &args.unrecognised {
        log::warn!("ignoring unrecognised argument {argument:?}");
    }
    // The administrator window: elevated, alone, and reachable by nothing
    // unelevated. Honoured only with a real administrator token; see `admin`.
    let admin = args.admin && admin::is_elevated();
    if admin {
        args.config = admin::prepare(args.config.take());
        log::info!("administrator window: in-process sessions, no agent surface");
    }

    // "Open in Unterm tab": the window the user means is the one already
    // open. Forward the directory there and exit; only with nobody to take
    // it does this process go on to become a window itself.
    if args.tab && !admin {
        if let Some(cwd) = args.cwd.as_deref() {
            match forward::open_tab_in_live_window(cwd) {
                Ok(()) => return Ok(()),
                Err(err) => {
                    log::info!("no live window took the tab ({err:#}); opening one");
                }
            }
        }
    }

    // A Lua config from an older build, converted once so the settings the
    // user wrote are not silently dropped on upgrade.
    if let Err(err) = migrate_old_config() {
        log::warn!("could not convert the previous config: {err:#}");
    }
    startup_trace::mark("config.migration_checked");

    // The same declarative config the rest of the product reads. Unreadable or
    // absent, we start on defaults rather than refusing to open: a terminal you
    // cannot open is no way to fix your config.
    let (config, errors) = unterm_services::settings::load(args.config.clone());
    for error in &errors {
        log::warn!("config line {}: {}", error.line, error.message);
    }
    startup_trace::mark("config.loaded");
    apply_path_append(&config);
    apply_session_env(&config);
    startup_trace::mark("env.applied");
    // The `[keys]` section, folded into the key table before the window and
    // the MCP surface start reading it. A broken entry is a warning with its
    // line, never a refusal to start.
    let (user_bindings, binding_errors) = keys::user_bindings_from(&config);
    for error in &binding_errors {
        log::warn!("config line {}: {}", error.line, error.message);
    }
    keys::install_user_bindings(user_bindings);
    startup_trace::mark("keys.installed");
    if let Ok(Some(milliseconds)) = config.int_of("stats.refresh_ms") {
        if let Ok(milliseconds) = u64::try_from(milliseconds) {
            statsbar::set_refresh_ms(milliseconds);
        }
    }
    unterm_services::settings::set_current(&config);
    unterm_engine::next_core::NextCoreEngine::set_new_session_scrollback_lines(
        unterm_services::settings::scrollback_lines(&config),
    );
    // Decide Local vs Core for the whole process -- window, MCP
    // surface and background threads alike -- before the MCP server
    // starts, so every consumer sees the same session world.
    let backend = engine_backend::init_from_environment();
    startup_trace::mark("backend.ready");
    if let Some(profile) = args.profile.as_deref() {
        std::env::set_var("UNTERM_STARTUP_PROFILE", profile);
    }

    // The agent-facing API. The engine provider was installed by
    // init_from_environment above (next-core, or the Core process);
    // this app answers the two things that need a font stack and a
    // key table. Installed before the window opens so an agent
    // connecting early finds a working surface rather than a
    // half-built one.
    mcp_host::install();
    startup_trace::mark("mcp_host.installed");
    // One surface per session world. In Core mode the Core is already
    // serving the agent API over the sessions it owns; a second server
    // here would answer the same questions from an empty world, and an
    // agent would have no way to tell which one it reached.
    let served_here = matches!(backend, engine_backend::Backend::Local);
    let (_port, token) = if admin {
        // Nothing for the unelevated side to connect to: no MCP server, no
        // instance record, no settings page.
        (0, String::new())
    } else if served_here {
        let (port, token) =
            unterm_mcp::start_mcp_server_with_version(unterm_protocol::PRODUCT_VERSION);
        log::info!("MCP server listening on 127.0.0.1:{port}");
        (port, token)
    } else {
        log::info!("agent surface is served by unterm-core; this window hosts its window half");
        // Serving no MCP does not excuse the window from registering: the
        // settings page bootstraps its credentials from this instance's
        // record, and every legacy client finds the surface through
        // server.json. Register with the Core's port and the Core's token,
        // so both roads lead to the server that actually owns the sessions.
        match unterm_core::read_discovery() {
            Ok(Some(core)) => {
                let port = core.mcp_port.unwrap_or(0);
                match unterm_services::server_info::write_initial_with_version_token(
                    port,
                    unterm_protocol::PRODUCT_VERSION,
                    Some(core.token.clone()),
                ) {
                    Ok(_) => (port, core.token),
                    Err(err) => {
                        log::warn!("could not register this window's instance: {err:#}");
                        (0, String::new())
                    }
                }
            }
            other => {
                if let Err(err) = other {
                    log::warn!("could not read core discovery: {err:#}");
                }
                (0, String::new())
            }
        }
    };
    startup_trace::mark("instance.registered");
    if !admin {
        start_shared_services(token);
    }
    stallwatch_and_watchers();
    let event_loop = winit::event_loop::EventLoop::new()?;
    startup_trace::mark("event_loop.created");
    finish_startup(&config, args, event_loop)
}

/// What a normal window shares with the rest of the machine: the bridge
/// bookkeeping, the settings page and the update check. The administrator
/// window runs none of them.
fn start_shared_services(token: String) {
    match unterm_services::bridge_registry::request_incompatible_drains() {
        Ok(0) => {}
        Ok(count) => log::info!("requested drain for {count} incompatible MCP bridge(s)"),
        Err(error) => log::warn!("could not inspect MCP bridge lifecycle records: {error:#}"),
    }
    startup_trace::mark("bridge_registry.checked");
    // The enforcement half of the cooperative replacement: bridges
    // that predate the registry get terminated right away (they will
    // never hear a drain request), and drained ones get a grace
    // period before force applies.
    std::thread::Builder::new()
        .name("bridge-drain-enforcer".into())
        .spawn(|| {
            use std::time::Duration;
            match unterm_services::bridge_registry::drain_unregistered_bridges(
                Duration::from_secs(300),
            ) {
                Ok(0) | Err(_) => {}
                Ok(count) => log::warn!("terminated {count} pre-registry MCP bridge(s)"),
            }
            std::thread::sleep(Duration::from_secs(30));
            match unterm_services::bridge_registry::terminate_overdue_drains(
                Duration::from_secs(30),
            ) {
                Ok(0) | Err(_) => {}
                Ok(count) => log::warn!("force-terminated {count} overdue MCP bridge(s)"),
            }
        })
        .ok();

    // The settings UI, on the same token. `unterm-cli settings` opens it.
    let settings_port = unterm_settings::start_web_settings_server(token);
    log::info!("settings UI listening on 127.0.0.1:{settings_port}");
    startup_trace::mark("settings_server.started");

    // The update check, which existed but was never started: without this
    // call the Updates page always said "never checked" and nobody was told
    // a release had shipped.
    unterm_settings::update_check::start_background_poller();
    startup_trace::mark("update_poller.started");
}

fn stallwatch_and_watchers() {
    // The stall watchdog, armed before the loop it watches exists: a frozen
    // window must leave a trace, not an argument about whether it happened.
    stallwatch::start();
    startup_trace::mark("stallwatch.started");
    // And the input-source watcher, so a composition stranded by a source
    // switch is cleared before it can swallow anyone's Backspace.
    #[cfg(target_os = "macos")]
    ime_watch::start();
}

fn finish_startup(
    config: &unterm_engine::next_core::config::Config,
    args: args::Args,
    event_loop: winit::event_loop::EventLoop<()>,
) -> anyhow::Result<()> {
    // The loop's delegate exists now; teach it to answer Finder and friends
    // when they say "open this folder".
    #[cfg(target_os = "macos")]
    macos_open::install();
    // And make sure the half of that conversation Finder owns is still
    // switched on: replacing the bundle registers the extension afresh, and
    // a fresh registration is not an enabled one.
    #[cfg(target_os = "macos")]
    macos_open::keep_finder_extension_enabled();
    mcp_host::install_waker(event_loop.create_proxy());
    let mut app = window::App::new(config)?;
    startup_trace::mark("app.created");
    // A plain launch reopens where the last one closed; naming a directory
    // or a command on the line asks for something specific instead.
    if args.cwd.is_none() && args.command.is_empty() {
        if let Some(saved) = session_restore::load() {
            app.set_restore(saved);
        }
    }
    app.set_start_directory(args.cwd);
    app.set_start_command(args.command);
    startup_trace::mark("event_loop.running");
    let run_result = event_loop.run_app(&mut app);
    let shutdown = unterm_services::server_info::unregister_current_instance();
    if !shutdown.errors.is_empty() {
        log::warn!(
            "could not fully unregister instance during shutdown: {}",
            shutdown.errors.join("; ")
        );
    }
    run_result?;
    Ok(())
}

/// Extend the environment inherited by every new pane.
///
/// The 0.57 Windows config did this in Lua. Keeping it at process startup
/// makes shell discovery, agent discovery and the shells themselves see one
/// identical PATH.
fn apply_path_append(config: &unterm_engine::next_core::config::Config) {
    // Only the Windows block below appends to it; other platforms read as-is.
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut additions: Vec<std::path::PathBuf> = config
        .list_of("path_append")
        .ok()
        .flatten()
        .into_iter()
        .flatten()
        .filter_map(|value| match value {
            unterm_engine::next_core::config::Value::Str(path) => {
                Some(std::path::PathBuf::from(path))
            }
            _ => None,
        })
        .collect();

    #[cfg(windows)]
    {
        let mut standard = vec![
            std::path::PathBuf::from(r"C:\Program Files\nodejs"),
            std::path::PathBuf::from(r"C:\Strawberry\perl\bin"),
        ];
        if let Some(appdata) = std::env::var_os("APPDATA") {
            standard.push(std::path::PathBuf::from(appdata).join("npm"));
        }
        if let Some(home) = std::env::var_os("USERPROFILE") {
            standard.push(std::path::PathBuf::from(home).join(".bun").join("bin"));
        }
        additions.extend(standard.into_iter().filter(|path| path.is_dir()));
    }

    let current = std::env::var_os("PATH").unwrap_or_default();
    let paths = merged_path(std::env::split_paths(&current), additions);
    match std::env::join_paths(paths) {
        Ok(path) => std::env::set_var("PATH", path),
        Err(error) => log::warn!("could not extend PATH: {error}"),
    }
}

fn merged_path(
    current: impl IntoIterator<Item = std::path::PathBuf>,
    additions: impl IntoIterator<Item = std::path::PathBuf>,
) -> Vec<std::path::PathBuf> {
    let mut paths: Vec<std::path::PathBuf> = current.into_iter().collect();
    for addition in additions {
        let duplicate = paths.iter().any(|known| {
            if cfg!(windows) {
                known
                    .to_string_lossy()
                    .eq_ignore_ascii_case(&addition.to_string_lossy())
            } else {
                known == &addition
            }
        });
        if !duplicate {
            paths.push(addition);
        }
    }
    paths
}

/// Put the `[env]` section into the environment every new pane inherits.
///
/// The Lua config called this `set_environment_variables`. Setting it on the
/// process, like `path_append` above, means the pty spawn path, shell
/// discovery and the agents this terminal launches all see the same values.
fn apply_session_env(config: &unterm_engine::next_core::config::Config) {
    let names: Vec<String> = config
        .keys()
        .filter_map(|key| key.strip_prefix("env."))
        .map(String::from)
        .collect();
    for name in names {
        match config.str_of(&format!("env.{name}")) {
            Ok(Some(value)) => std::env::set_var(&name, value),
            Ok(None) => {}
            // A non-string value: report it with its line rather than
            // exporting something the user did not write.
            Err(error) => log::warn!("config line {}: {}", error.line, error.message),
        }
    }
}

/// Convert a Lua config from an older build, once.
///
/// The previous terminal read `unterm.lua`; this one reads `unterm.conf`. A
/// user upgrading has settings written in the old format and no reason to
/// know the format changed, so the first run converts what it can and leaves
/// the original alone -- a conversion that eats the file it read is one
/// nobody can check.
///
/// Only when there is no new-format config yet: converting over one the user
/// has since written would undo their work.
fn migrate_old_config() -> anyhow::Result<()> {
    use unterm_engine::next_core::config_migrate;

    let Some(home) = dirs_next::home_dir() else {
        return Ok(());
    };
    // The old candidates below are read from the real home directory, but
    // the file we write -- and the one old copy that lived beside it -- are
    // state, so they follow the state directory wherever it points.
    let Some(state_dir) = unterm_protocol::state_dir() else {
        return Ok(());
    };
    let new_path = state_dir.join("unterm.conf");
    if new_path.exists() {
        return Ok(());
    }

    let candidates = [
        // This was the normal Windows/Linux config location used by the
        // WezTerm-based releases. Missing it made an upgrade silently fall
        // back to the new bundled font and spacing even though the user's
        // v0.57.4 configuration was still present.
        home.join(".config").join("unterm").join("unterm.lua"),
        state_dir.join("unterm.lua"),
        home.join(".unterm.lua"),
        home.join(".wezterm.lua"),
    ];
    let Some(old_path) = candidates.into_iter().find(|path| path.exists()) else {
        return Ok(());
    };

    let source = std::fs::read_to_string(&old_path)?;
    let migration = config_migrate::migrate_lua(&source);
    if let Some(parent) = new_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&new_path, &migration.text)?;

    log::info!("converted {} to {}", old_path.display(), new_path.display());
    for left in &migration.unconverted {
        log::info!("  not converted: {left:?}");
    }
    Ok(())
}

#[cfg(test)]
mod path_tests {
    use super::*;

    #[test]
    fn path_extensions_keep_order_and_do_not_duplicate_entries() {
        let current = [
            std::path::PathBuf::from("first"),
            std::path::PathBuf::from("second"),
        ];
        let merged = merged_path(
            current,
            [
                std::path::PathBuf::from("second"),
                std::path::PathBuf::from("third"),
            ],
        );

        assert_eq!(
            merged,
            [
                std::path::PathBuf::from("first"),
                std::path::PathBuf::from("second"),
                std::path::PathBuf::from("third"),
            ]
        );
    }

    #[test]
    fn the_env_section_reaches_the_process_environment() {
        let config = unterm_engine::next_core::config::parse(
            "[env]\nUNTERM_TEST_ENV_SECTION = \"on\"\nUNTERM_TEST_ENV_BAD = 3",
        )
        .unwrap();

        apply_session_env(&config);

        // The string is exported; the non-string is reported, not invented.
        assert_eq!(
            std::env::var("UNTERM_TEST_ENV_SECTION").as_deref(),
            Ok("on")
        );
        assert!(std::env::var("UNTERM_TEST_ENV_BAD").is_err());
    }

    #[test]
    #[cfg(windows)]
    fn windows_path_deduplication_ignores_case() {
        let merged = merged_path(
            [std::path::PathBuf::from(r"C:\Tools")],
            [std::path::PathBuf::from(r"c:\tools")],
        );
        assert_eq!(merged.len(), 1);
    }
}
