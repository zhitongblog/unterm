# Product Hunt 发布包（v0.71+ Multi-Agent Cockpit 版）

**时机**：太平洋时间 00:01 上架（北京时间 15:01/16:01），周二–周四。
**视觉资产建议（Gallery ≥ 4 张）**：
1. **图 1（Hero）**：多 Agent 座舱全景（左侧标签栏显示 Claude / Codex / Gemini 实时状态徽章与 `⚡协同` 标记 + 右侧分屏一键 AI 报错修复）。
2. **图 2（Web Settings 2.0）**：图形化设置面板中的「本机 AI 客户端 MCP 一键接入」网格卡片 + 「三级风险自治护栏 (`Cautious / Balanced / Autonomous`)」。
3. **图 3（Unterm Pocket & Checkpoints）**：窗口内 GPU 渲染的高清手机配对二维码 + 隐藏 Git 检查点（`refs/unterm/checkpoints`）一键 Diff 与无损回滚。
4. **图 4（Party Trick）**：无头滚屏高清长截图演示 + 会话录制转 Markdown（自动打码密钥）。

- **Name**: Unterm
- **Tagline**（60 字符内）: `The multi-agent terminal cockpit AI agents can drive`
- **备选 Tagline**: `Zero AI inside. 155 MCP tools & safety guardrails for yours.`

**Description**:

```text
Every terminal is racing to embed AI chat boxes and cloud subscriptions. Unterm makes the opposite bet: zero AI inside — instead, the terminal itself is a 155-method MCP server and multi-agent cockpit built in Rust.

Connect Claude Code, Codex, Gemini CLI, Cursor, Windsurf, or Antigravity in 1 click. Run agent fleets in isolated Git worktrees, delegate sub-tasks across agents, protect your repo with zero-pollution Git checkpoints (refs/unterm/checkpoints), diagnose command failures in 1 click, and approve agent actions from your phone over Wi-Fi/Tailscale with Unterm Pocket. Local-first, no account, no telemetry, MIT open source for macOS, Linux, and Windows.
```

**First comment（Maker Comment，上线即贴）**:

```text
Hi Product Hunt! Maker here 👋

I use Claude Code, Codex, and Gemini CLI side by side every day, and I kept hitting four walls:
1. Agents were blind to my live terminal sessions, splits, and scrollback;
2. Autonomous runs felt risky without a clean way to roll back a bad edit without dirtying `git status`;
3. Stepping away from my desk meant agents stalled waiting for confirmation prompts;
4. Handing context from one agent to another meant manually copy-pasting terminal output and diffs.

The industry answer is "put another AI chat box inside the terminal." I think terminals are 30-year tools and AI models are 6-month components. You already have great agents — they don't need another brain inside the terminal; they need hands, guardrails, and a cockpit.

So Unterm has ZERO AI inside. Instead, it's a pure-Rust GPU terminal (`<2ms` latency + persistent `unterm-core` daemon) that exposes 155 MCP methods across 16 namespaces:
- **1-Click MCP & Multi-Agent Delegation (`agent_session.delegate`)**
- **Zero-Pollution Git Checkpoints (`refs/unterm/checkpoints`)** with 1-click diff & rollback
- **Risk-Tiered Autonomy (`Cautious` / `Balanced` / `Autonomous`)** + Safe Multi-Line Paste Guard
- **Unterm Pocket**: scan a QR code to watch panes & approve actions from your phone over LAN / Tailscale (zero cloud relay)
- **1-Click Command Failure Diagnosis** in a split pane + headless full-scrollback PNG screenshots

Everything is 100% local (`127.0.0.1` + token + audit log), $0 forever, and MIT open source.
Ask me anything — do you prefer AI *in* your terminal, or *holding* it?
```
