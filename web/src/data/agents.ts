// Single source of truth for high-intent AI Coding CLI landing pages:
//   /for/claude-code, /for/codex, /for/agy (and /<lang>/for/<agent>)
//
// Each entry powers the localized landing page, hreflang alternates,
// SoftwareApplication + BreadcrumbList + FAQPage JSON-LD, and GEO signals.

export interface AgentPainPoint {
  tag: string;
  tagZh: string;
  badge: string;
  problem: string;
  problemZh: string;
  fix: string;
  fixZh: string;
}

export interface AgentFaqItem {
  q: string;
  qZh: string;
  a: string;
  aZh: string;
}

export interface AgentWorkflow {
  title: string;
  titleZh: string;
  body: string;
  bodyZh: string;
  cmd: string;
}

export interface AgentLandingData {
  slug: string;
  name: string;
  vendor: string;
  accent: string;
  title: string;
  titleZh: string;
  desc: string;
  descZh: string;
  eyebrow: string;
  eyebrowZh: string;
  h1: string;
  h1Zh: string;
  subtitle: string;
  subtitleZh: string;
  quickStartCmd: string;
  pains: AgentPainPoint[];
  workflows: AgentWorkflow[];
  faqs: AgentFaqItem[];
}

export const AGENT_LANDINGS: AgentLandingData[] = [
  {
    slug: "claude-code",
    name: "Claude Code",
    vendor: "Anthropic",
    accent: "amber",
    title: "Best Terminal for Claude Code (macOS, Windows & Linux) — Unterm",
    titleZh: "Claude Code 最佳终端与运行环境（macOS / Windows / Linux）— Unterm",
    desc: "Fix Claude Code typing lag, TUI flicker, Shift+Enter submits, Windows cmd flashes, and multi-instance Git conflicts. Unterm gives Claude Code a <2ms Rust GPU core, Git Worktree Fleet Arena, live token HUD, and 1-key Y/N approvals.",
    descZh: "彻底解决 Claude Code 在原生终端下的打字延迟、全屏 TUI 闪屏撕裂、Shift+Enter 误发送与多开改代码冲突。Unterm 为 Claude Code 提供 <2ms 纯 Rust GPU 内核、Git Worktree 并发竞速、实时 Token 水位预警与跨窗格一键审批。",
    eyebrow: "UNTERM FOR CLAUDE CODE · ANTHROPIC",
    eyebrowZh: "UNTERM × CLAUDE CODE · 专为 ANTHROPIC CLAUDE CODE 优化",
    h1: "The best terminal runtime & native assistant for Claude Code.",
    h1Zh: "Claude Code 的最佳终端运行环境与原生助手。",
    subtitle: "Stop fighting TUI flicker, sticky Windows typing, lost [y/n] approval tabs, and git collisions when running multiple Claude Code sessions. Unterm wires Claude Code's hooks and MCP in one command.",
    subtitleZh: "告别高频刷屏闪烁、Windows 打字卡顿、切 6 个 Tab 找审批弹窗以及多开互相覆盖代码。一行命令自动接入 Claude Code 官方 Hooks 与 165 个本地 MCP 工具。",
    quickStartCmd: `# 1. Wire Unterm's local MCP server & Claude Code official hooks
unterm-cli setup-ai
unterm-cli agent enable-hooks

# 2. Race 2 Claude Code instances + Codex in isolated Git worktrees
unterm-cli fleet launch --agents claude,claude,codex "add rate limiting to /api/auth"`,
    pains: [
      {
        tag: "PAIN 01 · TUI FLICKER & SHIFT+ENTER SUBMITS",
        tagZh: "痛点 01 · Claude Code 全屏刷屏撕裂 & Shift+Enter 误发送",
        badge: "✓ ?2026 + Kitty",
        problem: "Claude Code's streaming Ink TUI tears borders in legacy terminals, clips wrapped diffs on resize, and treats Shift+Enter as Enter—sending half-written prompts.",
        problemZh: "Claude Code 高频流式渲染在传统终端下边框错位重影、缩放窗口截断 Diff，且 Shift+Enter 换行经常直接误发送半截提示词。",
        fix: "Unterm's pure-Rust GPU renderer implements global ?2026 synchronized output, main-screen soft-wrap reflow, ECMA-48 CJK cell splitting, and Kitty keyboard protocol + modifyOtherKeys.",
        fixZh: "纯 Rust GPU 内核完整实现 ?2026 同步输出原子帧提交、主屏软换行 Reflow、ECMA-48 宽字符精确切分与 Kitty 键盘协议，Shift+Enter 稳稳换行。",
      },
      {
        tag: "PAIN 02 · LOST IN TABS WAITING ON TOOL APPROVALS",
        tagZh: "痛点 02 · 多开 Tab 找不到哪个 Claude 卡在工具审批",
        badge: "✓ Cockpit + Pocket",
        problem: "Running 4–6 Claude Code tabs means wasting minutes hunting for the pane blocked on an edit or bash confirmation—and losing control when you step away.",
        problemZh: "同时开 4–6 个 Claude Code 窗口，切来切去才发现某个后台窗口早就卡在确认弹窗干等了 10 分钟，离开工位更是完全盲人摸象。",
        fix: "Live Agent Cockpit dots (working / needs-you / done), 6-line sidebar hover peeks, Ctrl+Shift+I cross-pane Y/N quick-approve without switching tabs, and Unterm Pocket QR mobile approvals.",
        fixZh: "Agent Cockpit 实时状态点 + 侧边栏悬停 6 行预览 + Ctrl+Shift+I 巡检浮层直接按 Y/N 跨窗格放行；离座扫码用 Unterm Pocket 手机端实时审批。",
      },
      {
        tag: "PAIN 03 · PARALLEL CLAUDE INSTANCES CLOBBERING GIT",
        tagZh: "痛点 03 · 多开 Claude Code 改同一仓库互相踩踏",
        badge: "✓ Fleet Arena",
        problem: "Two Claude Code instances in the same directory overwrite each other's files and trigger .git/index.lock errors.",
        problemZh: "在同一目录开两个 Claude Code 并发干活，文件互相覆盖、频繁报 .git/index.lock 锁冲突，根本没法并行提效。",
        fix: "Native Git Worktree Studio & Fleet Arena spins up isolated worktrees per Claude instance in one click, shows live +added -deleted diffs, and 1-click squash-merges the winner.",
        fixZh: "原生 Git Worktree 工作台与 Fleet Arena 一键为每个 Claude 分配独立分支与目录，实时展示 +added -deleted，一键择优 Squash 合并并自动清理。",
      },
      {
        tag: "PAIN 04 · CONTEXT OVERFLOW & UNTRACKED EDITS",
        tagZh: "痛点 04 · 上下文爆窗失忆、费用黑盒与改乱难撤销",
        badge: "✓ HUD + Checkpoints",
        problem: "Claude silently hits 90% context mid-refactor, token spend is scattered across panes, and a bad multi-file edit has no clean undo if you didn't commit first.",
        problemZh: "重构到一半突然上下文爆窗降智、各窗口 Token 账单分散；没建 commit 前被改乱十几个文件根本撤不回。",
        fix: "Daily Telemetry HUD tracks live tokens, USD cost, and context % with 85% 1-click /compact alerts; zero-pollution hidden Git checkpoints (refs/unterm/checkpoints/) let you roll back any edit in 1 click.",
        fixZh: "Daily Telemetry HUD 实时汇总 Token、估算费用与上下文水位（85% 高亮一键 /compact）；隐藏 Git 检查点 + Cmd+Shift+G 内嵌 Diff 支持单文件一键打回。",
      },
    ],
    workflows: [
      {
        title: "Official Claude Code Hooks + 165 MCP Tools",
        titleZh: "原生接入官方 Hooks 与 165 个本地 MCP 工具",
        body: "One command wires Claude Code's lifecycle hooks into Unterm's Cockpit for exact waiting/done detection and exposes local MCP pane control with --mcp-trim (~30k tokens saved per governed turn).",
        bodyZh: "一行命令自动配置 Claude Code 官方生命周期 Hooks 实现精准待审批感知，并注入本地 MCP 与 --mcp-trim（单轮省约 3 万 Token）。",
        cmd: "unterm-cli setup-ai && unterm-cli agent enable-hooks",
      },
      {
        title: "Feed Build Errors or Browser Crashes Back to Claude in 1 Click",
        titleZh: "终端报错与浏览器控制台异常一键回流给 Claude",
        body: "Use OSC 133 to jump to the last failed command block and send clean output straight to Claude, press Ctrl+S (✦AI) in search, or bridge frontend errors from Unzoo Browser.",
        bodyZh: "借助 OSC 133 一键跳至上次失败命令块并直投给 Claude，搜索条按 Ctrl+S（✦AI）投喂日志，或配合 Unzoo Browser 自动回传前端报错自愈。",
        cmd: "unterm-cli screen last-command --json",
      },
    ],
    faqs: [
      {
        q: "Why is Unterm better than Terminal.app, iTerm2, or Windows Terminal for Claude Code?",
        qZh: "为什么跑 Claude Code 用 Unterm 比 Terminal.app、iTerm2 或 Windows Terminal 更好？",
        a: "Native terminals lack ?2026 synchronized rendering, multi-agent Git worktree orchestration, live approval inboxes, context/cost HUDs, and hidden rollback checkpoints. Unterm builds all of those natively into a <2ms Rust GPU terminal with zero cloud lock-in.",
        aZh: "传统原生终端缺少 ?2026 原子帧防闪屏、多 Agent Git Worktree 隔离、跨窗格审批收件箱、Token 水位预警与隐藏检查点回滚。Unterm 将这些能力全部原生集成在纯 Rust GPU 内核中，零云端绑定、MIT 开源免费。",
      },
      {
        q: "Does Unterm fix Claude Code typing lag and cmd black-box flashes on Windows?",
        qZh: "Unterm 能解决 Windows 下跑 Claude Code 打字卡顿和闪黑窗吗？",
        a: "Yes. v0.72.1 combines a dedicated coalescing input worker (unterm-input-tx), 1ms Windows timer resolution, DX12 Mailbox 1-frame latency, Win32 WM_CHAR recovery, and enforced CREATE_NO_WINDOW across every subprocess and MCP probe.",
        aZh: "可以。v0.72.1 引入独立非阻塞合并输入线程（unterm-input-tx）、Windows 1ms 时钟、DX12 Mailbox 零帧排队与全链路 CREATE_NO_WINDOW，彻底根治打字发粘和后台闪 cmd 黑窗。",
      },
      {
        q: "Can I run Claude Code with China Coding Plans (GLM, DeepSeek, Bailian, Ark) in Unterm?",
        qZh: "Unterm 支持为 Claude Code 一键配置国内大模型 Coding Plan 吗？",
        a: "Yes. Unterm's signed Claude Code manifest supports both official Anthropic auth and cn_coding_plan presets (Zhipu GLM, Aliyun Bailian / DashScope, Volcengine Ark, DeepSeek, MiniMax, Moonshot) with automatic PATH repair.",
        aZh: "支持。Unterm 内置的 Claude Code 签名配置同时支持官方登录与国内 Coding Plan 预设（智谱 GLM、阿里云百炼、火山方舟、DeepSeek、MiniMax、月之暗面），并支持一键修复 PATH。",
      },
    ],
  },
  {
    slug: "codex",
    name: "Codex CLI",
    vendor: "OpenAI",
    accent: "teal",
    title: "Best Terminal for OpenAI Codex CLI (macOS, Windows & Linux) — Unterm",
    titleZh: "OpenAI Codex CLI 最佳终端与运行环境（macOS / Windows / Linux）— Unterm",
    desc: "Run OpenAI Codex CLI faster with zero typing lag, parallel Git Worktree Fleet Arena, live token & USD cost HUD, inline Git diff review, and hosted codex app-server integration in Unterm.",
    descZh: "让 OpenAI Codex CLI 如虎添翼：零延迟纯 Rust GPU 输入、Git Worktree 并发多开竞速、实时 Token 与费用 HUD、内嵌 Diff 审查与 codex app-server 原生托管。",
    eyebrow: "UNTERM FOR CODEX CLI · OPENAI",
    eyebrowZh: "UNTERM × CODEX CLI · 专为 OPENAI CODEX CLI 优化",
    h1: "The best terminal runtime & native assistant for OpenAI Codex CLI.",
    h1Zh: "OpenAI Codex CLI 的最佳终端运行环境与原生助手。",
    subtitle: "Supercharge Codex CLI with isolated Git worktree fleets, hosted codex app-server orchestration, portable handoffs between Codex and Claude Code, and 1-click hidden checkpoint rollbacks.",
    subtitleZh: "为 Codex CLI 提供隔离 Git Worktree 并发舰队、原生 codex app-server 托管协议、与 Claude Code 无缝任务接力以及零污染隐藏检查点一键回滚。",
    quickStartCmd: `# 1. Auto-configure Codex CLI with Unterm's local MCP server
unterm-cli setup-ai

# 2. Run Codex vs Claude Code head-to-head in isolated worktrees
unterm-cli fleet launch --agents codex,claude "refactor payment webhook retries"`,
    pains: [
      {
        tag: "PAIN 01 · SERIAL SINGLE-TASK WORKFLOW",
        tagZh: "痛点 01 · 单窗口串行等待，无法多任务并发",
        badge: "✓ Fleet Arena",
        problem: "Running one Codex CLI session per checkout forces you to sit idle while it thinks, runs tests, and edits files—or risk Git conflicts if you open a second tab.",
        problemZh: "一个仓库只能串行跑一个 Codex，思考与跑测试期间只能干等；强行多开 Tab 又会导致文件互相覆盖和 Git 冲突。",
        fix: "Fleet Arena launches 2–3 Codex (or Codex + Claude + Agy) sessions in isolated Git worktrees, shows live +added -deleted diffs side by side, and 1-click squash-merges the best take.",
        fixZh: "Fleet Arena 一键在隔离 Git Worktree 并行启动 2–3 个 Codex（或与 Claude/Agy 同台竞速），实时对比代码改动行数，一键择优合并并自动清理。",
      },
      {
        tag: "PAIN 02 · SWITCHING BETWEEN CODEX & CLAUDE CODE",
        tagZh: "痛点 02 · Codex 与 Claude Code 换手要手动复制半天上下文",
        badge: "✓ Portable Handoff",
        problem: "When Codex finishes the architecture or hits a snag and you want Claude Code (or Agy) to take over, you have to manually summarize terminal output and git status.",
        problemZh: "Codex 写完架构想换 Claude Code 或 Agy 接手细化时，需要手动复制一堆终端输出和 Git diff 重新解释背景。",
        fix: "Press H for Portable Handoff: Unterm packages the active pane's clean output, git diff summary, and cwd into a structured handoff packet for the next agent.",
        fixZh: "按 H 触发跨 Agent 便携接力（Handoff）：自动打包当前窗格纯净输出、Git 状态与工作目录，无缝交接给下一个 Agent。",
      },
      {
        tag: "PAIN 03 · REVIEWING LARGE AUTONOMOUS EDITS",
        tagZh: "痛点 03 · Codex 一次改十几个文件，审查与局部撤销繁琐",
        badge: "✓ Inline Diff + Checkpoints",
        problem: "Reviewing a 15-file Codex patch inside raw terminal scrollback is painful, and reverting just 2 noisy files without losing the rest takes manual git gymnastics.",
        problemZh: "在纯文本终端里审查 Codex 改动的十几个文件非常费眼，只想打回其中 2 个改坏的文件还要手敲一堆 git checkout。",
        fix: "Open Inline Git Diff Review (Cmd+Shift+G) right inside Unterm to inspect hunks, revert individual files in 1 click, or roll back the entire task to an automatic hidden checkpoint.",
        fixZh: "按 Cmd+Shift+G 直接在终端内打开内嵌 Git Diff 审查，支持单文件一键打回，或一键无损回滚到自动创建的隐藏检查点。",
      },
      {
        tag: "PAIN 04 · WINDOWS & HIGH-LOAD TYPING STUTTER",
        tagZh: "痛点 04 · Windows 下打字延迟、黑窗弹窗与日志难定位",
        badge: "✓ <2ms + OSC 133",
        problem: "Heavy build logs and Windows ConPTY quirks make typing sticky, flash stray console windows, and force endless scrolling to find test failures.",
        problemZh: "高负载日志刷屏与 Windows 底层时钟限制导致打字发粘、后台闪黑窗，测试报错后还要往上翻几百行找失败点。",
        fix: "Dedicated non-blocking input worker (<2ms), zero-popup CREATE_NO_WINDOW execution, and OSC 133 1-click jump to the last failed command block.",
        fixZh: "独立非阻塞输入线程（<2ms）、全链路静默零黑窗执行，以及 OSC 133 一键跳至上次失败命令块并直投 AI 诊断。",
      },
    ],
    workflows: [
      {
        title: "Native codex app-server & Hosted Governed Sessions",
        titleZh: "原生支持 codex app-server 与受管会话",
        body: "Unterm speaks OpenAI's codex app-server protocol over contracts/v0, giving you structured tool approvals, standing policy rules, and multi-turn session resumption.",
        bodyZh: "Unterm 原生对接 codex app-server 协议，支持结构化工具审批、常设放行规则与进程退出后的会话续接。",
        cmd: "unterm-cli agent status --json",
      },
      {
        title: "Daily Telemetry HUD & Remote SSH Roaming",
        titleZh: "跨会话费用 HUD 与 SSH 远程漫游",
        body: "Track daily Codex tokens, estimated USD cost, and context pressure across all panes—and roam seamlessly to remote Linux dev servers via Remote Core over SSH.",
        bodyZh: "实时聚合所有 Codex 窗格的每日 Token、估算费用与上下文压力；配合 Remote Core over SSH 在远程开发机上无缝延续本地体验。",
        cmd: "unterm-cli telemetry status",
      },
    ],
    faqs: [
      {
        q: "How do I run OpenAI Codex CLI and Claude Code together in one workspace?",
        qZh: "如何在同一个仓库里同时运行 Codex CLI 和 Claude Code？",
        a: "Use Unterm's Fleet Arena (unterm-cli fleet launch --agents codex,claude \"your task\"). Unterm creates an isolated Git worktree and branch for each agent, tracks both in the Cockpit, and lets you squash-merge the winning diff in one click.",
        aZh: "使用 Unterm 的 Fleet Arena（unterm-cli fleet launch --agents codex,claude \"任务描述\"），系统会自动为每个 Agent 创建隔离的 Git Worktree 与分支，实时追踪进度并支持一键择优合并。",
      },
      {
        q: "Does Unterm support Codex CLI on Windows without WSL?",
        qZh: "在 Windows 原生环境下（不装 WSL）也能流畅跑 Codex CLI 吗？",
        a: "Yes. Unterm ships native Windows x64 and ARM64 installers with DX12 Mailbox rendering, 1ms input cadence, ConPTY passthrough on Windows 11, and zero stray cmd window flashes.",
        aZh: "完全可以。Unterm 提供 Windows x64/ARM64 原生安装包，支持 DX12 Mailbox 渲染、1ms 输入响应、Win11 ConPTY 直通与全链路零黑窗静默执行。",
      },
    ],
  },
  {
    slug: "agy",
    name: "Antigravity (agy)",
    vendor: "Google DeepMind",
    accent: "indigo",
    title: "Best Terminal for Antigravity (agy CLI) — Zero Flicker, Zero Windows Flashes | Unterm",
    titleZh: "Antigravity (agy CLI) 最佳终端与运行环境 — 零闪屏、零黑窗 | Unterm",
    desc: "First-party signed support for Google Antigravity (agy CLI). Fixes jumping TUI input boxes, cursor drift on wide CJK chars, Windows cmd black-box flashes, and typing lag with a pure-Rust GPU core.",
    descZh: "一线原生适配 Google Antigravity (agy CLI)：彻底修复 TUI 输入框乱跳、宽字符光标错位、Windows 启动闪 cmd 黑窗与高负载打字延迟，内置 agy acp 协议与多 Agent 协同。",
    eyebrow: "UNTERM FOR ANTIGRAVITY (AGY) · GOOGLE DEEPMIND",
    eyebrowZh: "UNTERM × ANTIGRAVITY (AGY CLI) · 一线原生深度适配",
    h1: "The best terminal runtime & native assistant for Antigravity (agy CLI).",
    h1Zh: "Antigravity (agy CLI) 的最佳终端运行环境与原生助手。",
    subtitle: "Built hand-in-hand with daily agy CLI usage: eliminates jumping input boxes, Windows cmd popup flashes on launch, CJK cursor glitches, and adds Fleet Arena, Telemetry HUD, and mobile approvals.",
    subtitleZh: "基于高频实战深度打磨：根治 agy 全屏 TUI 输入框乱跳、Windows 启动一闪而过的 cmd 黑框、宽字符光标错位与打字卡顿，并解锁 Fleet Arena、费用 HUD 与手机审批。",
    quickStartCmd: `# 1. Register Unterm's MCP server & PATH for agy CLI
unterm-cli setup-ai
unterm-cli agent repair-path

# 2. Launch agy alongside Claude Code & Codex in Fleet Arena
unterm-cli fleet launch --agents agy,claude "build end-to-end auth tests"`,
    pains: [
      {
        tag: "PAIN 01 · WINDOWS CMD BLACK-BOX FLASH ON LAUNCH",
        tagZh: "痛点 01 · Windows 每次打开 agy 都会一闪而过 cmd 黑框",
        badge: "✓ 0 Popups (v0.72.1)",
        problem: "On Windows, launching agy or running its MCP bridge spawns stray cmd/OpenConsole windows that flash across your screen, and Ctrl+C can drop background Core sessions.",
        problemZh: "Windows 下打开 agy 或触发 MCP 桥接时桌面频繁弹出并一闪而过 cmd 黑窗，甚至按 Ctrl+C 会连带误杀后台服务。",
        fix: "Unterm v0.72.1 enforces CREATE_NO_WINDOW across every CLI/MCP subprocess (i18n, mcp-trim, remote-core, update) plus a dedicated hidden-console Ctrl+C helper—100% silent on Windows.",
        fixZh: "Unterm v0.72.1 在全链路子进程与 MCP 桥接中强制启用 CREATE_NO_WINDOW，并配备独立隐藏控制台 Ctrl+C 助手，全天候静默零黑框。",
      },
      {
        tag: "PAIN 02 · JUMPING INPUT BOX & WIDE-CHAR CURSOR DRIFT",
        tagZh: "痛点 02 · agy TUI 输入框乱跳、宽字符竖线光标错位",
        badge: "✓ ECMA-48 + ?2026",
        problem: "During fast streaming updates, agy's input box jumps around, bar cursors snap backwards on CJK characters, and shell ghost predictions overwrite the agent prompt.",
        problemZh: "高频输出时 agy 输入框乱跑、中文宽字符上的竖线光标错误前跳、Shell 幽灵预测文字覆盖输入框。",
        fix: "Unterm preserves cursor columns across synchronized-output linefeeds, aligns bar cursors strictly to ECMA-48 wide-cell boundaries, and automatically yields shell ghost text to agy's prompt.",
        fixZh: "Unterm 在同步输出换行时严格保留光标列坐标、按 ECMA-48 宽字符边界精确绘制光标，并对 agy 自动让位禁用 Shell 幽灵预测。",
      },
      {
        tag: "PAIN 03 · STICKY TYPING UNDER HEAVY AGENT OUTPUT",
        tagZh: "痛点 03 · 后台跑多个 Agent 时在 agy 里打字发粘、掉字",
        badge: "✓ <2ms Input",
        problem: "Windows 15.6ms timer quantization and synchronous GUI frame waits make typing in agy feel sluggish or drop fast keystrokes and IME commits.",
        problemZh: "Windows 15.6ms 默认时钟粒度与同步帧等待导致在 agy 中快敲打字发粘、丢字，中文输入法选词残留。",
        fix: "Dedicated coalescing input thread (unterm-input-tx), 1ms Windows timer resolution, DX12 Mailbox 1-frame latency, and Win32 WM_CHAR orphan key recovery.",
        fixZh: "独立非阻塞合并输入线程（unterm-input-tx）+ Windows 1ms 系统时钟 + DX12 Mailbox 零帧排队 + Win32 WM_CHAR 孤儿字符回收，打字丝滑跟手。",
      },
      {
        tag: "PAIN 04 · MULTI-AGENT ORCHESTRATION & HUMAN HANDOVER",
        tagZh: "痛点 04 · 多 Agent 协同委派与登录/验证码真人接管",
        badge: "✓ agy acp + Handover",
        problem: "When agy needs to delegate a subtask to Claude/Codex or pauses on a login/2FA/CAPTCHA step, vanilla terminals offer no structured delegation or live takeover.",
        problemZh: "当 agy 需要委派子任务给 Claude/Codex，或遇到登录/验证码需要真人介入时，原生终端缺乏结构化委派与实时接管机制。",
        fix: "First-party agy acp adapter, agent_session.delegate for cross-agent subtask dispatch with auto-cleaned temp tabs, and agent_session.handover for seamless human takeover.",
        fixZh: "原生内置 agy acp 协议适配、agent_session.delegate 跨 Agent 子任务委派与临时标签页自动回收，以及 agent_session.handover 真人实时接管续跑。",
      },
    ],
    workflows: [
      {
        title: "First-Party Signed agy Manifest & 1-Click PATH Repair",
        titleZh: "一线内置 agy 签名配置与一键修复 PATH",
        body: "Unterm recognizes agy natively in the sidebar, status bar, and Agent Cockpit—and unterm-cli agent repair-path fixes missing PATH entries across zsh, bash, fish, and Windows User Path.",
        bodyZh: "侧边栏、状态栏与 Agent Cockpit 原生识别 agy，unterm-cli agent repair-path 一键幂等修复 macOS / Linux / Windows 环境变量。",
        cmd: "unterm-cli agent repair-path",
      },
      {
        title: "Unzoo Browser Frontend Self-Healing Loop",
        titleZh: "联动 Unzoo Browser 实现前端报错自动闭环",
        body: "When your dev server runs on localhost, Unterm shows a 1-click Unzoo preview pill and bridges browser console/network errors straight back into your agy pane.",
        bodyZh: "自动嗅探本地开发端口并展示一键预览胶囊，浏览器控制台与网络报错一键回流至 agy 窗格自动定位修复。",
        cmd: "unterm-cli unzoo status",
      },
    ],
    faqs: [
      {
        q: "How does Unterm fix the flashing cmd black window when opening agy on Windows?",
        qZh: "Unterm 是怎么彻底解决 Windows 下打开 agy 闪 cmd 黑框的？",
        a: "In v0.72.1, Unterm enforces Windows CREATE_NO_WINDOW (0x08000000) across every unterm-cli subprocess (including MCP stdio/trim bridges, locale probes, and update checks) and routes Ctrl+C through a dedicated hidden-console helper.",
        aZh: "在 v0.72.1 中，Unterm 在 unterm-cli 所有后台子进程（包括 MCP 桥接、语言探测、远程核心与更新检查）强制传入 CREATE_NO_WINDOW，并通过独立隐藏控制台助手处理 Ctrl+C，彻底消除黑框闪烁。",
      },
      {
        q: "Can agy drive Unterm panes and delegate work to Claude Code or Codex?",
        qZh: "agy 可以通过 MCP 控制 Unterm 分屏并委派任务给 Claude Code / Codex 吗？",
        a: "Yes. Running unterm-cli setup-ai registers Unterm's 165-method local MCP server with agy, enabling pane creation, command execution, scrollback reading, screenshots, and cross-agent delegation (agent_session.delegate).",
        aZh: "可以。运行 unterm-cli setup-ai 后，agy 即可直接调用 Unterm 的 165 个本地 MCP 工具来开分屏、跑命令、读屏幕、截图，并通过 agent_session.delegate 将子任务委派给其他 Agent。",
      },
    ],
  },
];

export function getAgentLanding(slug: string): AgentLandingData | undefined {
  return AGENT_LANDINGS.find((a) => a.slug === slug);
}

const ORIGIN = "https://unterm.app";

export function agentAlternates(
  slug: string,
  locales: { code: string; href: string }[],
): { code: string; href: string }[] {
  return locales.map((l) => {
    const prefix = l.href === "/" ? "" : l.href.replace(/\/$/, "");
    return { code: l.code, href: `${ORIGIN}${prefix}/for/${slug}` };
  });
}

export function agentJsonLd(
  agent: AgentLandingData,
  lang: string,
  pathPrefix: string,
) {
  const isZh = lang === "zh-CN";
  const url = `${ORIGIN}${pathPrefix}/for/${agent.slug}`;
  const home = `${ORIGIN}${pathPrefix || "/"}`;
  const title = isZh ? agent.titleZh : agent.title;
  const description = isZh ? agent.descZh : agent.desc;

  return [
    {
      "@context": "https://schema.org",
      "@type": "BreadcrumbList",
      itemListElement: [
        { "@type": "ListItem", position: 1, name: "Unterm", item: home },
        { "@type": "ListItem", position: 2, name: `Unterm for ${agent.name}`, item: url },
      ],
    },
    {
      "@context": "https://schema.org",
      "@type": "SoftwareApplication",
      name: `Unterm for ${agent.name}`,
      applicationCategory: "DeveloperApplication",
      operatingSystem: "macOS, Windows, Linux",
      softwareVersion: "0.73.0",
      url,
      downloadUrl: "https://github.com/zhitongblog/unterm/releases/latest",
      description,
      inLanguage: lang,
      license: "https://opensource.org/licenses/MIT",
      offers: { "@type": "Offer", price: "0", priceCurrency: "USD" },
      publisher: { "@type": "Organization", name: "Unzoo", url: `${ORIGIN}/` },
    },
    {
      "@context": "https://schema.org",
      "@type": "FAQPage",
      mainEntity: agent.faqs.map((f) => ({
        "@type": "Question",
        name: isZh ? f.qZh : f.q,
        acceptedAnswer: {
          "@type": "Answer",
          text: isZh ? f.aZh : f.a,
        },
      })),
    },
  ];
}
