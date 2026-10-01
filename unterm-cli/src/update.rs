//! `unterm-cli update` -- install the newest Unterm and restart into it.
//!
//! Downloads this platform's package from the latest GitHub release, checks
//! it (checksum always, code signature on macOS), hands the swap to a helper
//! that waits for Unterm to quit, and then quits Unterm the orderly way, as
//! `unterm-cli quit` does. Typed into an Unterm tab, the whole command is
//! first handed to a detached copy of itself, so it finishes when the tab
//! closes around it.

use anyhow::Result;
use clap::Args;
use std::io::Write;
use unterm_services::updater::{self, Outcome};

#[derive(Debug, Args, Clone)]
pub struct UpdateCommand {
    /// Only report whether a newer version exists; change nothing.
    #[arg(long)]
    pub check: bool,

    /// The install to update, when it is not the one this unterm-cli belongs
    /// to: the `.app` bundle on macOS, the install folder on Windows.
    #[arg(long, value_name = "path")]
    pub app: Option<std::path::PathBuf>,

    /// Install the latest release even when it is not newer.
    #[arg(long)]
    pub force: bool,

    /// Do not start Unterm again afterwards.
    #[arg(long = "no-restart")]
    pub no_restart: bool,
}

/// The executable that stands for the install being updated, and its version.
fn target(cmd: &UpdateCommand) -> Result<(std::path::PathBuf, String)> {
    let Some(app) = &cmd.app else {
        return Ok((
            std::env::current_exe()?,
            unterm_protocol::PRODUCT_VERSION.to_string(),
        ));
    };
    let exe = if app.extension().is_some_and(|ext| ext == "app") {
        app.join("Contents").join("MacOS").join("unterm-cli")
    } else if app.is_dir() {
        app.join(if cfg!(windows) { "unterm-cli.exe" } else { "unterm-cli" })
    } else {
        app.clone()
    };
    // Its own answer to --version, rather than ours: the point of --app is
    // updating an install other than the one running this command.
    let output = std::process::Command::new(&exe).arg("--version").output()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let version = text
        .split_whitespace()
        .last()
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("{} did not say its version", exe.display()))?;
    Ok((exe, version))
}

pub fn run(cmd: UpdateCommand, json_out: bool) -> Result<()> {
    let (exe, current) = target(&cmd)?;

    if cmd.check {
        let release = updater::latest()?;
        let newer = updater::is_newer(&release.tag, &current);
        let install = updater::install_of(&exe);
        if json_out {
            crate::output::print_json(&serde_json::json!({
                "current": current,
                "latest": release.tag.trim_start_matches('v'),
                "update_available": newer,
                "install": install,
                "package": updater::asset_for(&release, &install).map(|asset| &asset.name),
                "url": release.url,
            }));
        } else if newer {
            println!("{} is available (this is {current}): {}", release.tag, release.url);
        } else {
            println!("Unterm {current} is the latest");
        }
        return Ok(());
    }

    // Inside an Unterm tab, the quit below would close this terminal under
    // us: let a detached copy do all of it.
    if !crate::quit::handed_off() {
        let install = updater::install_of(&exe);
        if crate::quit::runs_inside(&updater::processes_of(&install)) {
            return crate::quit::hand_off(json_out);
        }
    }

    let mut last_percent = None;
    let outcome = updater::update(&exe, &current, cmd.force, !cmd.no_restart, |done, total| {
        if json_out || total == 0 {
            return;
        }
        let percent = done * 100 / total;
        if last_percent != Some(percent) {
            last_percent = Some(percent);
            eprint!("\rdownloading {percent}%");
            let _ = std::io::stderr().flush();
        }
    })?;
    if last_percent.is_some() {
        eprintln!();
    }
    match &outcome {
        Outcome::UpToDate { current, .. } => {
            if json_out {
                crate::output::print_json(&serde_json::to_value(&outcome)?);
            } else {
                println!("Unterm {current} is the latest");
            }
            Ok(())
        }
        Outcome::Manual { latest, reason, url, .. } => {
            if json_out {
                crate::output::print_json(&serde_json::to_value(&outcome)?);
            } else {
                println!("Unterm {latest} is available, but {reason}: {url}");
            }
            Ok(())
        }
        Outcome::Scheduled {
            latest,
            scheduled,
            waiting_for,
            ..
        } => {
            if json_out {
                crate::output::print_json(&serde_json::to_value(&outcome)?);
            } else {
                println!("Unterm {latest} downloaded and verified; {}", scheduled.summary);
            }
            if waiting_for.is_empty() {
                return Ok(());
            }
            // Quit the windows and the Core of that install, the orderly way.
            crate::quit::quit_processes(waiting_for, json_out)
        }
    }
}
