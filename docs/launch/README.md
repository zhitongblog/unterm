# Unterm 推广作战手册（v0.71+ 多 Agent 座舱版）

现状基线（2026-11-15 更新）：`v0.71.26` 已发，19 Stars / 2 Forks，近 14 天 90 浏览 / 62 独立访客。
- **生态自然发酵**：已被 `awesome-rust-apps`（中文榜）、`awesome-opencode-market#3185`、`sudosubin/agents.nix`（自动跟进至 `0.71.26`）、`patelflow/scoop-bucket-tools`（Windows Scoop 包）、`mediiiiium/mcp-jp`、`Glama.ai`、`PulseMCP` 收录。
- **数字最新口径**：**155 个 MCP 方法 / 16 个命名空间**（`v0.71.26`），纯自研 Rust 终端内核（`next-core`）+ `unterm-core` 后台守护进程（`<2ms` 极致输入响应、热重连与全量会话布局持久化）。

## 核心叙事（所有渠道统一升级）

> 所有终端都在往里塞聊天框（Warp 们）。我们反着做：终端内部零 AI，但终端本身是一个 **155 方法的 MCP Server + 多 Agent 并行指挥座舱（Cockpit）**。
> 让已经在你机器上的 Claude Code、Codex、Gemini CLI、Antigravity (`agy`)、Cursor、OpenCode 像人一样驱动它、互相委派子任务（`agent_session.delegate`）、跨 Agent 一键接力（Context Handoff），还能用手机扫码（`Unterm Pocket`）在局域网/Tailscale 远程看盘审批，配合零污染 Git 隐藏检查点（`refs/unterm/checkpoints`）一键无损回滚。

### 五大视觉与痛点钩子（发帖必带）

1. **多 Agent 并行座舱 + 跨 Agent 协同委派 (`delegate` / `Handoff`)**：一队 CLI Agent 各占独立 Tab + Git Worktree，左侧标签栏实时显示 `Working / Waiting / Done` 与 `⚡N 协同` 徽章；一个 Agent 卡壳可一键打包终端末尾上下文 + Git 状态交接给另一个 Agent。
2. **手机扫码远程看盘与审批 (`Unterm Pocket`)**：离开工位喝咖啡时，手机扫一下窗口内 GPU 渲染的高清二维码（仅限局域网 Wi-Fi / Tailscale，Ed25519 端到端挑战签名，零云端中转），随时查看终端实时输出并点击 `Allow / Deny` 放行高危操作。
3. **零污染隐藏 Git 检查点 (`refs/unterm/checkpoints`) + 风险分级护栏 (`Cautious / Balanced / Autonomous`)**：Agent 动手前自动打隐藏快照（不污染 `git status` 和分支提交历史），改崩了一键无损回滚。
4. **守护进程热重连 + 会话/分屏自动恢复**：`unterm-core` 独立常驻，客户端升级或冷重启后自动恢复所有标签页标题、分屏方向与工作目录，长任务满 10 秒后台完成自动推送系统通知。
5. **命令报错一键 AI 分屏诊断 + 滚屏高清长截图**：命令非零退出时，点一下 `▼ -> AI 诊断当前窗格报错` 即可在右侧分屏拉起 Claude/Codex/Gemini 自动修 Bug；或把整个回滚区无头渲染成一张高清长图。

## 渠道优先级与节奏

| 阶段 | 动作 | 文件 |
|---|---|---|
| 第 0 步（发帖前） | 录制 15 秒核心动图（Pocket 手机扫码远程审批 + 报错一键分屏 AI 修复 + 隐藏 Git 检查点回滚） | — |
| 第 1 波（目录与市场） | 跟进 `awesome-opencode-market#3185`、提交 `mcp.so`、`Smithery`、`Cursor MCP Directory`，更新 `awesome-mcp-servers` PR #7166 | [mcp-directories.md](mcp-directories.md) |
| 第 2 波（中文主力破圈） | V2EX `/go/create`（分享创造）首发 `v0.71` 多 Agent 座舱贴 + 即刻 / 小红书 15s 动图 | [zh-channels.md](zh-channels.md) |
| 第 3 波（英文社区互导） | Show HN + X 英文 8 连推（主打 Multi-Agent Cockpit + Pocket 远程审批 + Hidden Git Checkpoints） | [show-hn.md](show-hn.md) / [x-thread.md](x-thread.md) |
| 第 4 波（Reddit 垂直 Sub） | `r/ClaudeAI` → `r/ChatGPTCoding` → `r/mcp` → `r/commandline`（隔天错峰发） | [reddit.md](reddit.md) |
| 第 5 波 | Product Hunt（配齐 4 张高清 Gallery 图 + Maker Comment） | [product-hunt.md](product-hunt.md) |

## 分工与监控

- **日常自动监控**：`ci/promo-watch.py` 每日 10:00 自动巡检 GitHub Mentions、Awesome 榜单引用、HN、V2EX、Traffic Referrers 与 Star 增量，写入 `~/.unterm/promo-watch/reports/` 并推送 macOS 系统通知。
- **红线**：不买量、不互赞、不在 HN/Reddit 用小号自顶；每个 Sub 间隔 ≥1 天；面对技术质疑直接用架构事实回应（纯自研 Rust `next-core` 内核、`<2ms` 键入延迟、155 个 MCP 方法、100% 本地 `127.0.0.1` + Token + 审计日志、MIT 开源）。
