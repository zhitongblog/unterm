# Reddit 发帖包（v0.71+ Multi-Agent Cockpit 版）

**规则**：每个 Sub 间隔至少 1–2 天，避免同日群发被标 Spam。美东工作日上午发布效果最佳。

---

## 1. `r/ClaudeAI`（最对口，Claude Code 重度用户聚集地）

**Title**: `I built an open-source Rust terminal cockpit for Claude Code: 155 MCP tools, zero-pollution Git checkpoints, 1-click error diagnosis, and LAN/Tailscale phone approvals`

**Body**:

```text
I use Claude Code all day alongside Codex and Gemini CLI, and kept running into the same friction:
- Claude couldn't see or drive my actual terminal splits and live scrollback;
- I wanted autonomous execution, but needed an instant, zero-pollution rollback if an edit went sideways;
- Stepping away from my desk meant Claude would sit waiting on a permission prompt;
- When a build command failed in a normal shell, I had to copy-paste the error output and git status into Claude manually.

So I built **Unterm** (v0.71): a native GPU terminal in Rust (`<2ms` input latency, persistent `unterm-core` daemon) where the terminal itself is a 155-method MCP server and multi-agent cockpit:

- **1-Click MCP Integration & Cross-Agent Delegation**: One click in the Web Settings panel wires Unterm's MCP into Claude Code, Claude Desktop, Codex, Gemini CLI, Cursor, and Windsurf. Claude can even call `agent_session.delegate` to hand a sub-task to Codex/Gemini and get a structured reply.
- **Hidden Git Checkpoints (`refs/unterm/checkpoints`)**: Snapshots your working tree before agent edits without touching `HEAD` or dirtying `git status`. Diff or roll back in one click.
- **Risk-Tiered Autonomy (`Cautious` / `Balanced` / `Autonomous`)**: Auto-approves safe work while always intercepting destructive commands (`rm -rf`, `git push --force`, `DROP TABLE`, etc.).
- **Unterm Pocket (Mobile Remote over LAN / Tailscale)**: Scan a QR code in the terminal to watch live panes and tap Allow/Deny on your phone while away from your desk — 100% local/Tailscale, Ed25519 signed, zero cloud relay.
- **1-Click Failure Diagnosis**: When any command exits non-zero, click "Diagnose Last Command Failure" to spawn Claude Code in a right-hand split pane pre-loaded with the exit code, `git status`, and last 40 lines of terminal output.

100% local (`127.0.0.1` + bearer token + audit log), no account, no telemetry, MIT licensed for macOS, Linux, and Windows.

GitHub: https://github.com/zhitongblog/unterm
Site: https://unterm.app

Would love feedback from fellow heavy Claude Code users!
```

---

## 2. `r/mcp`（MCP 开发者与重度玩家）

**Title**: `Unterm v0.71: A desktop terminal that IS a 155-method MCP server (plus multi-agent delegation, risk-tiered guardrails, and hidden Git checkpoints)`

**Body**:

```text
Most MCP servers wrap a cloud API or a stateless CLI tool. Unterm wraps a real desktop application: the terminal itself.

It's a native Rust terminal (macOS / Linux / Windows) that exposes 155 authenticated MCP methods across 16 namespaces (`session`, `exec`, `screen`, `capture`, `cockpit`, `orchestrate`, `governance`, `workspace`, `proxy`, etc.):

- Spawn tabs & split panes, run commands with structured JSON results (`exit_code` + stdout), read screen & full scrollback as text
- Headlessly re-render an entire scrollback buffer into a single tall PNG (`capture.scrolling_screenshot`)
- Delegate sub-tasks across different CLI agents via `agent_session.delegate` and package cross-agent context handoffs (`handoff.create`)
- Zero-pollution Git checkpoints under `refs/unterm/checkpoints` (`checkpoint.create` / `diff` / `restore`)
- 1-click MCP config toggle for Claude Code, Claude Desktop, Codex, Gemini CLI, Cursor, Windsurf, and Antigravity

MIT licensed, local-first (`127.0.0.1` + bearer token + per-call audit log).

Repo: https://github.com/zhitongblog/unterm
```

---

## 3. `r/commandline`（强调纯 Rust 性能、守护进程持久化与可脚本化）

**Title**: `Unterm — a pure-Rust GPU terminal where every feature has a CLI and JSON-RPC API (daemon persistence, split layout restore, scrolling screenshots)`

**Body**:

```text
Every feature in Unterm ships with a CLI subcommand (`unterm-cli`) and a local JSON-RPC method on day one (`unterm-cli reference` prints all 155 methods):

- Persistent `unterm-core` daemon: your shells survive UI restarts, and cold boots automatically restore all tabs, custom titles, split-pane layouts, and working directories
- Native notifications when background commands running >=10s finish (with duration and exit code)
- Headless scrolling screenshots: re-renders the entire scrollback into one tall PNG using your exact font and theme, even when the window is occluded
- Session recording to Markdown with automatic secret/token redaction
- Built-in per-window identity profiles (git/SSH/env credentials), proxy & Clash node switcher, and safe multi-line paste guard

It also speaks MCP for those who drive their terminal from AI agents, while keeping zero AI bloat inside the terminal itself.

MIT licensed, native Rust kernel, macOS / Linux / Windows.
https://github.com/zhitongblog/unterm
```
