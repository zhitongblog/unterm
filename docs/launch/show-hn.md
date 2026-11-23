# Show HN — 发帖包（v0.71+ Multi-Agent Cockpit 版）

**时机**：美东周二–周四早上 8:00–10:00（北京时间 20:00–22:00）。避开周五/周末和美国节假日。
**账号与互动**：发帖后头两小时守在评论区，HN 算法极看重早期深度的技术互动。

## Title（选一，≤80 字符）

1. `Show HN: Unterm – A Rust terminal and MCP server built for CLI AI agents`
2. `Show HN: Unterm – Multi-agent terminal cockpit with Git checkpoints and MCP`
3. `Show HN: A terminal with zero AI inside – it's the AI agents' hands instead`

## URL

`https://github.com/zhitongblog/unterm`

## 正文（text 字段，纯文本）

```text
I run Claude Code, Codex, and Gemini CLI side by side all day, and kept hitting the same walls:

1. Agents were blind to my actual terminal sessions, splits, and scrollback history;
2. Running agents with auto-approve felt like Russian roulette without a clean way to roll back a botched edit;
3. Stepping away from my desk meant an agent would stall waiting on a confirmation prompt;
4. Switching from one agent to another mid-task meant manually copy-pasting terminal errors and `git diff`.

Most 2026 terminals answer this by embedding an AI chat box and a cloud subscription inside the terminal. I think that's backwards: terminals are thirty-year tools, while AI models are six-month components. You already have great CLI agents — what they need isn't another brain inside the terminal, it's hands, guardrails, and a cockpit.

So I built Unterm (v0.71): a cross-platform GPU terminal written in Rust (`next-core` kernel + persistent `unterm-core` daemon) where the terminal itself is a 155-method MCP server across 16 namespaces:

- **1-Click MCP & Multi-Agent Delegation (`agent_session.delegate`)**: Auto-wires into Claude Code, Claude Desktop, Codex, Gemini CLI, Cursor, Windsurf, and Antigravity. A hub agent can even delegate sub-tasks to other CLI agents and wait on structured results.
- **Zero-Pollution Git Checkpoints (`refs/unterm/checkpoints`)**: Snapshots your working tree via `git write-tree` before agents act — without touching `HEAD`, your branch history, or `git status` — with 1-click diff and rollback.
- **Risk-Tiered Autonomy (`Cautious` / `Balanced` / `Autonomous`)**: Classifies commands and multi-line pastes into ReadOnly, WorkspaceWrite, ExternalSideEffect, and Destructive (always gating `rm -rf`, `git push --force`, `DROP TABLE`, etc.).
- **Unterm Pocket (LAN / Tailscale Mobile Remote)**: Scan a GPU-rendered QR code to watch live panes, trigger checkpoints, or approve/deny pending agent actions from your phone over LAN/Tailscale (Ed25519 challenge-response, zero cloud relay).
- **1-Click Failure Diagnosis & Cross-Agent Handoff**: When a command exits non-zero, 1 click packages the exit code, `git status`, and last 40 lines of output, and spawns your chosen CLI agent in a split pane to fix it.
- **Daemon Persistence & Scrolling Screenshots**: `unterm-core` keeps sessions alive across UI restarts, auto-restores tabs/splits/CWDs on cold boot, notifies on background commands >=10s, and can headlessly re-render your entire scrollback into one tall PNG.

Everything runs on `127.0.0.1` with bearer-token auth and a full per-call audit log. No account, no cloud, no telemetry. MIT licensed, macOS / Linux / Windows.

Happy to answer any technical questions — especially curious whether you prefer AI *inside* your terminal or *holding* it.
```

## 高频问答弹药（评论区秒回）

- **"How is this different from Warp?"**
  Warp puts a proprietary AI chat inside the terminal and routes through their cloud (account + subscription). Unterm has zero AI inside — it exposes a local 155-method MCP server and multi-agent safety cockpit so whatever CLI agents you already run (`claude`, `codex`, `gemini`, `opencode`, local models) can drive it locally.
- **"Why not just tmux + send-keys?"**
  `tmux send-keys` doesn't give you structured exit codes + stdout (`exec.run_wait`), risk-tiered command/paste interception, zero-pollution Git checkpoints (`refs/unterm/checkpoints`), cross-agent `delegate`, headless full-scrollback PNG rendering, or LAN/Tailscale mobile approval (`Unterm Pocket`).
- **"How do the Git checkpoints work without dirtying git status?"**
  We use plumbing commands (`git write-tree` + `git commit-tree` stored under `refs/unterm/checkpoints/<timestamp>`) with a temporary index. Your real `.git/index`, `HEAD`, and branch history are never touched until you explicitly click Rollback (which automatically creates a backup checkpoint first).
- **"Is the mobile remote (`Unterm Pocket`) routed through a relay server?"**
  Zero cloud relay. The listener enforces a strict CIDR allowlist (`127.0.0.0/8`, RFC1918 LAN ranges, and Tailscale `100.64.0.0/10`), pairs via a single-use 6-digit PIN + QR code, and authenticates via Ed25519 signatures.
