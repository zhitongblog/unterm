# MCP 目录提交包（最高杠杆渠道 · v0.71+ 更新版）

在这些目录里，用户**主动搜索**“能让 Agent 控制终端/协同开发的 MCP Server”。Unterm 是极少数**“真实原生桌面终端本体 = 155 方法 MCP Server + 多 Agent 指挥座舱”**的项目，品类辨识度极高。

## 通用字段（各家表单直接粘贴）

- **Name**: Unterm
- **一句话 Tagline**: The terminal AI agents can drive — a native Rust desktop terminal & multi-agent cockpit (macOS/Linux/Windows) exposing 155 MCP methods across 16 namespaces.
- **Categories**: Developer Tools / Terminal / Agent Orchestration / Desktop Automation
- **Repo**: https://github.com/zhitongblog/unterm
- **Site**: https://unterm.app
- **License**: MIT
- **Transport**: TCP (line-delimited JSON-RPC on `127.0.0.1`) via `unterm-cli mcp-stdio` bridge → stdio
- **客户端配置 JSON**（stdio bridge）：

```json
{
  "mcpServers": {
    "unterm": {
      "command": "unterm-cli",
      "args": ["mcp-stdio"]
    }
  }
}
```

（注：打开 Unterm Web 设置面板 → MCP 即可**一键自动写入**本机 Claude Code、Claude Desktop、Codex、Gemini CLI、Cursor、Windsurf、Antigravity 的配置文件，无需手动复制粘贴。）

## 目录收录与提交状态清单

| 目录 / 榜单 | 方式 | 当前状态（2026-11-15） |
|---|---|---|
| **Glama.ai/mcp** | 自动索引 | ✅ 已上线 (`glama.ai/mcp/servers/zhitongblog/unterm`，已有引流访客) |
| **PulseMCP** | 自动同步 GitHub topic | ✅ 已自动收录 |
| **awesome-opencode-market** | Issue 自动评审 | ⏳ [#3185](https://github.com/awesome-market/awesome-opencode-market/issues/3185)（2026-11-14 自动触发评审） |
| **sudosubin/agents.nix** | Nix 自动跟进 | ✅ 已自动跟进至 `v0.71.26` ([#132785](https://github.com/sudosubin/agents.nix/pull/132785)) |
| **patelflow/scoop-bucket-tools** | Windows Scoop 包 | ✅ 已收录 (`bucket/unterm.json`) |
| **jaywcjlove/awesome-rust-apps** | Rust 应用榜单 | ✅ 已收录 (`README.zh.md`) |
| **punkpeye/awesome-mcp-servers** (~60k★) | PR #7166 | ⏳ 已加徽章 + Rebase，等待 Maintainer 合并 |
| **ComposioHQ/awesome-claude-skills** | PR #960 | ⏳ 已提交 3 个官方 Skills，等待合并 |
| **mcp.so** | 网页表单 (`mcp.so/submit`) | ☐ 待粘贴提交（文案见 `marketplace/submissions/02-mcp-so.md`） |
| **Smithery.ai** | `smithery.yaml` + Claim | ☐ 待登录 Claim（步骤见 `marketplace/submissions/01-smithery.md`） |
| **Cursor MCP Directory** | 邮件 / Discord 提交 | ☐ 待发送（草稿见 `marketplace/submissions/03-cursor.md`） |

## awesome-mcp-servers PR 条目（最新 155 方法口径）

分类放 `🖥️ <a name="command-line">Command Line</a>`：

```markdown
- [zhitongblog/unterm](https://github.com/zhitongblog/unterm) 🦀 🏠 🍎 🪟 🐧 - A native Rust desktop terminal and multi-agent cockpit that is itself a 155-method MCP server: spawn tabs/panes, run commands with structured output, delegate tasks across CLI agents, create zero-pollution Git checkpoints, take headless scrolling screenshots, and approve actions remotely via LAN/Tailscale.
```
