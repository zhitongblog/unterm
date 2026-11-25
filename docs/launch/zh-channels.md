# 中文渠道发帖包（v0.71+ 多 Agent 座舱版）

## V2EX — `/go/create`（分享创造）⭐ 中文首发主阵地

**时机**：工作日上午 10:00 或下午 14:30。发完守评论区，V2EX 用户关注底层架构、性能与真实工程痛点。

**标题候选（选一）**：
1. `用 Rust 写了一个专为 Claude Code / Codex / Gemini 打造的多 Agent 终端座舱：带隐藏 Git 检查点一键回滚 + 手机扫码远程审批（MIT 开源）`
2. `做了一个“反 Warp”的纯 Rust 终端 Unterm：终端内部零 AI，却是所有 CLI Agent 的多智能体指挥台（155 个 MCP 方法 / MIT 开源）`

**正文**：

```text
我是重度 Claude Code / Codex / Gemini CLI 用户。日常同时开四五个 Agent 改项目时，一直有几个非常痛的点：

1. Agent 在终端里跑长任务或遇到高危命令需要确认时，我一离开工位去泡咖啡就卡住；
2. 开了全自动模式又怕 Agent 突然抽风把工作区改崩，用 git stash/commit 做备份又会把分支历史弄得很脏；
3. 一个 Agent 卡壳了，想换另一个 Agent（比如从 Claude 换 Codex）接着看，得手动复制一大堆终端报错和 git diff；
4. 市面上的 AI 终端（比如 Warp）全在往终端里面塞聊天框、绑云端账号和月费订阅——但我明明已经有最强的 CLI Agent 了，它们缺的不是“大脑”，而是“手”和“安全护栏”。

所以这几个月我用纯 Rust 写了 Unterm（目前已迭代到 v0.71）：一个内部刻意“零 AI”，但专为操控与协同多 Agent 打造的原生 GPU 终端。

几个我自己每天都在用的核心能力：

1. 终端本身就是 155 个方法的 MCP Server + 一键接入本机所有 AI 客户端
   在图形化设置面板里点一下，就能把 Unterm MCP 自动写进 Claude Code、Claude Desktop、Codex、Gemini CLI、Cursor、Windsurf、Antigravity (agy) 的配置里。Agent 可以直接调结构化接口开分屏、跑命令拿退出码、读完整回滚区文本（无需 OCR）、甚至把整个长回滚区无头渲染成一张高清长截图。

2. 多 Agent 协同座舱（Cockpit）+ 跨 Agent 委派与一键接力
   - 支持 Fleet 并发：一队 Agent 各占独立 Tab + 独立 Git Worktree，同时改一个仓库互不踩脚；
   - 主 Agent 可以通过 `agent_session.delegate` 直接把子任务委派给其它 CLI Agent，左侧标签栏实时显示 Working / Waiting / ⚡协同 状态；
   - 跨 Agent 一键接力（Handoff）：随时把当前窗格末尾上下文 + git status 打包，在右侧分屏一键拉起另一个 Agent 接着干。

3. 零污染隐藏 Git 检查点（`refs/unterm/checkpoints`）+ 三级风险自治护栏
   - 提供谨慎（Cautious）/ 平衡（Balanced）/ 全自治（Autonomous）三档模式，任何模式下高危破坏性指令（`rm -rf`、`git push --force`、`DROP TABLE` 等）以及高危多行粘贴都会被强制拦截；
   - Agent 修改前自动在 `refs/unterm/checkpoints` 打下隐藏 Git 快照，完全不污染 `git status` 和分支提交树，跑偏了一键无损回滚并查看 Diff。

4. 手机扫码远程看盘与审批（Unterm Pocket）
   - 窗口内直接用 GPU 渲染高清配对二维码（仅限同一局域网 Wi-Fi 或 Tailscale，Ed25519 端到端挑战签名，完全不经过任何云端服务器）；
   - 离开工位时手机扫一下，就能实时查看电脑上各个窗格的输出、一键打检查点、远程点击 Allow / Deny 审批命令。

5. `unterm-core` 守护进程热重连 + 报错一键 AI 分屏诊断
   - UI 与 `unterm-core` 守护进程分离，`<2ms` 极致输入响应；即使重启或升级客户端，所有标签页、分屏布局、所在目录全部无缝恢复；
   - 后台长命令（>=10s）跑完自动发 macOS 系统通知；命令报错非零退出时，点一下右上角菜单「🩺 AI 诊断当前窗格报错」即可复制结构化诊断包或在右侧分屏直接拉起 AI 修复。

全本地（127.0.0.1 + Bearer Token + 完整调用审计日志），无账号、无云端、无遥测、无付费墙。MIT 开源，支持 macOS / Linux / Windows。

官网：https://unterm.app
GitHub：https://github.com/zhitongblog/unterm

欢迎大家试用拍砖——很好奇各位重度使用 CLI Agent 的朋友，你们更希望 AI 塞在终端“里面”，还是让终端成为多 Agent 的“指挥座舱”？
```

**预备高频问答（评论区弹药）**：
- **“和 tmux + send-keys 比有什么优势？”** → `tmux` 拿不到结构化退出码和输出（`exec.run_wait` 直接返回 JSON）、没有按风险分级的命令审批与多行粘贴拦截、没有零污染的 `refs/unterm/checkpoints` 一键回滚、没有手机局域网扫码看盘审批，也没有无头滚屏长截图和跨 Agent 委派（`delegate`）。
- **“延迟和资源占用怎么样？”** → 纯 Rust 自研 `next-core` 内核 + wgpu 原生渲染，UI 渲染通道与后台控制通道物理隔离，前台聚焦窗格 165Hz 零延迟刷新（键入唤醒 `<2ms`），后台非可见窗格自动按 48ms 合并刷新省电。
- **“手机远程连接安全吗？会走你们的服务器吗？”** → 零云端！硬编码 CIDR 白名单仅放行局域网（RFC1918）与 Tailscale 网段（`100.64.0.0/10`），使用 6 位一次性短效配对码 + Ed25519 公私钥挑战签名，且服务端拦截一切高危破坏性指令。

---

## 即刻 / 小红书（AI 工具与独立开发者圈）

**配图建议**：15 秒动图或 3 张拼图（① 图形化 Web 设置面板一键接入全部 MCP 客户端 & 三级风险护栏；② 终端内弹出 Pocket 二维码 + 手机远程看盘审批；③ 命令报错后一键在右侧分屏拉起 Claude Code 自动修复）。

```text
做了一个有点“反共识”的纯 Rust 终端：Unterm（v0.71 已开源）。

当所有终端都在往里塞 AI 聊天框和月费订阅时，我选择反着做：终端内部零 AI，但把终端本身做成了 155 个方法的 MCP Server + 多 Agent 指挥座舱。

几个解决日常痛点的设计：
1. 一键把终端 MCP 挂载给本机 Claude Code / Codex / Gemini / Cursor / Antigravity，甚至支持主 Agent 调 `delegate` 委派其它 CLI Agent 协同干活；
2. 零污染隐藏 Git 检查点（refs/unterm/checkpoints）：Agent 改代码前自动打隐藏快照，不脏 git status，改崩了一键无损回滚；
3. 手机扫码远程看盘（Unterm Pocket）：局域网/Tailscale 扫码直连，离开工位也能在手机上看终端实时输出、点 Allow/Deny 审批；
4. 命令报错一键 AI 分屏诊断 + 后台长命令（>=10s）完成原生系统通知 + 会话分屏布局自动恢复。

MIT 开源，全本地无云端无遥测，macOS / Linux / Windows 全平台：https://unterm.app
```

---

## 掘金 / 少数派（深度长文大纲）

**标题候选**：
1. 《终端不需要内嵌 AI，终端需要成为多 Agent 的座舱——纯 Rust 终端 Unterm 架构与设计复盘》
2. 《同时驾驭 Claude Code、Codex 与 Gemini：我如何用 Rust 打造一个 155 方法的 MCP 终端座舱》

**文章结构**：
1. **痛点引入**：当开发者同时运行 5 个 CLI Agent 时，传统终端缺什么？（看不见状态、不敢开全自动、离开工位卡审批、跨 Agent 交接靠手抄）。
2. **反 Warp 的核心哲学**：终端是 30 年寿命的基础设施，大模型是 6 个月一换的组件；把终端做成标准化的 MCP 执行面（155 方法 / 16 命名空间）。
3. **关键工程实现**：
   - `unterm-core` 守护进程分离与 `<2ms` 零阻塞输入管线；
   - 零污染 Git 检查点（`git write-tree` + `refs/unterm/checkpoints`）如何做到不碰工作区 Index 也能秒级回滚；
   - `Unterm Pocket` 局域网/Tailscale 端到端 Ed25519 手机配对架构；
   - 跨 Agent 接力（Portable Handoff）与 `agent_session.delegate` 子任务协同。
4. **开源与体验地址**：`https://unterm.app` · `https://github.com/zhitongblog/unterm`。

---

## 知乎精准问题蹲点

搜索并附上实机截图回答以下高浏览问题：
- “Claude Code 怎么用效率最高？有哪些必备的 MCP Server？”
- “Warp 终端怎么样？有没有免费开源、不强制登录的替代品？”
- “如何同时让 Claude Code 和 Codex 协同开发一个项目？”
- “Rust 写的终端模拟器（Alacritty / WezTerm / Rio / Unterm）各自有什么特点？”
