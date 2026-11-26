//! `unterm-cli remote-core` — attach to a remote `unterm-core --headless` over SSH.
//!
//! Spawns a local TCP port-forwarding tunnel (`ssh -N -L`) with keepalives,
//! optionally discovers the remote Core's port and token from its `core.json`,
//! and records the active tunnel under `~/.unterm/remote-cores/<name>.json`.

use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::process::{Command, Stdio};

#[derive(Parser, Debug, Clone)]
pub struct RemoteCoreCommand {
    #[command(subcommand)]
    pub action: RemoteCoreAction,
}

#[derive(Subcommand, Debug, Clone)]
pub enum RemoteCoreAction {
    /// Attach to a remote `unterm-core --headless` via an SSH local port-forward tunnel.
    Attach {
        /// SSH destination (`user@host` or SSH config alias).
        target: String,
        /// Optional friendly name for this remote Core tunnel (defaults to host).
        #[arg(long)]
        name: Option<String>,
        /// Optional SSH port (`-p`).
        #[arg(long)]
        ssh_port: Option<u16>,
        /// Remote `unterm-core` TCP port (auto-discovered from remote `core.json` if omitted).
        #[arg(long)]
        remote_port: Option<u16>,
        /// Local forwarded TCP port (picks an ephemeral free port if omitted).
        #[arg(long)]
        local_port: Option<u16>,
        /// Remote Core auth token (auto-discovered from remote `core.json` if omitted).
        #[arg(long)]
        token: Option<String>,
    },
    /// List recorded Remote Core SSH tunnels.
    List,
    /// Detach and stop a recorded Remote Core SSH tunnel.
    Detach {
        /// Tunnel name as shown by `unterm-cli remote-core list`.
        name: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteCoreRecord {
    pub name: String,
    pub ssh_target: String,
    pub ssh_port: Option<u16>,
    pub local_port: u16,
    pub remote_port: u16,
    pub endpoint: String,
    pub token: String,
    pub tunnel_pid: u32,
}

#[derive(Debug, Clone, Deserialize)]
struct RemoteCoreJson {
    endpoint: String,
    token: String,
}

fn records_dir() -> Result<PathBuf> {
    let base = unterm_protocol::state_dir()
        .ok_or_else(|| anyhow!("could not determine Unterm state directory"))?;
    Ok(base.join("remote-cores"))
}

fn sanitize_name(raw: &str) -> String {
    let host = raw.rsplit_once('@').map(|(_, h)| h).unwrap_or(raw);
    let cleaned: String = host
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "remote".to_string()
    } else {
        cleaned
    }
}

fn pick_local_port(requested: Option<u16>) -> Result<u16> {
    if let Some(port) = requested {
        return Ok(port);
    }
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .context("failed to allocate local ephemeral port for SSH tunnel")?;
    Ok(listener.local_addr()?.port())
}

pub fn parse_remote_core_json(raw: &str) -> Result<(u16, String)> {
    let parsed: RemoteCoreJson =
        serde_json::from_str(raw.trim()).context("invalid remote core.json payload")?;
    let addr: SocketAddr = parsed
        .endpoint
        .parse()
        .with_context(|| format!("invalid remote core endpoint: {}", parsed.endpoint))?;
    if parsed.token.trim().is_empty() {
        return Err(anyhow!("remote core.json contained an empty token"));
    }
    Ok((addr.port(), parsed.token))
}

pub fn build_ssh_tunnel_args(
    target: &str,
    ssh_port: Option<u16>,
    local_port: u16,
    remote_port: u16,
) -> Vec<String> {
    let mut args = vec![
        "-N".to_string(),
        "-o".to_string(),
        "ExitOnForwardFailure=yes".to_string(),
        "-o".to_string(),
        "ServerAliveInterval=15".to_string(),
        "-o".to_string(),
        "ServerAliveCountMax=3".to_string(),
        "-L".to_string(),
        format!("127.0.0.1:{local_port}:127.0.0.1:{remote_port}"),
    ];
    if let Some(port) = ssh_port {
        args.push("-p".to_string());
        args.push(port.to_string());
    }
    args.push(target.to_string());
    args
}

fn hidden_command(program: &str) -> Command {
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    cmd
}

fn probe_remote_core(target: &str, ssh_port: Option<u16>) -> Result<(u16, String)> {
    let mut cmd = hidden_command("ssh");
    if let Some(port) = ssh_port {
        cmd.arg("-p").arg(port.to_string());
    }
    // Check Linux XDG, macOS Application Support, and ~/.unterm fallback.
    let probe_script = concat!(
        "for p in ",
        "\"${XDG_DATA_HOME:-$HOME/.local/share}/unterm/core.json\" ",
        "\"$HOME/Library/Application Support/Unterm/core.json\" ",
        "\"$HOME/.unterm/core.json\"; ",
        "do if [ -f \"$p\" ]; then cat \"$p\"; exit 0; fi; done; exit 1"
    );
    let out = cmd
        .arg(target)
        .arg(probe_script)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("failed to run ssh probe against {target}"))?;
    if !out.status.success() {
        return Err(anyhow!(
            "could not find remote core.json on {target} (is `unterm-core --headless` running?)"
        ));
    }
    let raw = String::from_utf8_lossy(&out.stdout);
    parse_remote_core_json(&raw)
}

pub fn run(cmd: RemoteCoreCommand, json_out: bool) -> Result<()> {
    match cmd.action {
        RemoteCoreAction::Attach {
            target,
            name,
            ssh_port,
            remote_port,
            local_port,
            token,
        } => {
            let (remote_port, token) = match (remote_port, token) {
                (Some(p), Some(t)) => (p, t),
                (rp, tk) => {
                    let (probed_port, probed_token) = probe_remote_core(&target, ssh_port)?;
                    (rp.unwrap_or(probed_port), tk.unwrap_or(probed_token))
                }
            };
            let local_port = pick_local_port(local_port)?;
            let name = sanitize_name(name.as_deref().unwrap_or(&target));
            let ssh_args = build_ssh_tunnel_args(&target, ssh_port, local_port, remote_port);
            let child = hidden_command("ssh")
                .args(&ssh_args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .with_context(|| format!("failed to start SSH tunnel to {target}"))?;

            let record = RemoteCoreRecord {
                name: name.clone(),
                ssh_target: target,
                ssh_port,
                local_port,
                remote_port,
                endpoint: format!("127.0.0.1:{local_port}"),
                token,
                tunnel_pid: child.id(),
            };
            let dir = records_dir()?;
            std::fs::create_dir_all(&dir)?;
            let path = dir.join(format!("{name}.json"));
            std::fs::write(&path, serde_json::to_string_pretty(&record)?)?;

            if json_out {
                println!("{}", serde_json::to_string_pretty(&record)?);
            } else {
                println!(
                    "Attached remote Core '{}' -> {} (ssh pid {})",
                    record.name, record.endpoint, record.tunnel_pid
                );
            }
            Ok(())
        }
        RemoteCoreAction::List => {
            let dir = records_dir()?;
            let mut records = Vec::new();
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    if entry.path().extension().and_then(|e| e.to_str()) != Some("json") {
                        continue;
                    }
                    if let Ok(raw) = std::fs::read_to_string(entry.path()) {
                        if let Ok(rec) = serde_json::from_str::<RemoteCoreRecord>(&raw) {
                            records.push(rec);
                        }
                    }
                }
            }
            records.sort_by(|a, b| a.name.cmp(&b.name));
            if json_out {
                println!("{}", serde_json::to_string_pretty(&records)?);
            } else if records.is_empty() {
                println!("No active Remote Core SSH tunnels.");
            } else {
                for rec in records {
                    println!(
                        "{}\t{}\t{}\tpid={}",
                        rec.name, rec.ssh_target, rec.endpoint, rec.tunnel_pid
                    );
                }
            }
            Ok(())
        }
        RemoteCoreAction::Detach { name } => {
            let dir = records_dir()?;
            let path = dir.join(format!("{}.json", sanitize_name(&name)));
            if !path.is_file() {
                return Err(anyhow!("no remote Core tunnel named '{name}'"));
            }
            if let Ok(raw) = std::fs::read_to_string(&path) {
                if let Ok(rec) = serde_json::from_str::<RemoteCoreRecord>(&raw) {
                    #[cfg(unix)]
                    let _ = Command::new("kill")
                        .arg(rec.tunnel_pid.to_string())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status();
                    #[cfg(windows)]
                    let _ = hidden_command("taskkill")
                        .args(["/F", "/PID", &rec.tunnel_pid.to_string()])
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status();
                }
            }
            let _ = std::fs::remove_file(&path);
            if json_out {
                println!("{}", serde_json::json!({"ok": true, "detached": name}));
            } else {
                println!("Detached remote Core tunnel '{name}'.");
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_keepalive_ssh_tunnel_args() {
        let args = build_ssh_tunnel_args("ubuntu@gpu-01", Some(2222), 29876, 19876);
        assert!(args.contains(&"-N".to_string()));
        assert!(args.contains(&"ExitOnForwardFailure=yes".to_string()));
        assert!(args.contains(&"127.0.0.1:29876:127.0.0.1:19876".to_string()));
        assert!(args.contains(&"-p".to_string()));
        assert!(args.contains(&"2222".to_string()));
        assert_eq!(args.last().map(String::as_str), Some("ubuntu@gpu-01"));
    }

    #[test]
    fn parses_remote_core_json_endpoint_and_token() {
        let raw = r#"{"endpoint":"127.0.0.1:19876","token":"secret-tok","pid":4242}"#;
        let (port, token) = parse_remote_core_json(raw).unwrap();
        assert_eq!(port, 19876);
        assert_eq!(token, "secret-tok");
    }

    #[test]
    fn sanitizes_remote_tunnel_names() {
        assert_eq!(sanitize_name("ubuntu@gpu-01.internal"), "gpu-01.internal");
        assert_eq!(sanitize_name("my/bad:name"), "my-bad-name");
    }
}
