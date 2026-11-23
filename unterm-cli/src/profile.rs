//! `unterm-cli profile ...` — identity-profile management from the shell.
//!
//! Unlike most `unterm-cli` subcommands (which proxy to the running GUI
//! via MCP), profile management is mostly *direct*: list / create /
//! set-secret / show / delete / audit / edit / export all operate on
//! `~/.unterm/profiles/` and the OS keychain through the
//! `unterm-profile` crate. The CLI works whether or not the GUI is
//! running, which is what a vibe coder expects — "I want to add a new
//! token, I don't want to first open the app to do it."
//!
//! Subcommands that *do* need the GUI (`spawn`, `current`) land
//! alongside the MCP `profile.*` namespace and are wired separately.

use anyhow::{Context, Result};
use clap::{Parser, ValueHint};
use std::io::IsTerminal;
use std::io::Read;
use std::path::PathBuf;
use unterm_profile::{default_store, profile_path, ProfileFile, ProfileRegistry, SecretKey};

#[derive(Debug, Parser, Clone)]
pub struct ProfileCommand {
    #[command(subcommand)]
    pub sub: ProfileSubCommand,
}

#[derive(Debug, Parser, Clone)]
pub enum ProfileSubCommand {
    /// List all profiles in chip-display order.
    List,

    /// Create a new profile from a free-text display name. The internal
    /// ID is derived automatically — you never need to know it.
    Create {
        /// Display name (e.g. "Work — Acme" or "工作"). May contain
        /// spaces, em-dashes, CJK, even emoji — Unterm normalizes
        /// behind the scenes.
        display_name: String,

        /// Accent color as `#RRGGBB`. Used to tint the chip / tab strip /
        /// window border so you can tell identities apart at a glance.
        /// Defaults to emerald (`#10b981`).
        #[arg(long, value_name = "HEX")]
        accent: Option<String>,
    },

    /// Show one profile's details. Secret *values* are never displayed
    /// — only the env var names + keychain references.
    Show {
        /// Display name or ID. Unique-prefix matching is allowed.
        name: String,
    },

    /// Add or rotate a secret in the OS keychain and link it from the
    /// profile's `[secrets]` table.
    SetSecret {
        /// Display name or ID of the profile to attach the secret to.
        profile: String,
        /// Env var name (e.g. `GITHUB_TOKEN`). Will become an env var
        /// of this name in every shell spawned in this profile.
        env_name: String,
        /// Skip the interactive `/dev/tty` prompt and read the value
        /// from stdin instead. Useful for CI / piping from password
        /// managers: `op read 'op://...' | unterm-cli profile set-secret
        /// work GITHUB_TOKEN --from-stdin`.
        #[arg(long)]
        from_stdin: bool,
    },

    /// Delete a profile and clean up its keychain entries.
    Delete {
        /// Display name or ID. Unique-prefix matching is allowed.
        name: String,
        /// Skip the confirmation prompt. Required when not running on a tty.
        #[arg(long, short = 'y')]
        yes: bool,
    },

    /// Surface expiration warnings across all profiles. Prints any
    /// secret whose `[expiration]` date is within 7 days, plus a
    /// summary count of healthy ones.
    Audit,

    /// Open the profile's TOML file in `$EDITOR`.
    Edit {
        /// Display name or ID. Unique-prefix matching is allowed.
        name: String,
    },

    /// Print the profile's full env (including resolved secrets) as a
    /// shell-eval'able script. WARNING: this writes raw token values
    /// to stdout, so don't pipe into a file in a shared directory.
    Export {
        /// Display name or ID. Unique-prefix matching is allowed.
        name: String,
    },

    /// Open a new Unterm window bound to this profile. Resolves to the
    /// matching ID and execs `unterm --profile <id>`, which writes the
    /// binding into the new instance's JSON file before the first pane
    /// spawns. The Unterm window's panes then inherit the profile's
    /// keychain-backed env (UNTERM_PROFILE, GITHUB_TOKEN, GIT_AUTHOR_*, ...).
    Spawn {
        /// Display name or ID of the profile. Unique-prefix matching allowed.
        name: String,
        /// Working directory for the first pane.
        #[arg(long, value_hint = ValueHint::DirPath)]
        cwd: Option<PathBuf>,
    },

    /// Scan the local filesystem for credentials already configured in
    /// `gh` / `aws` / `npm` / `ssh` / `docker` / `gcloud` / `.netrc`
    /// and list them. Read-only — does NOT modify any source files
    /// and does NOT touch the keychain. Pair with `profile create` +
    /// `profile set-secret` to bring discovered credentials under
    /// Unterm management.
    Import {
        /// Restrict to a single source. Default: all 7 sniffers.
        #[arg(long, value_name = "SOURCE",
              value_parser = ["gh","aws","npm","ssh","docker","gcloud","netrc"])]
        source: Option<String>,
    },

    /// Set (or clear) the profile that new Unterm windows bind to
    /// when launched without an explicit `--profile X` flag. Stored
    /// in `~/.unterm/profiles/index.toml`.
    SetDefault {
        /// Display name or ID. Pass `-` to clear the default.
        name: String,
    },

    /// Emit a shell script that prompts before destructive
    /// operations (`gh repo delete`, `aws s3 rb`, `npm unpublish`,
    /// `git push --force`, `vercel rm`) when a profile is bound to
    /// the current window. Source the output from your shell rc:
    ///
    /// ```sh
    /// [[ -n "$UNTERM_PROFILE" ]] && eval "$(unterm-cli profile shell-integration zsh)"
    /// ```
    ///
    /// The script only triggers when `$UNTERM_PROFILE` is set, so
    /// non-Unterm shells (cron, SSH-to-server, ...) are unaffected.
    ShellIntegration {
        /// Shell flavor.
        #[arg(value_parser = ["bash", "zsh", "fish"])]
        shell: String,
    },
}

pub fn run(cmd: ProfileCommand, json_out: bool) -> Result<()> {
    match cmd.sub {
        ProfileSubCommand::List => run_list(json_out),
        ProfileSubCommand::Create {
            display_name,
            accent,
        } => run_create(&display_name, accent.as_deref(), json_out),
        ProfileSubCommand::Show { name } => run_show(&name, json_out),
        ProfileSubCommand::SetSecret {
            profile,
            env_name,
            from_stdin,
        } => run_set_secret(&profile, &env_name, from_stdin),
        ProfileSubCommand::Delete { name, yes } => run_delete(&name, yes),
        ProfileSubCommand::Audit => run_audit(json_out),
        ProfileSubCommand::Edit { name } => run_edit(&name),
        ProfileSubCommand::Export { name } => run_export(&name),
        ProfileSubCommand::Spawn { name, cwd } => run_spawn(&name, cwd),
        ProfileSubCommand::Import { source } => run_import(source.as_deref(), json_out),
        ProfileSubCommand::SetDefault { name } => run_set_default(&name),
        ProfileSubCommand::ShellIntegration { shell } => run_shell_integration(&shell),
    }
}

fn run_shell_integration(shell: &str) -> Result<()> {
    let script = unterm_profile::guard::script_for(shell)?;
    print!("{script}");
    Ok(())
}

fn run_set_default(name: &str) -> Result<()> {
    let mut r = load_registry()?;
    if name == "-" {
        r.set_default(None)?;
        println!("Cleared default profile. New windows will start un-bound.");
        return Ok(());
    }
    let id = resolve_id(&r, name)?;
    r.set_default(Some(id.clone()))?;
    println!("Default profile set to {id:?}");
    println!("New Unterm windows (`unterm` without --profile) will now bind to this profile.");
    Ok(())
}

fn run_import(source: Option<&str>, json_out: bool) -> Result<()> {
    use unterm_profile::sniff;
    let candidates: Vec<sniff::Candidate> = match source {
        Some("gh") => sniff::gh::sniff()?,
        Some("aws") => sniff::aws::sniff()?,
        Some("npm") => sniff::npm::sniff()?,
        Some("ssh") => sniff::ssh::sniff()?,
        Some("docker") => sniff::docker::sniff()?,
        Some("gcloud") => sniff::gcloud::sniff()?,
        Some("netrc") => sniff::netrc::sniff()?,
        None => sniff::scan_all(),
        Some(other) => anyhow::bail!("unknown source: {other}"),
    };

    if json_out {
        println!("{}", serde_json::to_string_pretty(&candidates)?);
        return Ok(());
    }

    if candidates.is_empty() {
        println!("No credentials found in any of the 7 supported sources.");
        println!("Tools we look at:");
        println!("  gh      ~/.config/gh/hosts.yml");
        println!("  aws     ~/.aws/credentials");
        println!("  npm     ~/.npmrc");
        println!("  ssh     ~/.ssh/{{config, id_*}}");
        println!("  docker  ~/.docker/config.json");
        println!("  gcloud  `gcloud config configurations list`");
        println!("  netrc   ~/.netrc");
        return Ok(());
    }

    // Group by source for a tidy listing.
    let mut by_source: std::collections::BTreeMap<&str, Vec<&sniff::Candidate>> =
        std::collections::BTreeMap::new();
    for c in &candidates {
        by_source.entry(c.source.as_str()).or_default().push(c);
    }
    for (src, list) in by_source {
        println!("\n{} ({} found):", src, list.len());
        for c in list {
            let value_hint = match &c.suggested_value {
                Some(_) => "value available",
                None => "needs manual paste",
            };
            let env = if c.suggested_env_name.is_empty() {
                String::from("(no env)")
            } else {
                format!("→ ${}", c.suggested_env_name)
            };
            println!("  {:<40}  {:<32}  [{value_hint}]", c.label, env);
        }
    }
    println!("\nNext: create a profile and import the values you want with");
    println!("  unterm-cli profile create \"My Profile\"");
    println!(
        "  echo <value> | unterm-cli profile set-secret \"My Profile\" <ENV_NAME> --from-stdin"
    );
    Ok(())
}

fn load_registry() -> Result<ProfileRegistry> {
    ProfileRegistry::load().context("load profile registry")
}

fn resolve_id(registry: &ProfileRegistry, name: &str) -> Result<String> {
    registry
        .resolve(name)
        .map(|(id, _)| id.to_string())
        .with_context(|| format!("no profile matches {name:?} (try `unterm-cli profile list`)"))
}

fn run_list(json_out: bool) -> Result<()> {
    let r = load_registry()?;
    if json_out {
        let arr: Vec<_> = r
            .iter_ordered()
            .into_iter()
            .map(|(id, p)| {
                serde_json::json!({
                    "id": id,
                    "display_name": p.display_name,
                    "accent_color": p.accent_color,
                    "description": p.description,
                    "secret_count": p.secrets.len(),
                    "default": r.default_id() == Some(id),
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&serde_json::json!(arr))?);
        return Ok(());
    }

    if r.is_empty() {
        println!("No profiles yet. Create one with:");
        println!("  unterm-cli profile create \"Work — Acme\"");
        return Ok(());
    }

    let default = r.default_id().unwrap_or("");
    for (id, p) in r.iter_ordered() {
        let marker = if id == default { "*" } else { " " };
        let secrets = if p.secrets.is_empty() {
            "no secrets".to_string()
        } else {
            format!("{} secret{}", p.secrets.len(), plural(p.secrets.len()))
        };
        println!("{marker} {:<24}  {}  ({secrets})", p.display_name, dim(id));
    }
    Ok(())
}

fn run_create(display_name: &str, accent: Option<&str>, json_out: bool) -> Result<()> {
    let mut r = load_registry()?;
    let id = r.create(display_name, accent)?;
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "id": id,
                "display_name": display_name,
                "file": profile_path(&id).display().to_string(),
            }))?
        );
    } else {
        println!("Created profile {display_name:?}");
        println!("  ID:    {id}");
        println!("  File:  {}", profile_path(&id).display());
        println!();
        println!("Next: add a secret with");
        println!(
            "  unterm-cli profile set-secret {:?} GITHUB_TOKEN",
            display_name
        );
    }
    Ok(())
}

fn run_show(name: &str, json_out: bool) -> Result<()> {
    let r = load_registry()?;
    let id = resolve_id(&r, name)?;
    let p = r.get(&id).expect("resolved id must be in registry");
    if json_out {
        // ProfileFile derives Serialize via the same skip_serializing_if
        // attributes used for TOML, so going straight to serde_json
        // produces a clean object without the toml crate as a wezterm dep.
        println!("{}", serde_json::to_string_pretty(p)?);
        return Ok(());
    }
    println!("{}", p.display_name);
    println!("  ID:           {id}");
    println!("  Accent:       {}", p.accent_color);
    if !p.description.is_empty() {
        println!("  Description:  {}", p.description);
    }
    if !p.git.is_empty() {
        println!("  Git identity: {} <{}>", p.git.user_name, p.git.user_email);
    }
    if !p.env.is_empty() {
        println!("  Static env:");
        for (k, v) in &p.env {
            println!("    {k}={v}");
        }
    }
    if !p.secrets.is_empty() {
        println!("  Secrets:");
        for (env, url) in &p.secrets {
            // Show env name + masked indicator — never the actual value.
            println!("    {env}  →  {} (keychain)", dim(url));
        }
    }
    if !p.expiration.is_empty() {
        println!("  Expirations:");
        for (env, date) in &p.expiration {
            println!("    {env}  {date}");
        }
    }
    Ok(())
}

fn read_secret_value(env_name: &str, from_stdin: bool) -> Result<String> {
    if from_stdin {
        let mut s = String::new();
        std::io::stdin()
            .read_to_string(&mut s)
            .context("read stdin")?;
        // Strip trailing newline a pipe often adds.
        Ok(s.trim_end_matches(|c| c == '\n' || c == '\r').to_string())
    } else if !std::io::stdin().is_terminal() {
        anyhow::bail!(
            "stdin is not a terminal and --from-stdin wasn't passed. \
             Either run interactively or pipe the value in with --from-stdin."
        )
    } else {
        let prompt = format!("Enter value for {env_name} (input hidden): ");
        rpassword::prompt_password(prompt).context("read hidden secret prompt")
    }
}

fn run_set_secret(profile_arg: &str, env_name: &str, from_stdin: bool) -> Result<()> {
    let mut r = load_registry()?;
    let id = resolve_id(&r, profile_arg)?;
    let store = default_store().context("open OS keychain")?;
    let value = read_secret_value(env_name, from_stdin)?;
    if value.is_empty() {
        anyhow::bail!("refusing to store an empty value for {env_name}");
    }
    r.set_secret(store.as_ref(), &id, env_name, &value)?;
    println!("Stored {env_name} in keychain for profile {id}");
    Ok(())
}

fn run_delete(name: &str, yes: bool) -> Result<()> {
    let r = load_registry()?;
    let id = resolve_id(&r, name)?;
    let p = r.get(&id).expect("resolved id must be in registry").clone();

    if !yes {
        if !std::io::stdin().is_terminal() {
            anyhow::bail!("refuse to delete non-interactively without --yes / -y");
        }
        eprint!(
            "Delete profile {:?} (ID {id}) and {} secret(s) from keychain? [y/N] ",
            p.display_name,
            p.secrets.len()
        );
        let mut line = String::new();
        std::io::stdin()
            .read_line(&mut line)
            .context("read confirmation")?;
        let ans = line.trim().to_lowercase();
        if ans != "y" && ans != "yes" {
            println!("Aborted.");
            return Ok(());
        }
    }

    // Clean keychain entries referenced from [secrets] before removing
    // the TOML. Best-effort: any failure here leaves orphans the user
    // can clear via the OS keychain GUI; we don't want a single failed
    // delete to block removing the profile.
    let store = default_store().context("open OS keychain")?;
    for env_name in p.secrets.keys() {
        let key = SecretKey::new(&id, env_name);
        if let Err(e) = store.delete(&key) {
            eprintln!("  warning: keychain delete failed for {env_name}: {e:#}");
        }
    }
    std::fs::remove_file(profile_path(&id)).context("remove profile TOML")?;
    // Regenerate SSH config so the deleted profile's Match blocks
    // disappear immediately rather than lingering until next startup.
    let reloaded = ProfileRegistry::load().unwrap_or_else(|_| ProfileRegistry::empty());
    if let Err(e) = reloaded.sync_ssh_config() {
        eprintln!("  warning: sync SSH config after delete failed: {e:#}");
    }
    println!("Deleted profile {:?}", p.display_name);
    Ok(())
}

fn run_audit(json_out: bool) -> Result<()> {
    let r = load_registry()?;
    let today = chrono::Local::now().date_naive();
    let mut warnings = Vec::new();
    let mut healthy = 0usize;
    for (id, p) in r.iter_ordered() {
        for (env_name, date_str) in &p.expiration {
            let Ok(date) = chrono::NaiveDate::parse_from_str(date_str, "%Y-%m-%d") else {
                continue;
            };
            let days = (date - today).num_days();
            if days <= 7 {
                warnings.push(serde_json::json!({
                    "profile": id,
                    "display_name": p.display_name,
                    "env_name": env_name,
                    "expires_on": date_str,
                    "days_remaining": days,
                }));
            } else {
                healthy += 1;
            }
        }
    }
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "warnings": warnings,
                "healthy_count": healthy,
            }))?
        );
        return Ok(());
    }
    if warnings.is_empty() {
        println!("No expiring secrets in the next 7 days. {healthy} healthy entries.");
        return Ok(());
    }
    for w in &warnings {
        let days = w["days_remaining"].as_i64().unwrap_or(0);
        let label = if days < 0 {
            format!("EXPIRED {} days ago", -days)
        } else if days == 0 {
            "expires today".to_string()
        } else {
            format!("{days} days left")
        };
        println!(
            "  {:<24}  {:<20}  {} ({})",
            w["display_name"].as_str().unwrap_or(""),
            w["env_name"].as_str().unwrap_or(""),
            w["expires_on"].as_str().unwrap_or(""),
            label
        );
    }
    Ok(())
}

fn run_edit(name: &str) -> Result<()> {
    let r = load_registry()?;
    let id = resolve_id(&r, name)?;
    let path = profile_path(&id);
    let editor = std::env::var("EDITOR")
        .or_else(|_| std::env::var("VISUAL"))
        .unwrap_or_else(|_| {
            // Reasonable default per platform. Vibe coders on macOS who
            // haven't set $EDITOR usually have nano installed.
            if cfg!(windows) {
                "notepad".to_string()
            } else {
                "nano".to_string()
            }
        });
    let status = std::process::Command::new(&editor)
        .arg(&path)
        .status()
        .with_context(|| format!("launch {editor} {}", path.display()))?;
    if !status.success() {
        anyhow::bail!("{editor} exited with {status}");
    }
    // Reload to validate the edit didn't corrupt the file.
    ProfileFile::load(&path)
        .with_context(|| format!("profile {} no longer parses — check syntax", path.display()))?;
    // Edits often add or change [ssh] entries — sync the SSH fragment
    // so the user's `ssh foo` command picks up the new routing without
    // having to restart Unterm.
    let reloaded = ProfileRegistry::load().unwrap_or_else(|_| ProfileRegistry::empty());
    if let Err(e) = reloaded.sync_ssh_config() {
        eprintln!("  warning: sync SSH config after edit failed: {e:#}");
    }
    Ok(())
}

fn run_export(name: &str) -> Result<()> {
    let r = load_registry()?;
    let id = resolve_id(&r, name)?;
    let store = default_store().context("open OS keychain")?;
    let env = r.resolve_env(store.as_ref(), &id)?;
    println!(
        "# Generated by `unterm-cli profile export {id}` at {}.",
        chrono::Local::now().to_rfc3339()
    );
    println!("# These values were read from your OS keychain — keep this file private.");
    let mut keys: Vec<&String> = env.keys().collect();
    keys.sort();
    for k in keys {
        let v = &env[k];
        // POSIX single-quote escape: end-quote, escaped quote, re-open
        // quote. Handles values containing single quotes, newlines,
        // backslashes — anything.
        let escaped = v.replace('\'', "'\\''");
        println!("export {k}='{escaped}'");
    }
    Ok(())
}

fn run_spawn(name: &str, cwd: Option<PathBuf>) -> Result<()> {
    let r = load_registry()?;
    let id = resolve_id(&r, name)?;

    // Locate the `unterm` binary. We're a sibling binary in the same
    // target directory (release artifacts ship them together), so look
    // alongside our own path first. Falls back to bare "unterm" on
    // PATH if the sibling lookup fails — useful in development setups
    // where unterm-cli might be launched via `cargo run` from a
    // workspace that puts artifacts in a different dir.
    let unterm_exe = unterm_protocol::program_path(unterm_protocol::Program::Terminal);

    let mut cmd = std::process::Command::new(&unterm_exe);
    cmd.arg("--profile").arg(&id);
    if let Some(dir) = cwd {
        // `start --cwd` is wezterm-gui's existing convention for
        // setting the first pane's working directory. The flag MUST
        // come *after* the subcommand, hence the explicit `start`.
        cmd.arg("start").arg("--cwd").arg(dir);
    }

    // Spawn detached: we don't want unterm-cli to block on the GUI
    // window's lifetime. On Unix, dropping the Child handle is enough;
    // on Windows the default process creation is also background-safe.
    let child = cmd
        .spawn()
        .with_context(|| format!("exec {} --profile {id}", unterm_exe.display()))?;
    println!(
        "Spawning Unterm window with profile {id:?} (pid {})",
        child.id()
    );
    Ok(())
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// Wrap a string in ANSI dim if stdout is a TTY; otherwise return raw.
fn dim(s: &str) -> String {
    if std::io::stdout().is_terminal() {
        format!("\x1b[2m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}
