# X/Twitter 线程包（v0.71+ Multi-Agent Cockpit & Pocket 版）

**时机**：配合 Show HN 同日发布，互相导流。
**标签**：`#MCP` `#ClaudeCode` `#Rust` `#CodingAgents`

## 英文 8 连推（v0.71 Multi-Agent Cockpit + Pocket + Checkpoints）

1/
Every terminal in 2026 is racing to put an AI chat box *inside* the terminal.
We made the opposite bet: a pure-Rust terminal with zero AI inside — built to be the cockpit and hands for the CLI agents you already run.
Unterm v0.71 is out. MIT, local-first, macOS / Linux / Windows. 🧵

2/
Why zero AI inside?
Terminals are 30-year tools. AI models are 6-month components.
You already run Claude Code, Codex, Gemini CLI, Cursor, or OpenCode. What they're missing isn't another brain — it's hands, safety guardrails, and multi-agent coordination.

3/
So Unterm is a 155-method MCP server across 16 namespaces:
- 1-click MCP wiring for Claude / Codex / Gemini / Cursor / Windsurf / Antigravity
- Spawn tabs & splits, run commands with structured JSON exit codes, read scrollback as text
- `agent_session.delegate`: let one CLI agent delegate sub-tasks to another!

4/
Fear #1 when running autonomous agents: "What if it wrecks my working tree?"
Unterm v0.71 ships Zero-Pollution Git Checkpoints (`refs/unterm/checkpoints`).
It snapshots your tree via `git write-tree` without touching `HEAD` or `git status` — diff or roll back in 1 click.

5/
Fear #2: "I stepped away for coffee and my agent is stuck waiting for approval."
Meet Unterm Pocket 📱: scan a GPU-rendered QR code over Wi-Fi / Tailscale (zero cloud relay, Ed25519 signed) to watch live terminal panes, create checkpoints, and tap Allow / Deny from your phone.

6/
Risk-Tiered Autonomy (`Cautious` / `Balanced` / `Autonomous`):
Commands and multi-line pastes are classified before execution. Even in Autonomous mode, destructive patterns (`rm -rf`, `git push --force`, `DROP TABLE`) are always intercepted for human confirmation.

7/
Command failed? Click "🩺 Diagnose Last Command Failure" in the top bar (or `Cmd+Shift+P`) to package the exit code, `git status`, and last 40 terminal lines — and automatically spawn Claude / Codex / Gemini in a split pane to fix it.

8/
Backed by `unterm-core` daemon for `<2ms` input latency, seamless session/split auto-restore across restarts, background long-command notifications (>=10s), and headless full-scrollback PNG screenshots.
No account. No cloud. No telemetry. MIT.
https://unterm.app
https://github.com/zhitongblog/unterm

---

## 中文精简线程（3 条）

1/
2026 年所有终端都在往里塞 AI 聊天框。我们反着做：纯 Rust 自研内核、内部零 AI 的终端，专门做 Claude Code / Codex / Gemini / Antigravity 的「多 Agent 协同座舱」。Unterm v0.71 已开源（MIT，macOS/Linux/Windows）。

2/
这次更新把日常用 CLI Agent 最痛的 4 件事全补齐了：
- 零污染 Git 隐藏检查点（`refs/unterm/checkpoints`）：不脏 git status，改崩一键无损回滚；
- Unterm Pocket 手机扫码远程看盘与审批（仅限局域网/Tailscale，零云端）；
- 跨 Agent 子任务委派（`delegate`）与一键上下文接力（Handoff）；
- 命令报错一键右侧分屏拉起 AI 诊断修复 + 后台长命令（>=10s）系统通知。

3/
终端本身暴露 155 个 MCP 方法（16 命名空间），图形化 Web 设置面板支持一键接入本机所有主流 AI 客户端、切换三级风险自治护栏、持久化恢复所有标签页与分屏布局。
官网：https://unterm.app
GitHub：https://github.com/zhitongblog/unterm
