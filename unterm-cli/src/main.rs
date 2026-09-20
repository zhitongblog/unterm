//! The Unterm command line.
//!
//! Everything here talks to a running Unterm over its MCP server, or to the
//! files Unterm keeps under `~/.unterm`. Nothing needs a window, which is why
//! this is a binary of its own rather than a mode of the terminal: an agent on
//! a headless box uses the same commands a user does at a prompt.

use anyhow::{Context, Result};
use clap::{Parser, ValueHint};
use clap_complete::{generate as generate_completion, shells::Shell};

mod agent;
mod client;
mod cockpit_hooks;
mod exec;
mod fleet;
mod i18n;
mod instance;
mod lang;
mod legacy;
mod mcp_stdio;
mod output;
mod policy;
mod provider;
mod records;
mod profile;
mod proxy;
mod reference;
mod review;
mod screenshot;
mod scrollback;
mod server;
mod session;
mod sessions;
mod settings;
mod setup_ai;
mod theme;
mod upload;
mod workspace;

use agent::AgentCommand;
use exec::ExecCommand;
use fleet::FleetCommand;
use instance::InstanceCommand;
use lang::LangCommand;
use legacy::LegacyCommand;
use policy::PolicyCommand;
use provider::ProviderCommand;
use records::{ArtifactCommand, EvidenceCommand, ScopeCommand, SystemCommand};
use profile::ProfileCommand;
use proxy::ProxyCommand;
use reference::ReferenceCommand;
use review::ReviewCommand;
use scrollback::ScrollbackCommand;
use server::ServerCommand;
use session::SessionCommand;
use sessions::SessionsCommand;
use settings::SettingsCommand;
use setup_ai::SetupAiCommand;
use theme::ThemeCommand;
use upload::UploadCommand;
use workspace::WorkspaceCommand;

#[derive(Debug, Parser)]
#[command(
    name = "unterm-cli",
    about = "Drive Unterm from a shell or a script",
    version
)]
struct Opt {
    /// Emit machine-readable JSON for commands that support structured output.
    /// MCP-backed commands print the JSON-RPC `result`; local commands such as
    /// setup-ai dry-runs print their own stable result object.
    #[arg(long = "json", global = true)]
    json: bool,

    /// Override the interface locale for this invocation only (does not
    /// persist). Use `unterm-cli lang set <code>` to make it permanent.
    #[arg(long = "lang", global = true, value_name = "code")]
    lang: Option<String>,

    /// Route MCP-backed CLI commands to a specific running Unterm instance
    /// such as alpha, bravo, or charlie. Defaults to active/latest.
    #[arg(long = "instance", global = true, value_name = "id")]
    instance: Option<String>,

    #[command(subcommand)]
    cmd: SubCommand,
}

#[derive(Debug, Parser)]
enum SubCommand {
    #[command(name = "start", about = "Start a new Unterm GUI instance")]
    Start {
        /// Directory for the first pane.
        #[arg(long, value_hint = ValueHint::DirPath)]
        cwd: Option<std::path::PathBuf>,
        /// Identity profile to bind to the new window.
        #[arg(long)]
        profile: Option<String>,
        /// Program and arguments for the first pane; place them after `--`.
        #[arg(last = true)]
        command: Vec<String>,
    },

    #[command(
        name = "profile",
        about = "Manage identity profiles (GitHub / AWS / npm tokens, git identity, SSH keys)"
    )]
    Profile(ProfileCommand),

    #[command(name = "proxy", about = "Manage Unterm's proxy via the MCP server")]
    Proxy(ProxyCommand),

    #[command(name = "theme", about = "List/switch Unterm theme presets")]
    Theme(ThemeCommand),

    #[command(name = "session", about = "Operate on a single live pane")]
    Session(SessionCommand),

    #[command(name = "exec", about = "Run commands in a live pane via MCP")]
    Exec(ExecCommand),

    #[command(name = "sessions", about = "Browse the recorded session archive")]
    Sessions(SessionsCommand),

    #[command(name = "workspace", about = "Save or restore named pane workspaces")]
    Workspace(WorkspaceCommand),

    #[command(
        name = "instance",
        about = "List, inspect, label, or focus live Unterm instances"
    )]
    Instance(InstanceCommand),

    #[command(
        name = "settings",
        about = "Open the Unterm Web Settings UI in your browser"
    )]
    Settings(SettingsCommand),

    #[command(
        name = "lang",
        about = "List, set, or print the active interface locale"
    )]
    Lang(LangCommand),

    #[command(name = "policy", about = "Inspect MCP write-policy decisions")]
    Policy(PolicyCommand),

    #[command(
        name = "system",
        about = "Process health, redacted diagnostics, and data snapshots"
    )]
    System(SystemCommand),

    #[command(
        name = "scope",
        about = "Workspaces: named roots that work is confined to, and blind to each other"
    )]
    Scope(ScopeCommand),

    #[command(
        name = "artifact",
        about = "What tasks produced, addressed by content"
    )]
    Artifact(ArtifactCommand),

    #[command(
        name = "evidence",
        about = "Export one task's whole story, verify a bundle, or check the audit chain"
    )]
    Evidence(EvidenceCommand),

    #[command(
        name = "provider",
        about = "Bind, pause, diagnose and revoke the capability providers Unterm can reach"
    )]
    Provider(ProviderCommand),

    #[command(
        name = "agent",
        about = "Install, authenticate, configure, and launch AI coding-agent CLIs (Claude Code / Codex / Gemini / OpenCode / Aider)"
    )]
    Agent(AgentCommand),

    #[command(
        name = "fleet",
        about = "Run one task across N agents in N isolated git worktrees (Agent Cockpit)"
    )]
    Fleet(FleetCommand),

    #[command(
        name = "review",
        about = "Inspect, merge, discard, or roll back agent-produced changes (Agent Cockpit)"
    )]
    Review(ReviewCommand),

    #[command(
        name = "screenshot",
        about = "Capture the screen via Unterm's MCP server. \
                 --scrollback renders a pane's entire history to one tall PNG; \
                 --scroll-app/--scroll-title long-screenshots another app's window (macOS)"
    )]
    Screenshot {
        /// Include Unterm's own window in the capture (default: exclude).
        #[arg(long = "include-window")]
        include_window: bool,
        /// Capture only Unterm's own window, not the whole screen. Uses the
        /// running server's CGWindowID — works even when Unterm isn't the
        /// frontmost app and never depends on what's behind it.
        #[arg(long = "self", conflicts_with_all = ["scrollback", "scroll_app", "scroll_title", "scroll_pid"])]
        self_window: bool,
        /// Include base64 PNG bytes in --json output. Supported for normal
        /// screen capture and --self; long screenshot modes return paths.
        #[arg(long = "base64", conflicts_with_all = ["scrollback", "scroll_app", "scroll_title", "scroll_pid"])]
        base64: bool,
        /// In-terminal long screenshot: render the pane's ENTIRE scrollback
        /// to one tall PNG (headless re-render; window may be occluded).
        #[arg(long = "scrollback")]
        scrollback: bool,
        /// Pane id for --scrollback (default: the active pane).
        #[arg(long = "pane-id", aliases = ["id", "pane"])]
        pane: Option<u64>,
        /// Row cap for --scrollback; keeps the most recent rows (default 10000).
        #[arg(long = "max-rows")]
        max_rows: Option<u64>,
        /// Raster dpi for --scrollback, 48-288 (default 144 on macOS).
        #[arg(long = "dpi")]
        dpi: Option<u64>,
        /// External long screenshot: scroll + stitch the window of the app
        /// whose name contains this substring (macOS), e.g. "Safari".
        #[arg(long = "scroll-app")]
        scroll_app: Option<String>,
        /// External long screenshot: match the window by title substring.
        #[arg(long = "scroll-title")]
        scroll_title: Option<String>,
        /// External long screenshot: match the window by owning pid.
        #[arg(long = "scroll-pid")]
        scroll_pid: Option<u64>,
        /// Frame cap for external scroll capture (default 25).
        #[arg(long = "max-frames")]
        max_frames: Option<u64>,
        /// Optional output PNG path. If omitted, the MCP-side path is printed.
        #[arg(short = 'o', long = "output", value_hint=ValueHint::FilePath)]
        output: Option<std::path::PathBuf>,
    },

    #[command(
        name = "upload",
        about = "Upload a local file to your configured object storage \
                 (Aliyun OSS / Tencent COS / Qiniu Kodo) and print the public URL"
    )]
    Upload(UploadCommand),

    #[command(
        name = "scrollback",
        about = "Dump the full scrollback + viewport of a pane as text \
                 (AI-friendly alternative to a rendered long screenshot)"
    )]
    Scrollback(ScrollbackCommand),

    #[command(
        name = "reference",
        about = "Print MCP methods, CLI subcommands, and live keybindings \
                 (one-call surface inventory for agents and operators)"
    )]
    Reference(ReferenceCommand),

    #[command(
        name = "server",
        about = "Inspect the running Unterm MCP server health and capabilities"
    )]
    Server(ServerCommand),

    #[command(
        name = "setup-ai",
        about = "Register Unterm with every AI coding agent on this machine \
                 (Claude Code / Codex / Gemini / Cursor / Windsurf / OpenCode) \
                 so they auto-discover and can drive the terminal. Idempotent; \
                 use --remove to undo."
    )]
    SetupAi(SetupAiCommand),

    #[command(
        name = "mcp-stdio",
        about = "Run an MCP (Model Context Protocol) stdio server that bridges \
                 an AI agent to this Unterm instance. Spawned automatically by \
                 the agent launcher; rarely run by hand."
    )]
    McpStdio,

    #[command(name = "cli", about = "Legacy mux compatibility command")]
    Cli(LegacyCommand),

    #[command(name = "show-keys", about = "Show effective key assignments")]
    ShowKeys,

    #[command(name = "ls-fonts", about = "Display font discovery locations")]
    LsFonts,

    #[command(name = "imgcat", about = "Output an image to the terminal")]
    Imgcat {
        /// Image file to print inline.
        #[arg(value_hint = ValueHint::FilePath)]
        path: std::path::PathBuf,
    },

    #[command(
        name = "set-working-directory",
        about = "Emit an OSC 7 escape so Unterm learns the cwd"
    )]
    SetWorkingDirectory {
        /// Directory to report. Defaults to the current directory.
        #[arg(value_hint = ValueHint::DirPath)]
        path: Option<std::path::PathBuf>,
    },

    #[command(name = "record", about = "Legacy recording compatibility command")]
    Record(LegacyCommand),

    #[command(name = "replay", about = "Legacy replay compatibility command")]
    Replay(LegacyCommand),

    #[command(name = "ssh", about = "Open an SSH command in a new Unterm pane")]
    Ssh(LegacyCommand),

    #[command(name = "connect", about = "Legacy mux connect compatibility command")]
    Connect(LegacyCommand),

    /// Generate shell completion information
    #[command(name = "shell-completion")]
    ShellCompletion {
        /// Which shell to generate for
        #[arg(long, value_parser)]
        shell: Shell,
    },
}

fn main() -> Result<()> {
    let opts = Opt::parse();
    apply_transient_lang(opts.lang.as_deref());
    set_target_instance(opts.instance.as_deref());
    match opts.cmd {
        SubCommand::Start {
            cwd,
            profile,
            command,
        } => run_start(cwd, profile, command),
        SubCommand::Profile(cmd) => run_profile(cmd, opts.json),
        SubCommand::Proxy(cmd) => run_proxy(cmd, opts.json),
        SubCommand::Theme(cmd) => run_theme(cmd, opts.json),
        SubCommand::Session(cmd) => run_session(cmd, opts.json),
        SubCommand::Exec(cmd) => run_exec(cmd, opts.json),
        SubCommand::Sessions(cmd) => run_sessions(cmd, opts.json),
        SubCommand::Workspace(cmd) => run_workspace(cmd, opts.json),
        SubCommand::Instance(cmd) => run_instance(cmd, opts.json),
        SubCommand::Screenshot {
            include_window,
            self_window,
            base64,
            output,
            scrollback,
            pane,
            max_rows,
            dpi,
            scroll_app,
            scroll_title,
            scroll_pid,
            max_frames,
        } => run_screenshot(
            ScreenshotArgs {
                include_window,
                self_window,
                base64,
                output,
                scrollback,
                pane,
                max_rows,
                dpi,
                scroll_app,
                scroll_title,
                scroll_pid,
                max_frames,
            },
            opts.json,
        ),
        SubCommand::Upload(cmd) => run_upload(cmd, opts.json),
        SubCommand::Scrollback(cmd) => run_scrollback(cmd, opts.json),
        SubCommand::Reference(cmd) => run_reference(cmd, opts.json),
        SubCommand::Server(cmd) => run_server(cmd, opts.json),
        SubCommand::SetupAi(cmd) => run_setup_ai(cmd, opts.json),
        SubCommand::McpStdio => run_mcp_stdio(),
        SubCommand::Cli(cmd) => legacy::run_cli(cmd, opts.json),
        SubCommand::ShowKeys => legacy::run_show_keys(opts.json),
        SubCommand::LsFonts => legacy::run_ls_fonts(opts.json),
        SubCommand::Imgcat { path } => legacy::run_imgcat(path),
        SubCommand::SetWorkingDirectory { path } => legacy::run_set_working_directory(path),
        SubCommand::Record(cmd) => legacy::run_record(cmd, opts.json),
        SubCommand::Replay(cmd) => legacy::run_replay(cmd, opts.json),
        SubCommand::Ssh(cmd) => legacy::run_ssh(cmd),
        SubCommand::Connect(cmd) => legacy::run_connect(cmd, opts.json),
        SubCommand::Settings(cmd) => run_settings(cmd),
        SubCommand::Lang(cmd) => run_lang(cmd, opts.json),
        SubCommand::Policy(cmd) => run_policy(cmd, opts.json),
        SubCommand::Provider(cmd) => provider::run(cmd, opts.json),
        SubCommand::System(cmd) => records::run_system(cmd, opts.json),
        SubCommand::Scope(cmd) => records::run_scope(cmd, opts.json),
        SubCommand::Artifact(cmd) => records::run_artifact(cmd, opts.json),
        SubCommand::Evidence(cmd) => records::run_evidence(cmd, opts.json),
        SubCommand::Agent(cmd) => run_agent(cmd, opts.json),
        SubCommand::Fleet(cmd) => run_fleet(cmd, opts.json),
        SubCommand::Review(cmd) => run_review(cmd, opts.json),
        SubCommand::ShellCompletion { shell } => {
            use clap::CommandFactory;
            let mut cmd = Opt::command();
            let name = cmd.get_name().to_string();
            generate_completion(shell, &mut cmd, name, &mut std::io::stdout());
            Ok(())
        }
    }
}

/// Ask a running front end for another window. False if none would take it.
/// Why a window could not be opened on a front end that is already running.
///
/// Distinguished from "nobody was there to ask", which is not a problem: the
/// first `start` on a quiet machine has nothing to hand over to and goes on
/// to become the window itself. A front end that *is* there and still could
/// not take it is a different matter, and the difference is the whole reason
/// this type exists -- see `run_start`.
enum HandOver {
    /// A window was opened on the running front end. Nothing more to do.
    Done,
    /// No front end is running. Becoming one is the right answer.
    NobodyHome,
    /// One is running and refused, or could not be reached.
    Refused(String),
}

fn hand_over_window(
    cwd: Option<&std::path::Path>,
    profile: Option<&str>,
    command: &[String],
) -> HandOver {
    // Is anyone actually there? Asked before connecting, because a failed
    // connection means two very different things depending on the answer.
    let live = unterm_services::server_info::list_live_instances();
    let mut client = match crate::client::McpClient::connect() {
        Ok(client) => client,
        Err(err) if live.is_empty() => {
            let _ = err;
            return HandOver::NobodyHome;
        }
        Err(err) => {
            return HandOver::Refused(format!(
                "{} front end(s) registered but none would talk: {err:#}",
                live.len()
            ))
        }
    };
    let mut params = serde_json::Map::new();
    if let Some(cwd) = cwd {
        params.insert("cwd".into(), cwd.display().to_string().into());
    }
    if let Some(profile) = profile {
        params.insert("profile".into(), profile.into());
    }
    if !command.is_empty() {
        params.insert("command".into(), command.to_vec().into());
    }
    match client.call("instance.new_window", serde_json::Value::Object(params)) {
        Ok(_) => HandOver::Done,
        Err(err) => HandOver::Refused(format!("{err:#}")),
    }
}

fn run_start(
    cwd: Option<std::path::PathBuf>,
    profile: Option<String>,
    command: Vec<String>,
) -> Result<()> {
    // A front end that is already up can open the window itself, and that is
    // worth asking for: starting a second process costs a GPU adapter --
    // ~200 ms, paid again by every process -- while a window on a front end
    // that already has one costs 31 ms.
    //
    // `--cwd`, `--profile` and a program used to force a second process:
    // `instance.new_window` took no arguments, so handing the ask over lost
    // it, and a window on the wrong folder is worse than a slow one. The
    // request carries them now, so every `start` can hand over -- which also
    // stops each one leaving a Core of its own behind when it quits.
    match hand_over_window(cwd.as_deref(), profile.as_deref(), &command) {
        HandOver::Done => return Ok(()),
        HandOver::NobodyHome => {}
        // Say it. A front end is running and would not take the window, so
        // what follows is a second process -- and a second process is not
        // the same product in a second window. It pays for its own GPU
        // adapter and its own font stack (587 ms against 31 ms, which is
        // what 0.68.2's single-process multi-window was for), and it shares
        // the Core with the one already running, so closing either of them
        // reaches for a Core that is not only its own.
        //
        // The usual cause is version skew: a `unterm-cli` newer than the
        // running front end is refused at the handshake, by design. Nothing
        // said so, so the fallback looked like ordinary behaviour, and the
        // windows piled up as processes -- slower, and fragile in a way that
        // only shows when one of them closes.
        HandOver::Refused(why) => {
            eprintln!("unterm: could not open a window on the running Unterm: {why}");
            eprintln!(
                "unterm: starting a separate process instead — it will be slower, and \
                 closing any window may take the others with it. \
                 `unterm-cli server health` says whether the versions match."
            );
        }
    }
    let current = std::env::current_exe().context("locating unterm-cli executable")?;
    let sibling = current.with_file_name(if cfg!(windows) {
        "unterm.exe"
    } else {
        "unterm"
    });
    let program = if sibling.is_file() {
        sibling
    } else {
        std::path::PathBuf::from(if cfg!(windows) {
            "unterm.exe"
        } else {
            "unterm"
        })
    };
    let mut launch = std::process::Command::new(&program);
    launch.arg("start");
    if let Some(cwd) = cwd {
        launch.arg("--cwd").arg(cwd);
    }
    if let Some(profile) = profile {
        launch.arg("--profile").arg(profile);
    }
    if !command.is_empty() {
        launch.arg("--").args(command);
    }
    launch
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    launch
        .spawn()
        .with_context(|| format!("starting {}", program.display()))?;
    Ok(())
}

pub fn set_target_instance(id: Option<&str>) {
    client::set_target_instance(id);
}

pub fn run_proxy(cmd: ProxyCommand, json_out: bool) -> Result<()> {
    proxy::run(cmd, json_out)
}

pub fn run_profile(cmd: ProfileCommand, json_out: bool) -> Result<()> {
    profile::run(cmd, json_out)
}

pub fn run_theme(cmd: ThemeCommand, json_out: bool) -> Result<()> {
    theme::run(cmd, json_out)
}

pub fn run_session(cmd: SessionCommand, json_out: bool) -> Result<()> {
    session::run(cmd, json_out)
}

pub fn run_sessions(cmd: SessionsCommand, json_out: bool) -> Result<()> {
    sessions::run(cmd, json_out)
}

use screenshot::ScreenshotArgs;

pub fn run_screenshot(args: ScreenshotArgs, json_out: bool) -> Result<()> {
    screenshot::run(args, json_out)
}

pub fn run_settings(cmd: SettingsCommand) -> Result<()> {
    settings::run(cmd)
}

pub fn run_lang(cmd: LangCommand, json_out: bool) -> Result<()> {
    lang::run(cmd, json_out)
}

pub fn run_policy(cmd: PolicyCommand, json_out: bool) -> Result<()> {
    policy::run(cmd, json_out)
}

pub fn run_agent(cmd: AgentCommand, json_out: bool) -> Result<()> {
    agent::run(cmd, json_out)
}

pub fn run_fleet(cmd: FleetCommand, json_out: bool) -> Result<()> {
    fleet::run(cmd, json_out)
}

pub fn run_review(cmd: ReviewCommand, json_out: bool) -> Result<()> {
    review::run(cmd, json_out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;
    use std::collections::HashSet;

    #[test]
    fn reference_cli_commands_are_real_clap_subcommands() {
        let command = Opt::command();
        let actual: HashSet<_> = command
            .get_subcommands()
            .map(|sub| sub.get_name())
            .collect();
        let missing: Vec<_> = unterm_agents::mcp_meta::CLI_COMMANDS
            .iter()
            .map(|command| command.name)
            .filter(|name| !actual.contains(name))
            .collect();
        assert!(
            missing.is_empty(),
            "reference advertises CLI commands missing from clap parser: {missing:?}"
        );
    }

    #[test]
    fn reference_cli_subcommands_match_real_clap_subcommands() {
        let command = Opt::command();
        let actual: std::collections::HashMap<_, _> = command
            .get_subcommands()
            .map(|sub| {
                let subcommands: HashSet<_> =
                    sub.get_subcommands().map(|cmd| cmd.get_name()).collect();
                (sub.get_name().to_string(), subcommands)
            })
            .collect();

        let mut drift = Vec::new();
        for advertised in unterm_agents::mcp_meta::CLI_COMMANDS {
            let Some(real) = actual.get(advertised.name) else {
                continue;
            };
            let expected: HashSet<_> = advertised.subcommands.iter().copied().collect();
            let missing: Vec<_> = real.difference(&expected).copied().collect();
            let extra: Vec<_> = expected.difference(real).copied().collect();
            if !missing.is_empty() || !extra.is_empty() {
                drift.push(format!(
                    "{} missing_from_reference={missing:?} extra_in_reference={extra:?}",
                    advertised.name
                ));
            }
        }

        assert!(
            drift.is_empty(),
            "reference CLI subcommands drifted from clap parser: {drift:?}"
        );
    }
}

pub fn run_exec(cmd: ExecCommand, json_out: bool) -> Result<()> {
    exec::run(cmd, json_out)
}

pub fn run_instance(cmd: InstanceCommand, json_out: bool) -> Result<()> {
    instance::run(cmd, json_out)
}

pub fn run_upload(cmd: UploadCommand, json_out: bool) -> Result<()> {
    upload::run(cmd, json_out)
}

pub fn run_workspace(cmd: WorkspaceCommand, json_out: bool) -> Result<()> {
    workspace::run(cmd, json_out)
}

pub fn run_scrollback(cmd: ScrollbackCommand, json_out: bool) -> Result<()> {
    scrollback::run(cmd, json_out)
}

pub fn run_server(cmd: ServerCommand, json_out: bool) -> Result<()> {
    server::run(cmd, json_out)
}

pub fn run_reference(cmd: ReferenceCommand, json_out: bool) -> Result<()> {
    reference::run(cmd, json_out)
}

pub fn run_setup_ai(cmd: SetupAiCommand, json_out: bool) -> Result<()> {
    setup_ai::run(cmd, json_out)
}

pub fn run_mcp_stdio() -> Result<()> {
    mcp_stdio::run()
}

/// Apply the optional `--lang <code>` flag for the lifetime of this process.
pub fn apply_transient_lang(code: Option<&str>) {
    if let Some(c) = code {
        let _ = i18n::set_locale_transient(c);
    }
}

#[cfg(test)]
mod command_line_tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn start_accepts_window_identity_directory_and_program() {
        let parsed = Opt::try_parse_from([
            "unterm-cli",
            "start",
            "--cwd",
            "D:\\work",
            "--profile",
            "work",
            "--",
            "python",
            "-V",
        ])
        .unwrap();

        let SubCommand::Start {
            cwd,
            profile,
            command,
        } = parsed.cmd
        else {
            panic!("expected start command");
        };
        assert_eq!(cwd, Some(std::path::PathBuf::from("D:\\work")));
        assert_eq!(profile.as_deref(), Some("work"));
        assert_eq!(command, ["python", "-V"]);
    }

    /// One spelling for "which pane", everywhere, forever.
    ///
    /// Three used to coexist: `exec` and `session` took `--id`, `scrollback`
    /// took `--pane-id`, `screenshot` took `--pane`. Only `--id` worked on
    /// all of them, so an agent reaching for the clearer `--pane-id` on
    /// `exec` got "unexpected argument" instead of a pane -- a surface whose
    /// whole audience is agents guessing at it.
    ///
    /// Walked rather than enumerated: a list of subcommands is a list to
    /// forget to add to, and the next pane-taking command would drift the
    /// same way this one did.
    #[test]
    fn every_pane_argument_is_spelled_the_same_way() {
        fn walk(cmd: &clap::Command, trail: &str, bad: &mut Vec<String>) {
            let here = if trail.is_empty() {
                cmd.get_name().to_string()
            } else {
                format!("{trail} {}", cmd.get_name())
            };
            for arg in cmd.get_arguments() {
                let Some(long) = arg.get_long() else { continue };
                if long == "id" || long == "pane" {
                    bad.push(format!("`{here} --{long}` should be `--pane-id`"));
                }
                if long == "pane-id" {
                    let aliases = arg.get_all_aliases().unwrap_or_default();
                    if !aliases.contains(&"id") {
                        bad.push(format!(
                            "`{here} --pane-id` dropped the `id` alias that keeps \
                             older invocations working"
                        ));
                    }
                }
            }
            for sub in cmd.get_subcommands() {
                walk(sub, &here, bad);
            }
        }
        let mut bad = Vec::new();
        walk(&Opt::command(), "", &mut bad);
        assert!(bad.is_empty(), "{bad:#?}");
    }

    #[test]
    fn pane_id_alias_is_accepted_for_scrollback_entrypoints() {
        let screenshot =
            Opt::try_parse_from(["unterm-cli", "screenshot", "--scrollback", "--id", "7"]).unwrap();
        let SubCommand::Screenshot { pane, .. } = screenshot.cmd else {
            panic!("expected screenshot command");
        };
        assert_eq!(pane, Some(7));

        let scrollback = Opt::try_parse_from(["unterm-cli", "scrollback", "--id", "7"]).unwrap();
        let SubCommand::Scrollback(cmd) = scrollback.cmd else {
            panic!("expected scrollback command");
        };
        assert_eq!(cmd.pane_id.as_deref(), Some("7"));
    }

    #[test]
    fn actual_cli_exposes_every_required_product_family_and_global_override() {
        let command = Opt::command();
        let names: std::collections::HashSet<_> = command
            .get_subcommands()
            .map(|sub| sub.get_name())
            .collect();
        for required in [
            "start",
            "session",
            "exec",
            "sessions",
            "workspace",
            "instance",
            "screenshot",
            "upload",
            "scrollback",
            "reference",
            "server",
            "setup-ai",
            "mcp-stdio",
            "settings",
            "policy",
            "provider",
            "scope",
            "artifact",
            "evidence",
            "system",
            "proxy",
            "theme",
            "profile",
            "agent",
            "fleet",
            "review",
            "lang",
            "shell-completion",
        ] {
            assert!(names.contains(required), "missing CLI family {required}");
        }
        for retained in [
            "cli",
            "show-keys",
            "ls-fonts",
            "imgcat",
            "set-working-directory",
            "record",
            "replay",
            "ssh",
            "connect",
        ] {
            assert!(
                names.contains(retained),
                "missing retained compatibility CLI family {retained}"
            );
        }
        for global in ["json", "lang", "instance"] {
            assert!(
                command.get_arguments().any(
                    |argument| argument.get_id().as_str() == global && argument.is_global_set()
                ),
                "--{global} is not global"
            );
        }
    }
}
