//! `unterm-cli remote` — manage Unterm Mobile remote access (LAN / Tailscale).

use super::client::ServerEndpoint;
use anyhow::{Context, Result};
use clap::Parser;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::Duration;
use unterm_settings::remote as gw;

#[derive(Debug, Parser, Clone)]
pub struct RemoteCommand {
    #[command(subcommand)]
    pub sub: RemoteSubCommand,
}

#[derive(Debug, Parser, Clone)]
pub enum RemoteSubCommand {
    /// Show remote gateway status, LAN/Tailscale addresses, and paired devices.
    Status,
    /// Enable LAN/Tailscale mobile remote access (listens on port 19880).
    Enable,
    /// Disable mobile remote access.
    Disable,
    /// Generate a 2-minute 6-digit one-time pairing code and QR URI.
    Pair {
        /// Permission scope for the paired device: full, approve, or readonly.
        #[arg(long, default_value = "full")]
        scope: String,
    },
    /// List paired mobile devices.
    Devices,
    /// Revoke a paired mobile device by its device_id.
    Revoke {
        /// Device ID (or prefix) to revoke.
        device_id: String,
    },
    /// List active local dev server ports discovered for mobile preview.
    Ports,
    /// Create, list, diff, or restore zero-pollution hidden Git checkpoints (refs/unterm/checkpoints/<id>).
    Checkpoint {
        /// Action: list, create, diff, or restore.
        #[arg(default_value = "list")]
        action: String,
        /// Checkpoint ID (for diff / restore) or label (for create).
        target: Option<String>,
        /// Optional working directory (defaults to current directory).
        #[arg(long)]
        cwd: Option<String>,
    },
    /// Build a budgeted cross-agent portable handoff markdown bundle (~/.unterm/handoffs/).
    Handoff {
        /// User request / instruction (never truncated).
        #[arg(default_value = "请根据当前上下文与 Git 变更继续完成任务")]
        request: String,
        /// Target agent ID (e.g. claude-code, codex-cli, gemini-cli).
        #[arg(long, default_value = "claude-code")]
        target_agent: String,
        /// Optional working directory.
        #[arg(long)]
        cwd: Option<String>,
    },
    /// Capture foreground window PNG screenshot + UI Automation (A11y) control tree (~/.unterm/snapshots/).
    Snapshot,
    /// Run the mobile remote gateway in the foreground (useful for headless / CLI testing).
    Serve,
}

pub fn run(cmd: RemoteCommand, json_out: bool) -> Result<()> {
    match cmd.sub {
        RemoteSubCommand::Status => {
            let status = call_settings_http("GET", "/api/remote/status", None)
                .unwrap_or_else(|_| gw::status_snapshot());
            if json_out {
                println!("{}", serde_json::to_string_pretty(&status)?);
            } else {
                print_status(&status);
            }
            Ok(())
        }
        RemoteSubCommand::Enable => {
            let status = call_settings_http(
                "POST",
                "/api/remote/enable",
                Some(json!({"enabled": true})),
            )
            .or_else(|_| gw::set_enabled(true, None))?;
            if json_out {
                println!("{}", serde_json::to_string_pretty(&status)?);
            } else {
                println!("✅ 已开启手机远程连接（仅限局域网 / Tailscale）");
                print_status(&status);
            }
            Ok(())
        }
        RemoteSubCommand::Disable => {
            let status = call_settings_http(
                "POST",
                "/api/remote/enable",
                Some(json!({"enabled": false})),
            )
            .or_else(|_| gw::set_enabled(false, None))?;
            if json_out {
                println!("{}", serde_json::to_string_pretty(&status)?);
            } else {
                println!("⛔ 已关闭手机远程连接");
            }
            Ok(())
        }
        RemoteSubCommand::Pair { scope } => {
            let resp = call_settings_http(
                "POST",
                "/api/remote/pair-code",
                Some(json!({ "scope": scope })),
            )
            .or_else(|_| -> Result<Value> {
                let _ = gw::set_enabled(true, None)?;
                let bundle = gw::generate_pairing_code_with_scope(Some(&scope))?;
                Ok(json!({
                    "ok": true,
                    "pairing": bundle,
                    "status": gw::status_snapshot(),
                }))
            })?;
            if json_out {
                println!("{}", serde_json::to_string_pretty(&resp)?);
            } else if let Some(p) = resp.get("pairing") {
                println!("====================================================");
                println!("  Unterm 手机配对（有效期 120 秒 · 一次性使用）");
                println!(
                    "  ⚠️ {}",
                    p["notice"].as_str().unwrap_or("仅限局域网或 Tailscale")
                );
                println!("====================================================");
                println!("  6 位配对码 :  {}", p["code"].as_str().unwrap_or("------"));
                println!(
                    "  设备权限   :  {} ({})",
                    p["scope_label"].as_str().unwrap_or("完全控制"),
                    p["scope"].as_str().unwrap_or("full")
                );
                println!("  端口       :  {}", p["port"].as_u64().unwrap_or(19880));
                println!(
                    "  机器指纹   :  {}",
                    p["fingerprint"].as_str().unwrap_or("")
                );
                if let Some(addrs) = p["addresses"].as_array() {
                    println!("  可用地址   :");
                    for a in addrs {
                        println!(
                            "    - {} ({})",
                            a["ip"].as_str().unwrap_or(""),
                            a["label"].as_str().unwrap_or("")
                        );
                    }
                }
                println!("----------------------------------------------------");
                println!(
                    "  手机浏览器直连 : {}",
                    p["web_url"].as_str().unwrap_or("")
                );
                println!(
                    "  App 扫码 URI   : {}",
                    p["qr_uri"].as_str().unwrap_or("")
                );
                println!("====================================================");
            }
            Ok(())
        }
        RemoteSubCommand::Devices => {
            let status = call_settings_http("GET", "/api/remote/status", None)
                .unwrap_or_else(|_| gw::status_snapshot());
            let devices = status.get("devices").cloned().unwrap_or_else(|| json!([]));
            if json_out {
                println!("{}", serde_json::to_string_pretty(&devices)?);
            } else if let Some(arr) = devices.as_array() {
                if arr.is_empty() {
                    println!("当前没有已配对的手机设备。");
                } else {
                    println!("已配对的手机设备 ({})：", arr.len());
                    for d in arr {
                        println!(
                            "  - {} [{}] ({}) · 最近在线: {} ({})",
                            d["device_name"].as_str().unwrap_or(""),
                            d["platform"].as_str().unwrap_or(""),
                            d["device_id"].as_str().unwrap_or(""),
                            d["last_seen_at"].as_str().unwrap_or(""),
                            d["last_ip"].as_str().unwrap_or("")
                        );
                    }
                }
            }
            Ok(())
        }
        RemoteSubCommand::Revoke { device_id } => {
            let status = call_settings_http("GET", "/api/remote/status", None)
                .unwrap_or_else(|_| gw::status_snapshot());
            let mut full_id = device_id.clone();
            if let Some(arr) = status.get("devices").and_then(Value::as_array) {
                if let Some(found) = arr.iter().find(|d| {
                    d["device_id"]
                        .as_str()
                        .map(|s| s == device_id || s.starts_with(&device_id))
                        .unwrap_or(false)
                }) {
                    if let Some(s) = found["device_id"].as_str() {
                        full_id = s.to_string();
                    }
                }
            }
            let resp = call_settings_http(
                "POST",
                "/api/remote/revoke",
                Some(json!({"device_id": full_id})),
            )
            .or_else(|_| -> Result<Value> {
                let removed = gw::revoke_device(&full_id)?;
                Ok(json!({
                    "ok": true,
                    "removed": removed,
                    "status": gw::status_snapshot(),
                }))
            })?;
            if json_out {
                println!("{}", serde_json::to_string_pretty(&resp)?);
            } else if resp.get("removed").and_then(Value::as_bool) == Some(true) {
                println!("✅ 已撤销设备 {}", full_id);
            } else {
                println!("未找到设备 {}", device_id);
            }
            Ok(())
        }
        RemoteSubCommand::Ports => {
            let ports = unterm_settings::harness::discover_listening_ports(&[19880]);
            if json_out {
                println!("{}", serde_json::to_string_pretty(&ports)?);
            } else if ports.is_empty() {
                println!("当前未发现运行中的本地开发服务端口。");
            } else {
                println!("已发现本地开发服务端口 ({})：", ports.len());
                for p in ports {
                    println!(
                        "  - :{}  [{}]  手机预览路由: {}",
                        p.port, p.label, p.preview_path
                    );
                }
            }
            Ok(())
        }
        RemoteSubCommand::Checkpoint {
            action,
            target,
            cwd,
        } => {
            let dir = cwd.unwrap_or_else(|| {
                std::env::current_dir()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|_| ".".to_string())
            });
            match action.as_str() {
                "create" => {
                    let label = target.as_deref().unwrap_or("CLI 检查点");
                    let cp = unterm_settings::harness::create_git_checkpoint(&dir, label, None)?;
                    if json_out {
                        println!("{}", serde_json::to_string_pretty(&cp)?);
                    } else {
                        println!(
                            "✅ 已创建零污染隐藏检查点 {} ({}) -> refs/unterm/checkpoints/{} ({})",
                            cp.id,
                            cp.label,
                            cp.id,
                            &cp.commit_sha[..cp.commit_sha.len().min(10)]
                        );
                    }
                }
                "diff" => {
                    let id = target.as_deref().context("请提供检查点 ID")?;
                    let diff = unterm_settings::harness::diff_git_checkpoint(&dir, id)?;
                    if json_out {
                        println!("{}", serde_json::to_string_pretty(&json!({ "id": id, "diff": diff }))?);
                    } else {
                        println!("{}", diff);
                    }
                }
                "restore" => {
                    let id = target.as_deref().context("请提供要回滚的检查点 ID")?;
                    let msg = unterm_settings::harness::restore_git_checkpoint(&dir, id)?;
                    if json_out {
                        println!("{}", serde_json::to_string_pretty(&json!({ "ok": true, "id": id, "message": msg }))?);
                    } else {
                        println!("✅ {}", msg);
                    }
                }
                _ => {
                    let list = unterm_settings::harness::list_git_checkpoints(&dir)?;
                    if json_out {
                        println!("{}", serde_json::to_string_pretty(&list)?);
                    } else if list.is_empty() {
                        println!("当前仓库暂无隐藏检查点 (refs/unterm/checkpoints/*)。");
                    } else {
                        println!("隐藏 Git 检查点 ({})：", list.len());
                        for cp in list {
                            println!(
                                "  - {}  [{}]  commit {}  ({})",
                                cp.id,
                                cp.label,
                                &cp.commit_sha[..cp.commit_sha.len().min(8)],
                                cp.created_at
                            );
                        }
                    }
                }
            }
            Ok(())
        }
        RemoteSubCommand::Handoff {
            request,
            target_agent,
            cwd,
        } => {
            let dir = cwd.unwrap_or_else(|| {
                std::env::current_dir()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|_| ".".to_string())
            });
            let bundle = unterm_settings::harness::build_portable_handoff(
                0,
                &dir,
                "cli",
                "",
                &target_agent,
                &request,
                16_000,
            )?;
            if json_out {
                println!("{}", serde_json::to_string_pretty(&bundle)?);
            } else {
                println!(
                    "✅ 已生成跨 Agent 便携接力包 (约 {} tokens)：{}",
                    bundle.token_estimate, bundle.markdown_path
                );
            }
            Ok(())
        }
        RemoteSubCommand::Snapshot => {
            let snap = unterm_settings::harness::capture_window_with_a11y(true)?;
            if json_out {
                println!("{}", serde_json::to_string_pretty(&snap)?);
            } else {
                println!("✅ 已抓取当前窗口截图与 UI Automation 控件树：");
                println!("  截图路径 : {}", snap.image_path);
                println!("  控件树   : {} ({} 个控件)", snap.a11y_path, snap.controls.len());
                println!("  Agent 提示: {}", snap.agent_prompt_snippet);
            }
            Ok(())
        }
        RemoteSubCommand::Serve => {
            let _ = gw::set_enabled(true, None)?;
            let port = gw::ensure_remote_server_started_default();
            let snap = gw::status_snapshot();
            println!(
                "🚀 Unterm 手机远程网关已启动，监听端口 {}（仅限局域网 / Tailscale）",
                port
            );
            print_status(&snap);
            loop {
                std::thread::sleep(Duration::from_secs(60));
            }
        }
    }
}

fn print_status(s: &Value) {
    let enabled = s.get("enabled").and_then(Value::as_bool).unwrap_or(false);
    let running = s.get("running").and_then(Value::as_bool).unwrap_or(false);
    let port = s.get("port").and_then(Value::as_u64).unwrap_or(19880);
    println!(
        "状态       : {}",
        if enabled && running {
            "开启（运行中）"
        } else if enabled {
            "已开启"
        } else {
            "关闭（默认）"
        }
    );
    println!("网络限制   : {}", s["notice"].as_str().unwrap_or(""));
    println!("监听端口   : {}", port);
    println!("主机名     : {}", s["hostname"].as_str().unwrap_or(""));
    println!("机器 ID    : {}", s["machine_id"].as_str().unwrap_or(""));
    println!("机器指纹   : {}", s["fingerprint"].as_str().unwrap_or(""));
    if let Some(addrs) = s.get("addresses").and_then(Value::as_array) {
        println!("本机地址   :");
        for a in addrs {
            println!(
                "  - http://{}:{}/  [{}]",
                a["ip"].as_str().unwrap_or(""),
                port,
                a["label"].as_str().unwrap_or("")
            );
        }
    }
    if let Some(devs) = s.get("devices").and_then(Value::as_array) {
        println!("已绑定设备 : {} 台", devs.len());
    }
}

fn call_settings_http(method: &str, path: &str, body: Option<Value>) -> Result<Value> {
    let info = ServerEndpoint::resolve_http()?;
    if info.http_port == 0 {
        anyhow::bail!("no http port");
    }
    let addr = format!("127.0.0.1:{}", info.http_port);
    let mut stream = TcpStream::connect_timeout(&addr.parse()?, Duration::from_secs(2))
        .context("connecting to local Unterm Web Settings server")?;
    stream.set_read_timeout(Some(Duration::from_secs(4))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(4))).ok();

    let body_bytes = match body {
        Some(v) => serde_json::to_vec(&v)?,
        None => Vec::new(),
    };
    let req = format!(
        "{} {} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        method,
        path,
        info.http_port,
        info.token,
        body_bytes.len()
    );
    stream.write_all(req.as_bytes())?;
    if !body_bytes.is_empty() {
        stream.write_all(&body_bytes)?;
    }
    stream.flush()?;

    let mut reader = BufReader::new(stream);
    let mut status_line = String::new();
    reader.read_line(&mut status_line)?;
    if !status_line.contains(" 200 ") {
        anyhow::bail!("HTTP {}", status_line.trim());
    }
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line.trim().is_empty() {
            break;
        }
    }
    let mut resp_body = Vec::new();
    reader.read_to_end(&mut resp_body)?;
    let val: Value = serde_json::from_slice(&resp_body)
        .with_context(|| format!("invalid JSON from {}", path))?;
    Ok(val)
}
