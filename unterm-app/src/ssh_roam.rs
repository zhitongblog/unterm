//! SSH Remote Agent Roaming — detect remote host, remote working directory,
//! and remote AI agent sessions across SSH connections.
//!
//! When a user connects to a remote Linux/GPU box over `ssh`, local `OSC 7`
//! either stops updating or reports the local home folder, while the remote
//! shell sets prompt titles like `ubuntu@gpu-01: ~/code/llm-train` or runs
//! `claude` / `codex` / `python train.py` remotely. This module extracts that
//! remote context so the sidebar displays `🌐 gpu-01 · ~/code/llm-train` and
//! offers 1-click "Clone SSH Session in Same Remote Directory".

/// Parsed remote SSH session metadata for a pane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteSession {
    /// Full `[user@]host` target for `ssh`.
    pub target: String,
    /// Short host name for sidebar badge (`gpu-01`).
    pub short_host: String,
    /// Optional port (`-p 2222`).
    pub port: Option<u16>,
    /// Remote working directory inferred from the terminal title (`~/code/llm`).
    pub remote_cwd: Option<String>,
}

impl RemoteSession {
    /// Compact subtitle for the sidebar (`🌐 gpu-01 · ~/code/llm`).
    pub fn sidebar_subtitle(&self) -> String {
        match &self.remote_cwd {
            Some(cwd) if !cwd.is_empty() => format!("🌐 {} · {}", self.short_host, cwd),
            _ => format!("🌐 {}", self.short_host),
        }
    }

    /// Build a shell command that opens a new SSH connection to the same host
    /// and enters the same remote directory.
    pub fn clone_command(&self) -> String {
        let mut cmd = String::from("ssh");
        if let Some(port) = self.port {
            cmd.push_str(&format!(" -p {port}"));
        }
        if let Some(cwd) = &self.remote_cwd {
            let safe_cwd = cwd.replace('\'', "'\\''");
            cmd.push_str(&format!(
                " -t {} 'cd {} 2>/dev/null; exec $SHELL -l'",
                self.target, safe_cwd
            ));
        } else {
            cmd.push_str(&format!(" {}", self.target));
        }
        cmd
    }
}

/// Detect whether a pane is running an SSH session from its foreground process,
/// command line, or prompt title (`user@host: /path`).
pub fn detect(process_name: &str, pane_title: &str) -> Option<RemoteSession> {
    let proc_lower = process_name.trim().to_ascii_lowercase();
    let is_ssh_proc = proc_lower == "ssh"
        || proc_lower.ends_with("/ssh")
        || proc_lower.ends_with("\\ssh.exe")
        || proc_lower.starts_with("ssh ");

    // 1. Try parsing `user@host: /remote/path` from the pane title.
    if let Some((target, cwd)) = parse_user_host_title(pane_title) {
        let short_host = target
            .rsplit_once('@')
            .map(|(_, h)| h.to_string())
            .unwrap_or_else(|| target.clone());
        return Some(RemoteSession {
            target,
            short_host,
            port: None,
            remote_cwd: cwd,
        });
    }

    // 2. Try parsing `ssh [-p PORT] [user@]host` from the pane title or process name.
    for candidate in [pane_title, process_name] {
        if let Some(session) = parse_ssh_command(candidate) {
            return Some(session);
        }
    }

    if is_ssh_proc {
        Some(RemoteSession {
            target: "remote".to_string(),
            short_host: "ssh".to_string(),
            port: None,
            remote_cwd: None,
        })
    } else {
        None
    }
}

fn parse_user_host_title(title: &str) -> Option<(String, Option<String>)> {
    let trimmed = title.trim();
    // Strip leading "ssh " if present
    let trimmed = trimmed.strip_prefix("ssh ").unwrap_or(trimmed).trim();
    let (who, rest) = trimmed.split_once(':')?;
    let (user, host) = who.trim().split_once('@')?;
    let valid_token = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    if !valid_token(user) || !valid_token(host) {
        return None;
    }
    let cwd = rest.trim();
    let remote_cwd = (!cwd.is_empty() && (cwd.starts_with('~') || cwd.starts_with('/')))
        .then(|| cwd.to_string());
    Some((format!("{user}@{host}"), remote_cwd))
}

pub fn parse_ssh_command(cmd: &str) -> Option<RemoteSession> {
    let words: Vec<&str> = cmd.split_whitespace().collect();
    let first = *words.first()?;
    if first != "ssh" && !first.ends_with("/ssh") && !first.ends_with("\\ssh.exe") {
        return None;
    }
    let mut port: Option<u16> = None;
    let mut target: Option<String> = None;
    let mut i = 1;
    while i < words.len() {
        let w = words[i];
        if w == "-p" {
            if let Some(p) = words.get(i + 1).and_then(|s| s.parse::<u16>().ok()) {
                port = Some(p);
            }
            i += 2;
            continue;
        }
        if matches!(w, "-i" | "-l" | "-J" | "-o" | "-L" | "-R" | "-D" | "-F") {
            i += 2;
            continue;
        }
        if w.starts_with('-') {
            i += 1;
            continue;
        }
        if target.is_none() {
            target = Some(w.to_string());
        }
        i += 1;
    }
    let target = target?;
    let short_host = target
        .rsplit_once('@')
        .map(|(_, h)| h.to_string())
        .unwrap_or_else(|| target.clone());
    Some(RemoteSession {
        target,
        short_host,
        port,
        remote_cwd: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_user_at_host_prompt_title_with_remote_cwd() {
        let session = detect("ssh", "ubuntu@gpu-node-02: ~/workspace/deepseek").unwrap();
        assert_eq!(session.target, "ubuntu@gpu-node-02");
        assert_eq!(session.short_host, "gpu-node-02");
        assert_eq!(session.remote_cwd.as_deref(), Some("~/workspace/deepseek"));
        assert_eq!(
            session.sidebar_subtitle(),
            "🌐 gpu-node-02 · ~/workspace/deepseek"
        );
        assert!(session
            .clone_command()
            .contains("ssh -t ubuntu@gpu-node-02 'cd ~/workspace/deepseek"));
    }

    #[test]
    fn parses_ssh_command_with_port_and_flags() {
        let session = parse_ssh_command("ssh -p 2202 -i ~/.ssh/id_ed25519 root@10.0.4.12").unwrap();
        assert_eq!(session.target, "root@10.0.4.12");
        assert_eq!(session.short_host, "10.0.4.12");
        assert_eq!(session.port, Some(2202));
        assert_eq!(session.clone_command(), "ssh -p 2202 root@10.0.4.12");
    }
}
