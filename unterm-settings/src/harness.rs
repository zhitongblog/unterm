//! T3 Code-inspired Agent Harness capabilities for Unterm:
//! 1. Scoped remote permissions (`full`, `approve`, `readonly`) + Multi-route metadata.
//! 2. Local Dev Server Port Discovery (`discover_listening_ports`) + Localhost Preview Proxy (`/preview/<port>/...`).
//! 3. Zero-pollution Hidden Git Ref Checkpoints (`refs/unterm/checkpoints/<id>`) with diff & rollback.
//! 4. Cross-Agent Portable Handoffs (`build_portable_handoff` / `execute_portable_handoff`).
//! 5. Window SnapShot + UI Accessibility (A11y) Control Tree capture (`capture_window_with_a11y`).

use anyhow::{anyhow, Result};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn silent_command(program: &str) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

// ============================================================================
// 1. Scoped Permissions (`full`, `approve`, `readonly`)
// ============================================================================

pub fn normalize_scope(scope: Option<&str>) -> String {
    match scope.unwrap_or("full").trim().to_ascii_lowercase().as_str() {
        "readonly" | "read-only" | "watch" => "readonly".to_string(),
        "approve" | "steer" | "supervised" => "approve".to_string(),
        _ => "full".to_string(),
    }
}

pub fn scope_label(scope: &str) -> &'static str {
    match scope {
        "readonly" => "只读监看 (Read-Only)",
        "approve" => "仅审批与对话 (Approve & Steer)",
        _ => "完全控制 (Full Control)",
    }
}

/// Checks whether a paired device with `scope` is allowed to invoke `method`.
pub fn is_method_allowed_for_scope(scope: &str, method: &str) -> bool {
    let is_read = matches!(
        method,
        "instance.list"
            | "instance.info"
            | "agent.status"
            | "cockpit.inbox"
            | "session.list"
            | "session.cwd"
            | "session.idle"
            | "screen.text"
            | "screen.read"
            | "screen.scrollback_text"
            | "screen.cursor"
            | "screen.scroll"
            | "approval.list"
            | "review.list"
            | "review.diff"
            | "ports.list"
            | "checkpoint.list"
            | "checkpoint.diff"
    );
    if is_read {
        return true;
    }

    match normalize_scope(Some(scope)).as_str() {
        "readonly" => false,
        "approve" => matches!(
            method,
            "approval.decide"
                | "session.focus"
                | "session.paste"
                | "session.input"
                | "checkpoint.create"
                | "handoff.build"
        ),
        _ => matches!(
            method,
            "approval.decide"
                | "session.focus"
                | "session.input"
                | "session.paste"
                | "session.resize"
                | "signal.send"
                | "checkpoint.create"
                | "checkpoint.restore"
                | "handoff.build"
                | "handoff.execute"
                | "snapshot.capture"
        ),
    }
}

// ============================================================================
// 2. Local Dev Server Port Discovery + Reverse Proxy
// ============================================================================

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DiscoveredPort {
    pub port: u16,
    pub pid: u32,
    pub bind_addr: String,
    pub label: String,
    pub local_url: String,
    pub preview_path: String,
}

static PORT_CACHE: OnceLock<Mutex<Option<(Instant, Vec<DiscoveredPort>)>>> = OnceLock::new();

fn port_cache() -> &'static Mutex<Option<(Instant, Vec<DiscoveredPort>)>> {
    PORT_CACHE.get_or_init(|| Mutex::new(None))
}

/// Well-known web / dev-server port ranges we surface for one-tap preview.
pub fn is_likely_dev_port(port: u16, excluded_ports: &[u16]) -> bool {
    if excluded_ports.contains(&port) {
        return false;
    }
    matches!(
        port,
        80
            | 3000..=3999
            | 4000..=4999
            | 5000..=5999
            | 6006
            | 7000..=7999
            | 8000..=8999
            | 9000..=9099
            | 1313
    )
}

pub fn discover_listening_ports(excluded_ports: &[u16]) -> Vec<DiscoveredPort> {
    {
        let guard = port_cache().lock();
        if let Some((ts, cached)) = guard.as_ref() {
            if ts.elapsed() < Duration::from_secs(3) {
                return cached
                    .iter()
                    .filter(|p| !excluded_ports.contains(&p.port))
                    .cloned()
                    .collect();
            }
        }
    }

    let discovered = scan_listening_ports_once(excluded_ports);
    *port_cache().lock() = Some((Instant::now(), discovered.clone()));
    discovered
}

fn scan_listening_ports_once(excluded_ports: &[u16]) -> Vec<DiscoveredPort> {
    let mut out: Vec<DiscoveredPort> = Vec::new();
    let mut seen_ports = std::collections::BTreeSet::new();

    #[cfg(windows)]
    {
        if let Ok(output) = silent_command("netstat").args(["-ano", "-p", "tcp"]).output() {
            let text = String::from_utf8_lossy(&output.stdout);
            for line in text.lines() {
                let parts: Vec<&str> = line.split_whitespace().collect();
                // Proto Local-Address Foreign-Address State PID
                if parts.len() < 5 || !parts[3].eq_ignore_ascii_case("LISTENING") {
                    continue;
                }
                let local = parts[1];
                let pid: u32 = parts[4].parse().unwrap_or(0);
                if pid <= 4 {
                    continue;
                }
                let Some((addr_part, port_str)) = local.rsplit_once(':') else {
                    continue;
                };
                let Ok(port) = port_str.parse::<u16>() else {
                    continue;
                };
                if !is_likely_dev_port(port, excluded_ports) || !seen_ports.insert(port) {
                    continue;
                }
                let label = guess_port_label(port);
                out.push(DiscoveredPort {
                    port,
                    pid,
                    bind_addr: addr_part.to_string(),
                    label,
                    local_url: format!("http://127.0.0.1:{}/", port),
                    preview_path: format!("/preview/{}/", port),
                });
            }
        }
    }

    #[cfg(not(windows))]
    {
        if let Ok(output) = silent_command("lsof")
            .args(["-iTCP", "-sTCP:LISTEN", "-P", "-n"])
            .output()
        {
            let text = String::from_utf8_lossy(&output.stdout);
            for line in text.lines().skip(1) {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() < 9 {
                    continue;
                }
                let proc_name = parts[0];
                let pid: u32 = parts[1].parse().unwrap_or(0);
                let name_col = parts[8];
                let Some((addr_part, port_str)) = name_col.rsplit_once(':') else {
                    continue;
                };
                let Ok(port) = port_str.parse::<u16>() else {
                    continue;
                };
                if !is_likely_dev_port(port, excluded_ports) || !seen_ports.insert(port) {
                    continue;
                }
                let label = if proc_name.is_empty() {
                    guess_port_label(port)
                } else {
                    format!("{} (:{})", proc_name, port)
                };
                out.push(DiscoveredPort {
                    port,
                    pid,
                    bind_addr: addr_part.to_string(),
                    label,
                    local_url: format!("http://127.0.0.1:{}/", port),
                    preview_path: format!("/preview/{}/", port),
                });
            }
        }
    }

    out.sort_by_key(|p| p.port);
    out
}

fn guess_port_label(port: u16) -> String {
    match port {
        3000 => "Next.js / Node Dev (:3000)".to_string(),
        3001..=3010 => format!("Web Dev (:{})", port),
        4321 => "Astro Dev (:4321)".to_string(),
        5173..=5179 => format!("Vite Dev (:{})", port),
        6006 => "Storybook (:6006)".to_string(),
        8000 | 8080 | 8888 => format!("HTTP Server (:{})", port),
        _ => format!("Local Server (:{})", port),
    }
}

/// Proxies an incoming `/preview/<port>/<rest>` HTTP request to `127.0.0.1:<port>/<rest>`.
pub fn proxy_localhost_preview(
    client_stream: &mut TcpStream,
    port: u16,
    subpath: &str,
    query: &str,
    method: &str,
    body: &[u8],
    set_token_cookie: Option<&str>,
) -> Result<()> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut upstream = TcpStream::connect_timeout(&addr, Duration::from_millis(1500))
        .map_err(|e| anyhow!("无法连接本地端口 127.0.0.1:{}: {}", port, e))?;
    upstream.set_read_timeout(Some(Duration::from_secs(8))).ok();
    upstream.set_write_timeout(Some(Duration::from_secs(5))).ok();

    let clean_path = if subpath.starts_with('/') {
        subpath.to_string()
    } else {
        format!("/{}", subpath)
    };
    let target_uri = if query.is_empty() {
        clean_path
    } else {
        format!("{}?{}", clean_path, query)
    };

    let req_header = format!(
        "{} {} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
        method,
        target_uri,
        port,
        body.len()
    );
    upstream.write_all(req_header.as_bytes())?;
    if !body.is_empty() {
        upstream.write_all(body)?;
    }
    upstream.flush()?;

    let mut raw_resp = Vec::new();
    upstream.read_to_end(&mut raw_resp)?;

    if let Some(token) = set_token_cookie {
        if let Some(pos) = raw_resp.windows(4).position(|w| w == b"\r\n\r\n") {
            let mut patched = Vec::with_capacity(raw_resp.len() + 128);
            patched.extend_from_slice(&raw_resp[..pos]);
            let cookie = format!(
                "\r\nSet-Cookie: unterm_preview_token={}; Path=/preview/; HttpOnly; SameSite=Lax",
                token
            );
            patched.extend_from_slice(cookie.as_bytes());
            patched.extend_from_slice(&raw_resp[pos..]);
            client_stream.write_all(&patched)?;
            client_stream.flush()?;
            return Ok(());
        }
    }

    client_stream.write_all(&raw_resp)?;
    client_stream.flush()?;
    Ok(())
}

// ============================================================================
// 3. Hidden Git Ref Checkpoints (`refs/unterm/checkpoints/<id>`)
// ============================================================================

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GitCheckpoint {
    pub id: String,
    pub commit_sha: String,
    pub created_at: String,
    pub label: String,
    pub pane_id: Option<u64>,
    pub repo_root: String,
    pub stat_summary: String,
}

pub fn resolve_git_root(cwd: &str) -> Result<PathBuf> {
    let path = if cwd.trim().is_empty() {
        std::env::current_dir()?
    } else {
        PathBuf::from(cwd)
    };
    let out = silent_command("git")
        .current_dir(&path)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|e| anyhow!("无法执行 git: {}", e))?;
    if !out.status.success() {
        return Err(anyhow!("当前目录不是 Git 仓库: {}", path.display()));
    }
    let root = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok(PathBuf::from(root))
}

/// Creates a hidden Git checkpoint under `refs/unterm/checkpoints/<id>` without
/// modifying `.git/index`, `HEAD`, or the working tree.
pub fn create_git_checkpoint(
    cwd: &str,
    label: &str,
    pane_id: Option<u64>,
) -> Result<GitCheckpoint> {
    let root = resolve_git_root(cwd)?;
    let git_dir_out = silent_command("git")
        .current_dir(&root)
        .args(["rev-parse", "--git-dir"])
        .output()?;
    let git_dir_rel = String::from_utf8_lossy(&git_dir_out.stdout).trim().to_string();
    let git_dir = if Path::new(&git_dir_rel).is_absolute() {
        PathBuf::from(git_dir_rel)
    } else {
        root.join(git_dir_rel)
    };

    let id = format!("ckpt-{}-{}", pane_id.unwrap_or(0), now_ms());
    let temp_index = git_dir.join(format!("unterm_idx_{}", now_ms()));
    let real_index = git_dir.join("index");
    if real_index.exists() {
        let _ = std::fs::copy(&real_index, &temp_index);
    }

    // 1. Stage all working tree changes (including untracked) into the temporary index
    let add_out = silent_command("git")
        .current_dir(&root)
        .env("GIT_INDEX_FILE", &temp_index)
        .args(["add", "-A"])
        .output();
    if let Err(e) = add_out {
        let _ = std::fs::remove_file(&temp_index);
        return Err(anyhow!("git add 临时索引失败: {}", e));
    }

    // 2. Write tree object from temporary index
    let tree_out = silent_command("git")
        .current_dir(&root)
        .env("GIT_INDEX_FILE", &temp_index)
        .arg("write-tree")
        .output()?;
    let _ = std::fs::remove_file(&temp_index);

    if !tree_out.status.success() {
        return Err(anyhow!(
            "git write-tree 失败: {}",
            String::from_utf8_lossy(&tree_out.stderr)
        ));
    }
    let tree_sha = String::from_utf8_lossy(&tree_out.stdout).trim().to_string();

    // 3. Resolve HEAD if present
    let head_out = silent_command("git")
        .current_dir(&root)
        .args(["rev-parse", "--verify", "HEAD"])
        .output()?;
    let head_sha = if head_out.status.success() {
        Some(String::from_utf8_lossy(&head_out.stdout).trim().to_string())
    } else {
        None
    };

    let clean_label = if label.trim().is_empty() {
        "手动检查点 (Checkpoint)"
    } else {
        label.trim()
    };
    let subject = format!(
        "[unterm:{}] {}",
        pane_id.map(|p| p.to_string()).unwrap_or_else(|| "-".to_string()),
        clean_label.replace('\n', " ")
    );

    // 4. Create commit object via git commit-tree
    let mut commit_cmd = silent_command("git");
    commit_cmd.current_dir(&root).arg("commit-tree").arg(&tree_sha);
    if let Some(ref h) = head_sha {
        commit_cmd.args(["-p", h]);
    }
    commit_cmd.args(["-m", &subject]);
    let commit_out = commit_cmd.output()?;
    if !commit_out.status.success() {
        return Err(anyhow!(
            "git commit-tree 失败: {}",
            String::from_utf8_lossy(&commit_out.stderr)
        ));
    }
    let commit_sha = String::from_utf8_lossy(&commit_out.stdout).trim().to_string();

    // 5. Store under hidden ref `refs/unterm/checkpoints/<id>`
    let ref_name = format!("refs/unterm/checkpoints/{}", id);
    let update_out = silent_command("git")
        .current_dir(&root)
        .args(["update-ref", &ref_name, &commit_sha])
        .output()?;
    if !update_out.status.success() {
        return Err(anyhow!(
            "git update-ref 失败: {}",
            String::from_utf8_lossy(&update_out.stderr)
        ));
    }

    let stat_summary = checkpoint_stat_summary(&root, &commit_sha, head_sha.as_deref());

    Ok(GitCheckpoint {
        id,
        commit_sha,
        created_at: now_iso(),
        label: clean_label.to_string(),
        pane_id,
        repo_root: root.to_string_lossy().to_string(),
        stat_summary,
    })
}

fn checkpoint_stat_summary(root: &Path, commit_sha: &str, parent_sha: Option<&str>) -> String {
    let out = if let Some(p) = parent_sha {
        silent_command("git")
            .current_dir(root)
            .args(["diff", "--shortstat", p, commit_sha])
            .output()
    } else {
        silent_command("git")
            .current_dir(root)
            .args(["show", "--shortstat", "--format=", commit_sha])
            .output()
    };
    match out {
        Ok(o) if o.status.success() => {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if s.is_empty() {
                "工作区无变更 (Clean)".to_string()
            } else {
                s
            }
        }
        _ => String::new(),
    }
}

pub fn list_git_checkpoints(cwd: &str) -> Result<Vec<GitCheckpoint>> {
    let root = resolve_git_root(cwd)?;
    let out = silent_command("git")
        .current_dir(&root)
        .args([
            "for-each-ref",
            "--sort=-creatordate",
            "--count=30",
            "--format=%(refname)|%(objectname)|%(creatordate:iso-strict)|%(subject)",
            "refs/unterm/checkpoints/",
        ])
        .output()?;
    if !out.status.success() {
        return Ok(Vec::new());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut list = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.splitn(4, '|').collect();
        if parts.len() < 4 {
            continue;
        }
        let id = parts[0]
            .strip_prefix("refs/unterm/checkpoints/")
            .unwrap_or(parts[0])
            .to_string();
        let commit_sha = parts[1].to_string();
        let created_at = parts[2].to_string();
        let raw_subj = parts[3];
        let (pane_id, label) = if let Some(rest) = raw_subj.strip_prefix("[unterm:") {
            if let Some((pid_str, lbl)) = rest.split_once(']') {
                (pid_str.parse::<u64>().ok(), lbl.trim().to_string())
            } else {
                (None, raw_subj.to_string())
            }
        } else {
            (None, raw_subj.to_string())
        };
        let parent_out = silent_command("git")
            .current_dir(&root)
            .args(["rev-parse", "--verify", &format!("{}^1", commit_sha)])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
        let stat_summary = checkpoint_stat_summary(&root, &commit_sha, parent_out.as_deref());
        list.push(GitCheckpoint {
            id,
            commit_sha,
            created_at,
            label,
            pane_id,
            repo_root: root.to_string_lossy().to_string(),
            stat_summary,
        });
    }
    Ok(list)
}

pub fn diff_git_checkpoint(cwd: &str, checkpoint_id: &str) -> Result<Value> {
    let root = resolve_git_root(cwd)?;
    let ref_name = if checkpoint_id.starts_with("refs/") {
        checkpoint_id.to_string()
    } else {
        format!("refs/unterm/checkpoints/{}", checkpoint_id)
    };
    let sha_out = silent_command("git")
        .current_dir(&root)
        .args(["rev-parse", "--verify", &ref_name])
        .output()?;
    if !sha_out.status.success() {
        return Err(anyhow!("未找到检查点: {}", checkpoint_id));
    }
    let commit_sha = String::from_utf8_lossy(&sha_out.stdout).trim().to_string();

    // Diff against parent (what was changed in that turn) + diff against current working tree
    let turn_diff = silent_command("git")
        .current_dir(&root)
        .args(["show", "--stat", "--patch", "--format=", &commit_sha])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();

    let vs_current = silent_command("git")
        .current_dir(&root)
        .args(["diff", "--stat", &commit_sha])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();

    let truncated_patch = if turn_diff.len() > 24_000 {
        format!("{}...\n[Diff truncated at 24KB]", &turn_diff[..24_000])
    } else {
        turn_diff
    };

    Ok(json!({
        "id": checkpoint_id,
        "commit_sha": commit_sha,
        "repo_root": root.to_string_lossy(),
        "diff_patch": truncated_patch,
        "vs_current_stat": vs_current.trim(),
    }))
}

pub fn restore_git_checkpoint(cwd: &str, checkpoint_id: &str) -> Result<Value> {
    let root = resolve_git_root(cwd)?;
    let ref_name = if checkpoint_id.starts_with("refs/") {
        checkpoint_id.to_string()
    } else {
        format!("refs/unterm/checkpoints/{}", checkpoint_id)
    };
    let sha_out = silent_command("git")
        .current_dir(&root)
        .args(["rev-parse", "--verify", &ref_name])
        .output()?;
    if !sha_out.status.success() {
        return Err(anyhow!("未找到检查点: {}", checkpoint_id));
    }
    let commit_sha = String::from_utf8_lossy(&sha_out.stdout).trim().to_string();

    // 1. Always create an automatic safety checkpoint before restoring
    let backup = create_git_checkpoint(
        cwd,
        &format!("回滚前自动备份 (Before restore -> {})", checkpoint_id),
        None,
    )?;

    // 2. Restore working tree files from checkpoint tree without moving HEAD branch
    let checkout_out = silent_command("git")
        .current_dir(&root)
        .args(["checkout", &commit_sha, "--", "."])
        .output()?;
    if !checkout_out.status.success() {
        return Err(anyhow!(
            "恢复检查点文件失败: {}",
            String::from_utf8_lossy(&checkout_out.stderr)
        ));
    }
    // Unstage so working tree has the exact restored files while index matches HEAD
    let _ = silent_command("git")
        .current_dir(&root)
        .args(["reset", "HEAD"])
        .output();

    Ok(json!({
        "ok": true,
        "restored_id": checkpoint_id,
        "restored_sha": commit_sha,
        "safety_backup": backup,
    }))
}

// ============================================================================
// 4. Cross-Agent Portable Handoffs
// ============================================================================

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PortableHandoffBundle {
    pub handoff_id: String,
    pub source_pane_id: u64,
    pub cwd: String,
    pub target_agent: String,
    pub markdown_path: String,
    pub prompt_text: String,
    pub token_estimate: usize,
}

pub fn build_portable_handoff(
    source_pane_id: u64,
    cwd: &str,
    pane_title: &str,
    scrollback_text: &str,
    target_agent: &str,
    user_request: &str,
    token_cap: usize,
) -> Result<PortableHandoffBundle> {
    let cap = token_cap.clamp(1024, 64_000);
    let effective_cwd = if cwd.trim().is_empty() {
        std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".to_string())
    } else {
        cwd.trim().to_string()
    };

    // Gather git status + diff stat if in a git repository
    let mut git_context = String::new();
    if let Ok(root) = resolve_git_root(&effective_cwd) {
        let status = silent_command("git")
            .current_dir(&root)
            .args(["status", "--short"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        let diff_stat = silent_command("git")
            .current_dir(&root)
            .args(["diff", "--stat"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        if !status.is_empty() || !diff_stat.is_empty() {
            git_context = format!(
                "## Git Workspace State (`{}`)\n```text\n{}\n\n{}\n```\n",
                root.display(),
                status,
                diff_stat
            );
        }
    }

    // Reserve full room for user_request (T3 Code rule: never shorten the user's new request)
    let reserved = user_request.len() + git_context.len() + 600;
    let history_budget = cap.saturating_sub(reserved).max(512);

    // Keep the most recent lines from scrollback_text within history_budget
    let lines: Vec<&str> = scrollback_text
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty())
        .collect();
    let mut selected_lines = Vec::new();
    let mut used = 0usize;
    for &line in lines.iter().rev() {
        if used + line.len() + 1 > history_budget {
            break;
        }
        selected_lines.push(line);
        used += line.len() + 1;
    }
    selected_lines.reverse();
    let omitted_lines = lines.len().saturating_sub(selected_lines.len());

    let handoff_id = format!("handoff-pane{}-{}", source_pane_id, now_ms());
    let handoff_dir = unterm_protocol::state_dir()
        .unwrap_or_else(|| dirs_next::home_dir().unwrap_or_default().join(".unterm"))
        .join("handoffs");
    let _ = std::fs::create_dir_all(&handoff_dir);
    let md_path = handoff_dir.join(format!("{}.md", handoff_id));

    let prompt_text = format!(
        "# Portable Agent Handoff ({})\n\
         - Source Pane: `#{}` (`{}`)\n\
         - Working Directory: `{}`\n\
         - Target Agent: `{}`\n\
         - Omitted Earlier Terminal Lines: `{}` (full log saved in `{}`)\n\n\
         {}\n\
         ## Recent Session Activity & Output\n\
         ```text\n\
         {}\n\
         ```\n\n\
         ## Next Task / User Request\n\
         {}\n",
        handoff_id,
        source_pane_id,
        pane_title,
        effective_cwd,
        target_agent,
        omitted_lines,
        md_path.display(),
        git_context,
        selected_lines.join("\n"),
        if user_request.trim().is_empty() {
            "请基于以上前序终端上下文与 Git 修改状态，继续推进未完成的任务。"
        } else {
            user_request.trim()
        }
    );

    std::fs::write(&md_path, &prompt_text)?;
    let token_estimate = prompt_text.len();

    Ok(PortableHandoffBundle {
        handoff_id,
        source_pane_id,
        cwd: effective_cwd,
        target_agent: target_agent.to_string(),
        markdown_path: md_path.to_string_lossy().to_string(),
        prompt_text,
        token_estimate,
    })
}

// ============================================================================
// 5. Window SnapShot + Accessibility (A11y) Control Tree
// ============================================================================

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WindowSnapShotResult {
    pub id: String,
    pub window_title: String,
    pub process_name: String,
    pub image_path: String,
    pub a11y_path: String,
    pub bounds: Value,
    pub controls: Vec<Value>,
    pub agent_prompt_snippet: String,
}

pub fn capture_window_with_a11y(include_a11y: bool) -> Result<WindowSnapShotResult> {
    let snap_dir = unterm_protocol::state_dir()
        .unwrap_or_else(|| dirs_next::home_dir().unwrap_or_default().join(".unterm"))
        .join("snapshots");
    let _ = std::fs::create_dir_all(&snap_dir);

    let id = format!("snap-{}", now_ms());
    let img_path = snap_dir.join(format!("{}.png", id));
    let a11y_path = snap_dir.join(format!("{}.a11y.json", id));

    #[cfg(windows)]
    {
        let ps_script = format!(
            r#"
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes

Add-Type @"
using System;
using System.Runtime.InteropServices;
public class Win32Snap {{
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint lpdwProcessId);
    [StructLayout(LayoutKind.Sequential)] public struct RECT {{ public int Left; public int Top; public int Right; public int Bottom; }}
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT lpRect);
}}
"@

$hwnd = [Win32Snap]::GetForegroundWindow()
$rect = New-Object Win32Snap+RECT
[void][Win32Snap]::GetWindowRect($hwnd, [ref]$rect)
$w = [Math]::Max(1, $rect.Right - $rect.Left)
$h = [Math]::Max(1, $rect.Bottom - $rect.Top)

$bmp = New-Object System.Drawing.Bitmap($w, $h)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($rect.Left, $rect.Top, 0, 0, (New-Object System.Drawing.Size($w, $h)))
$g.Dispose()
$bmp.Save('{img}', [System.Drawing.Imaging.ImageFormat]::Png)
$bmp.Dispose()

[uint32]$pidOut = 0
[void][Win32Snap]::GetWindowThreadProcessId($hwnd, [ref]$pidOut)
$proc = Get-Process -Id $pidOut -ErrorAction SilentlyContinue
$title = if ($proc) {{ $proc.MainWindowTitle }} else {{ "ForegroundWindow" }}
$procName = if ($proc) {{ $proc.ProcessName }} else {{ "unknown" }}

$controls = @()
if (${inc_a11y}) {{
    try {{
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)
        if ($root) {{
            if (-not $title) {{ $title = $root.Current.Name }}
            $cond = [System.Windows.Automation.Condition]::TrueCondition
            $all = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $cond)
            $limit = [Math]::Min(40, $all.Count)
            for ($i = 0; $i -lt $limit; $i++) {{
                $el = $all[$i]
                $name = $el.Current.Name
                $ct = $el.Current.ControlType.ProgrammaticName -replace '^ControlType\.', ''
                $b = $el.Current.BoundingRectangle
                if ($name -and (-not [Double]::IsInfinity($b.Width)) -and $b.Width -gt 0 -and $b.Height -gt 0) {{
                    $controls += @{{
                        name = $name
                        type = $ct
                        rect = @([int]($b.X - $rect.Left), [int]($b.Y - $rect.Top), [int]$b.Width, [int]$b.Height)
                    }}
                }}
            }}
        }}
    }} catch {{}}
}}

$res = @{{
    window_title = [string]$title
    process_name = [string]$procName
    bounds = @{{ x = $rect.Left; y = $rect.Top; width = $w; height = $h }}
    controls = $controls
}}
$res | ConvertTo-Json -Depth 5 -Compress
"#,
            img = img_path.to_string_lossy().replace('\'', "''"),
            inc_a11y = if include_a11y { "true" } else { "false" }
        );

        let out = silent_command("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &ps_script])
            .output()
            .map_err(|e| anyhow!("执行截图失败: {}", e))?;

        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let parsed: Value = serde_json::from_str(&stdout).unwrap_or_else(|_| {
            json!({
                "window_title": "Captured Window",
                "process_name": "desktop",
                "bounds": {"x": 0, "y": 0, "width": 0, "height": 0},
                "controls": []
            })
        });

        let window_title = parsed
            .get("window_title")
            .and_then(Value::as_str)
            .unwrap_or("Window")
            .to_string();
        let process_name = parsed
            .get("process_name")
            .and_then(Value::as_str)
            .unwrap_or("app")
            .to_string();
        let bounds = parsed.get("bounds").cloned().unwrap_or_else(|| json!({}));
        let controls = parsed
            .get("controls")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        std::fs::write(&a11y_path, serde_json::to_string_pretty(&parsed)?)?;

        let snippet = format!(
            "[SnapShot `{}` | Window: `{}` (`{}`) | Image: `{}` | A11y JSON ({} controls): `{}`]",
            id,
            window_title,
            process_name,
            img_path.display(),
            controls.len(),
            a11y_path.display()
        );

        return Ok(WindowSnapShotResult {
            id,
            window_title,
            process_name,
            image_path: img_path.to_string_lossy().to_string(),
            a11y_path: a11y_path.to_string_lossy().to_string(),
            bounds,
            controls,
            agent_prompt_snippet: snippet,
        });
    }

    #[cfg(target_os = "macos")]
    {
        let _ = silent_command("screencapture")
            .args(["-x", &img_path.to_string_lossy()])
            .output();

        let jxa_script = format!(
            r#"
ObjC.import('stdlib');
var se = Application('System Events');
var procName = 'macOS';
var winTitle = 'Window';
var bounds = {{ x: 0, y: 0, width: 0, height: 0 }};
var controls = [];
try {{
    var procs = se.applicationProcesses.whose({{ frontmost: true }})();
    if (procs.length > 0) {{
        var p = procs[0];
        procName = p.name();
        var wins = p.windows();
        if (wins.length > 0) {{
            var w = wins[0];
            winTitle = w.name() || procName;
            var pos = w.position();
            var sz = w.size();
            bounds = {{ x: pos[0] || 0, y: pos[1] || 0, width: sz[0] || 0, height: sz[1] || 0 }};
            if ({inc_a11y}) {{
                var btns = w.buttons();
                var limit = Math.min(30, btns.length);
                for (var i = 0; i < limit; i++) {{
                    try {{
                        var b = btns[i];
                        var nm = b.name() || b.description() || '';
                        var bp = b.position();
                        var bs = b.size();
                        if (nm) {{
                            controls.push({{
                                name: String(nm),
                                type: 'Button',
                                rect: [bp[0] - bounds.x, bp[1] - bounds.y, bs[0], bs[1]]
                            }});
                        }}
                    }} catch (e) {{}}
                }}
            }}
        }}
    }}
}} catch (e) {{}}
JSON.stringify({{
    window_title: String(winTitle),
    process_name: String(procName),
    bounds: bounds,
    controls: controls
}});
"#,
            inc_a11y = if include_a11y { "true" } else { "false" }
        );

        let out = silent_command("osascript")
            .args(["-l", "JavaScript", "-e", &jxa_script])
            .output()
            .map_err(|e| anyhow!("执行 macOS 窗口快照失败: {}", e))?;

        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let parsed: Value = serde_json::from_str(&stdout).unwrap_or_else(|_| {
            json!({
                "window_title": "Captured Window",
                "process_name": "macos",
                "bounds": {"x": 0, "y": 0, "width": 0, "height": 0},
                "controls": []
            })
        });

        let window_title = parsed
            .get("window_title")
            .and_then(Value::as_str)
            .unwrap_or("Window")
            .to_string();
        let process_name = parsed
            .get("process_name")
            .and_then(Value::as_str)
            .unwrap_or("app")
            .to_string();
        let bounds = parsed.get("bounds").cloned().unwrap_or_else(|| json!({}));
        let controls = parsed
            .get("controls")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        std::fs::write(&a11y_path, serde_json::to_string_pretty(&parsed)?)?;

        let snippet = format!(
            "[SnapShot `{}` | Window: `{}` (`{}`) | Image: `{}` | A11y JSON ({} controls): `{}`]",
            id,
            window_title,
            process_name,
            img_path.display(),
            controls.len(),
            a11y_path.display()
        );

        return Ok(WindowSnapShotResult {
            id,
            window_title,
            process_name,
            image_path: img_path.to_string_lossy().to_string(),
            a11y_path: a11y_path.to_string_lossy().to_string(),
            bounds,
            controls,
            agent_prompt_snippet: snippet,
        });
    }

    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        let _ = (include_a11y, img_path, a11y_path);
        Err(anyhow!("当前平台暂未配置窗口 A11y 捕获脚本"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_enforcement_permits_and_blocks_expected_methods() {
        // Read-only can read screen and list checkpoints, but cannot approve or send input
        assert!(is_method_allowed_for_scope("readonly", "screen.text"));
        assert!(is_method_allowed_for_scope("readonly", "checkpoint.list"));
        assert!(!is_method_allowed_for_scope("readonly", "approval.decide"));
        assert!(!is_method_allowed_for_scope("readonly", "session.input"));

        // Approve & Steer can decide approvals and paste input, but cannot send signals or restore checkpoints
        assert!(is_method_allowed_for_scope("approve", "approval.decide"));
        assert!(is_method_allowed_for_scope("approve", "session.paste"));
        assert!(!is_method_allowed_for_scope("approve", "signal.send"));
        assert!(!is_method_allowed_for_scope("approve", "checkpoint.restore"));

        // Full can do all allowlisted operations
        assert!(is_method_allowed_for_scope("full", "signal.send"));
        assert!(is_method_allowed_for_scope("full", "checkpoint.restore"));
        assert!(!is_method_allowed_for_scope("full", "exec.run"));
    }

    #[test]
    fn hidden_git_checkpoint_creates_lists_diffs_and_restores_without_touching_branch() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path();

        let run_git = |args: &[&str]| {
            let out = silent_command("git").current_dir(repo).args(args).output().unwrap();
            assert!(
                out.status.success(),
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&out.stderr)
            );
        };

        run_git(&["init"]);
        run_git(&["config", "user.email", "test@unterm.dev"]);
        run_git(&["config", "user.name", "Unterm Test"]);

        let file_a = repo.join("hello.txt");
        std::fs::write(&file_a, "v1\n").unwrap();
        run_git(&["add", "."]);
        run_git(&["commit", "-m", "initial"]);

        // Modify tracked file + create untracked file
        std::fs::write(&file_a, "v2-checkpointed\n").unwrap();
        let file_b = repo.join("new_note.txt");
        std::fs::write(&file_b, "untracked content\n").unwrap();

        let repo_str = repo.to_string_lossy().to_string();
        let ckpt = create_git_checkpoint(&repo_str, "Before risky refactor", Some(7)).unwrap();
        assert!(ckpt.id.starts_with("ckpt-7-"));
        assert_eq!(ckpt.label, "Before risky refactor");

        // List checkpoints
        let list = list_git_checkpoints(&repo_str).unwrap();
        assert!(!list.is_empty());
        assert_eq!(list[0].id, ckpt.id);

        // Diff checkpoint
        let diff = diff_git_checkpoint(&repo_str, &ckpt.id).unwrap();
        assert!(diff["diff_patch"].as_str().unwrap().contains("v2-checkpointed"));

        // Overwrite hello.txt with broken content, then restore from checkpoint
        std::fs::write(&file_a, "BROKEN_CONTENT\n").unwrap();
        let res = restore_git_checkpoint(&repo_str, &ckpt.id).unwrap();
        assert_eq!(res["ok"], json!(true));
        assert_eq!(std::fs::read_to_string(&file_a).unwrap(), "v2-checkpointed\n");
    }

    #[test]
    fn portable_handoff_preserves_user_request_and_stays_within_budget() {
        let temp = tempfile::tempdir().unwrap();
        let long_log: String = (0..200)
            .map(|i| format!("step {}: executed cargo test and checked output", i))
            .collect::<Vec<_>>()
            .join("\n");
        let user_req = "Continue implementing the WebSocket reconnect logic without breaking existing tests.";
        let bundle = build_portable_handoff(
            12,
            &temp.path().to_string_lossy(),
            "claude",
            &long_log,
            "codex",
            user_req,
            2048,
        )
        .unwrap();
        assert!(bundle.prompt_text.contains(user_req));
        assert!(Path::new(&bundle.markdown_path).exists());
    }
}
