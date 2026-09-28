use anyhow::Result;
use unterm_core::lifetime::{self, CoreMode};
use unterm_core::{clear_discovery, try_acquire_instance_lock, write_discovery, CoreServer};

fn main() -> Result<()> {
    let args = CoreArgs::parse(std::env::args_os().skip(1))?;
    if args.version {
        println!("unterm-core {}", unterm_protocol::PRODUCT_VERSION);
        return Ok(());
    }
    if args.help {
        print_help();
        return Ok(());
    }

    // Single-instance gate: concurrent GUI/CLI launches may race to
    // spawn a core; only the lock holder may bind and publish
    // discovery. Losers exit quietly and their parent keeps polling
    // the winner's discovery record.
    let Some(_lock) = try_acquire_instance_lock()? else {
        eprintln!("unterm-core already running for this user; exiting");
        return Ok(());
    };
    // Whose Core this is decides when it ends; see `lifetime`. A window
    // that started us passes its pid, and we hold a handle on it so a
    // recycled pid cannot keep us alive.
    let mode = if args.headless || args.gui_pid.is_none() {
        CoreMode::Headless
    } else {
        CoreMode::GuiOwned
    };
    lifetime::set_mode(mode);
    let spawner = args
        .gui_pid
        .and_then(unterm_services::process_lifetime::ProcessWatch::open);

    // Every session's process tree -- conpty host, shell, and what the shell
    // started -- joins this job by inheritance, and the kernel kills the job
    // when this process ends however it ends. Before this a killed Core left
    // OpenConsole and every shell running with nothing to show them.
    static SESSION_JOB: std::sync::OnceLock<unterm_services::process_lifetime::SessionJob> =
        std::sync::OnceLock::new();
    let session_job = match unterm_services::process_lifetime::SessionJob::adopt_current_process() {
        Ok(job) => {
            let job = SESSION_JOB.get_or_init(|| job);
            // A shell Windows starts in a job of its own (a Store app) does
            // not inherit ours; this catches it as it is spawned.
            #[cfg(windows)]
            portable_pty::win::set_spawn_observer(move |process| job.contain(process));
            Some(job)
        }
        Err(err) => {
            if cfg!(windows) {
                eprintln!(
                    "unterm-core: could not create the session job ({err}); \
                     sessions will not end if this process is killed"
                );
            }
            None
        }
    };

    let token =
        std::env::var("UNTERM_CORE_TOKEN").unwrap_or_else(|_| uuid::Uuid::new_v4().to_string());
    let server = CoreServer::bind(("127.0.0.1", 0), &token)?;
    let endpoint = server.endpoint()?;

    // The agent surface enforces the user's policy — trusted agents,
    // confirmation mode, scrollback size. A Core that skipped the
    // config would judge every write by defaults instead of by what
    // the user chose in it.
    let (config, config_errors) = unterm_services::settings::load(None);
    for error in &config_errors {
        eprintln!("unterm-core: config line {}: {}", error.line, error.message);
    }
    unterm_services::settings::set_current(&config);
    unterm_engine::next_core::NextCoreEngine::set_new_session_scrollback_lines(
        unterm_services::settings::scrollback_lines(&config),
    );

    // The agent surface lives here, not in any GUI: sessions belong to
    // this process, so the 103-method MCP server drives the local
    // engine directly. Discovery via our own record; a GUI's MCP
    // server (transitional) keeps server.json to itself.
    unterm_engine::install_next_core_provider();
    // Register the window-facing half of the surface before the MCP
    // server opens. It answers "is there a window?" with the truth at
    // the moment it is asked -- no window attached and it degrades
    // exactly as a headless surface always did, so this is safe to
    // install whether or not a GUI ever shows up.
    unterm_engine::set_mcp_host(&unterm_core::RemoteMcpHost);
    let mcp_port = match unterm_mcp::start_headless_mcp_server(&token) {
        Ok(port) => Some(port),
        Err(err) => {
            eprintln!("unterm-core: MCP surface unavailable: {err:#}");
            None
        }
    };
    match unterm_services::bridge_registry::request_incompatible_drains() {
        Ok(0) | Err(_) => {}
        Ok(count) => eprintln!("unterm-core: requested drain for {count} incompatible bridge(s)"),
    }

    // Two installs share one state directory: whichever starts first owns the
    // sessions, and two versions can migrate the same database differently.
    // Said at startup, once, because it explains a class of failure nobody
    // would otherwise connect to having installed Unterm twice.
    let installs = unterm_services::install::survey();
    for conflict in unterm_services::install::conflicts(&installs) {
        eprintln!("unterm-core: {} — {}", conflict.reason, conflict.advice);
    }

    write_discovery(&endpoint.to_string(), &token, mcp_port, server.started_at())?;
    eprintln!(
        "unterm-core ready endpoint={} mcp_port={:?} pid={}",
        endpoint,
        mcp_port,
        std::process::id()
    );
    // The system will tell us before it takes this process away — SIGTERM on
    // macOS and Linux, a console control event on Windows. The Core is where
    // that has to be heard: it owns the sessions and the task store, so it is
    // the process whose sudden death costs something.
    unterm_services::power::install();
    std::thread::Builder::new()
        .name("core-power-watch".into())
        .spawn(|| loop {
            if let Some(reason) = unterm_services::power::should_stop() {
                // Seconds, not minutes. Say why we are going, take the
                // discovery record with us so nobody connects to a corpse,
                // and leave. Finishing work here is how a process gets killed
                // halfway through finishing it.
                eprintln!("unterm-core stopping: {reason}");
                let _ = clear_discovery();
                std::process::exit(0);
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        })
        .ok();

    if mode == CoreMode::GuiOwned {
        let running = server.running_flag();
        let draining_to_exit = server.exit_when_idle_flag();
        std::thread::Builder::new()
            .name("core-orphan-watchdog".into())
            .spawn(move || {
                let mut watchdog = lifetime::Watchdog::new();
                while running.load(std::sync::atomic::Ordering::Acquire) {
                    let front_end_alive = !unterm_core::host_channel().attached_pids().is_empty()
                        || spawner.as_ref().is_some_and(|watch| watch.is_alive());
                    let verdict = watchdog.tick(
                        std::time::Instant::now(),
                        lifetime::WatchdogInput {
                            mode,
                            front_end_alive,
                            draining_to_exit: draining_to_exit
                                .load(std::sync::atomic::Ordering::Acquire),
                        },
                    );
                    if let lifetime::Verdict::Exit(reason) = verdict {
                        eprintln!("unterm-core stopping: {reason}");
                        running.store(false, std::sync::atomic::Ordering::Release);
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_secs(1));
                }
            })
            .ok();
    }

    let result = server.run();
    // However `run` ended -- `core.shutdown`, a finished drain, the orphan
    // watchdog -- the sessions end with it, the ordinary way.
    unterm_core::destroy_all_sessions();
    let _ = clear_discovery();
    // An orderly exit: every session has been closed. What is left in the job
    // is what the user launched from a shell to keep (an editor window), and
    // quitting the terminal must not take it along.
    if let Some(job) = session_job {
        job.disarm();
    }
    result
}

struct CoreArgs {
    help: bool,
    version: bool,
    headless: bool,
    /// The window that spawned this Core, if one did.
    gui_pid: Option<u32>,
}

impl CoreArgs {
    fn parse(arguments: impl IntoIterator<Item = std::ffi::OsString>) -> Result<Self> {
        let mut parsed = Self {
            help: false,
            version: false,
            headless: false,
            gui_pid: None,
        };
        let mut arguments = arguments.into_iter();
        while let Some(argument) = arguments.next() {
            match argument.to_string_lossy().as_ref() {
                "--help" | "-h" => parsed.help = true,
                "--version" | "-V" => parsed.version = true,
                "--headless" => parsed.headless = true,
                "--gui-pid" => {
                    let value = arguments
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--gui-pid needs a process id"))?;
                    parsed.gui_pid = Some(
                        value
                            .to_string_lossy()
                            .parse()
                            .map_err(|_| anyhow::anyhow!("--gui-pid {value:?} is not a pid"))?,
                    );
                }
                other => anyhow::bail!("unknown unterm-core argument {other:?}"),
            }
        }
        Ok(parsed)
    }
}

fn print_help() {
    println!(
        "unterm-core {}\n\nUSAGE:\n    unterm-core [--headless | --gui-pid <pid>]\n\nOPTIONS:\n    --headless    Run without any window, until told to stop (the default)\n    --gui-pid <pid>\n                  Started by that Unterm window: exit once no window has been\n                  attached for 30s\n    -V, --version Print version and exit before initialization\n    -h, --help    Print help and exit before initialization",
        unterm_protocol::PRODUCT_VERSION
    );
}
