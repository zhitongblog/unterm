# Unterm 一体化产品与架构升级规划（All-in-One 落地方案）

> **核心定位**：做 **AI Coding CLI（Claude Code / Codex / Gemini CLI / Antigravity `agy` / Qwen Code / OpenCode / Cursor / Aider）的最佳原生宿主与多智能体指挥台**——兼具原生 GPU 终端的 `<2ms` 极致输入响应与跨会话 AI 协同能力。

---

## 第一阶段（已发布于 `v0.71.26`）

1. **输入与渲染绝对零阻塞（Zero-Stall Input Pipeline）**
   - 物理隔离 IPC 通道（`ui_client` 专属连接）+ `Condvar` 事件唤醒（`6ms` 活跃帧率 + `2ms` 键入回显唤醒）+ `pane_modes` / `mcp_state` 内存级缓存。
2. **窗口内原生设置中心（Native In-Window Settings Center）**
   - 点击 `⚙` 或按 `Cmd+,` 直接在当前窗口内弹出原生设置面板（主题、字号、Shell、侧边栏、一键接入本机 AI Agent MCP、Web 控制台入口）。
3. **多 Agent 并行指挥闭环（Active Agent Inspection & Sidebar Hover Peek）**
   - `Ctrl+Shift+I` 实时抓取后台 Agent 终端末尾输出预览、支持 `Y`/`N` 跨分屏远程快捷审批、左侧边栏悬停预览卡（`append_sidebar_peek`）及 `agy` 专属矢量图标（`✦`）。
4. **按风险分级自治与控制台运行时（PR `#68`–`#72`）**
   - `Autonomy` 风险分级、多轮任务线程、专属受管浏览器（`~/.unterm/browser-profile`）、本地 CLI 登录、技能插件与后台任务 Core 保活。

---

## 第二阶段（Phase 2：原生 AI 护栏闭环、主动感知、安全粘贴与 Pocket 扫码直连）

### 支柱一：原生窗口内的「AI 安全护栏、一键无损回滚与跨 Agent 接力」
1. **原生「检查点与一键无损回滚」面板（Native Checkpoints & 1-Click Rollback）**
   - 在原生设置中心（`Cmd+,`）、命令面板（`Cmd+Shift+P`）及智能体巡检面板（`Ctrl+Shift+I` 内按 **`R`**）直接管理当前 Git 仓库的零污染隐藏检查点（`refs/unterm/checkpoints/<id>` 与 Cockpit 自动快照）。
   - 支持**一键立即创建快照检查点**、查看每个检查点的改动统计（`+lines / -lines`）、**一键无损回滚工作区**（回滚前自动生成备份检查点），以及在新分屏一键打开 `git diff` 预览。
2. **原生切换「风险自治等级 (`Autonomy`)」与结构化审批合一**
   - 在原生设置中心与命令面板中直接展示并一键切换全局/托管任务的 `Autonomy` 等级（**谨慎 `Cautious`** / **平衡 `Balanced`** / **全自动 `Autonomous`**）。
   - 在 `Ctrl+Shift+I` 巡检面板中按 `Y` / `N` 时，同时支持向 TUI 终端发送 `y\r`/`n\r` 以及对 `agent_session` 待审批工具调用执行 `agent_session.decide` 结构化放行/拒绝。
3. **跨 Agent 一键接力（1-Click Portable Agent Handoff）**
   - 在 `Ctrl+Shift+I` 巡检面板按 **`H`**（或从设置中心/命令面板触发），自动提取当前窗格的最近终端输出、`git status` 与 `git diff --stat`，打包为 `~/.unterm/handoffs/<id>.md` 便携交接包，并一键在同目录新建分屏拉起目标 Agent（`agy` / `claude` / `codex` / `gemini` / `opencode`）继续推进任务。

### 支柱二：多 Agent 后台值守的「主动感知与报错直达」
1. **失焦/后台时的智能系统通知与窗口关注提醒（Smart Native Notification & Attention）**
   - 实时追踪每个窗格的 Agent 状态跃迁：当窗口失焦或窗格处于后台标签页，且 Agent 进入 **等待审批 (`Waiting`)** 或从运行态进入 **已完成 (`Done`)** 时，自动弹出窗口内提醒横幅、发送系统级原生通知（Toast），并触发 Dock / 任务栏窗口关注提醒（`request_user_attention`）。
2. **终端输出文件路径与行列号 `Cmd+Click` / `Ctrl+Click` 一键直达**
   - 扩展终端内链接检测规则（`unterm-app/src/links.rs`），自动识别编译报错与堆栈中的 `path/to/file.rs:123:45`（支持相对路径与绝对路径）。
   - 在 macOS 上同时支持 `Cmd+Click` 与 `Ctrl+Click`，结合当前窗格的 `cwd` 解析真实文件路径，自动调用本机编辑器（`agy --goto`、`cursor --goto`、`code --goto`、`zed` 或系统默认打开方式）精准跳转到对应行。

### 支柱三：输入与大输出体验的防呆与功耗精修
1. **多行/高危粘贴安全拦截条（Safe Multi-line Paste Guard）**
   - 当向未开启 Bracketed Paste 的普通 Shell 粘贴多行文本，或粘贴包含高危指令（`rm -rf`、`sudo`、`git reset --hard`、`curl ... | sh` 等）的多行脚本时，自动弹出原生安全预览确认面板（显示行数与前 3 行内容，按 `Enter` 确认粘贴、`Esc` 取消），防止误把日志或多行命令直接回车执行。
2. **前后台自适应帧率分级（Foreground 165Hz / Background Batching）**
   - 在 `unterm-core::FrameCache` 中引入 `primary_pane` 焦点感知：当前聚焦窗格保持 `0ms` 延迟、`165Hz` 极速刷新；后台非可见窗格在高频刷屏时按 `48ms` 周期自动合并 `styled_frame` 拉取，切回前台时立即同步最新帧，兼顾前台极致跟手与后台低功耗。

### 支柱四：SSH 远程会话一等公民 & 原生窗口「Pocket 手机扫码配对」
1. **SSH 感知会话与一键重连/克隆分屏（SSH-Aware Panes & Reconnect）**
   - 自动识别 `ssh` 远程会话并提取目标主机（`SSH · user@host`），在侧边栏与巡检面板提供醒目标识，支持在设置中心或命令面板一键克隆同主机 SSH 新分屏或断线重连。
2. **原生窗口内直接渲染「Pocket 手机扫码配对」（In-Window GPU QR Overlay）**
   - 在 `unterm-settings::qrcode_svg` 中暴露二维码矩阵接口，并在 `unterm-app` 内实现原生 GPU 像素级二维码浮层（`Command::OpenPocketQr`）。
   - 在原生设置中心点选「📱 手机扫码远程看盘与审批 (Unterm Pocket)」即可在终端窗口中央直接渲染高清二维码、6 位配对码与局域网/Tailscale 地址，手机扫码即可远程查看各 Agent 终端并执行审批。

---

## 模块改动矩阵（Phase 2）

| 模块 | 文件 | 核心改动 |
| :--- | :--- | :--- |
| **前后台自适应帧缓存** | `unterm-core/src/lib.rs`、`unterm-app/src/engine_backend.rs` | `FrameCache::set_primary_pane` 焦点窗格零延迟刷新 + 后台高频刷屏 `48ms` 批次合并 |
| **原生二维码矩阵接口** | `unterm-settings/src/qrcode_svg.rs` | 新增 `render_matrix(text)` 供原生 GPU 渲染像素级清晰二维码 |
| **代码文件行列号链接直达** | `unterm-app/src/links.rs` | 识别 `file.ext:line:col`、支持 macOS `Cmd+Click`、结合窗格 `cwd` 唤醒编辑器跳转指定行 |
| **四大支柱原生窗口闭环** | `unterm-app/src/window.rs`、`unterm-app/src/palette.rs` | 原生检查点与回滚面板、风险自治等级切换、跨 Agent 接力（`H`）、多行高危粘贴安全拦截、Agent 状态跃迁系统通知、SSH 会话克隆与窗口内 GPU 渲染 Pocket 扫码浮层 |
