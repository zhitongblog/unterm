# Unterm 一体化产品与架构升级规划（All-in-One 落地方案）

> **核心定位**：做 **AI Coding CLI（Claude Code / Codex / Gemini CLI / Antigravity `agy` / Cursor / Aider）的最佳原生宿主与多智能体指挥台**——兼具原生 GPU 终端的 `<2ms` 极致输入响应与跨会话 AI 协同能力。

---

## 一、核心痛点诊断与设计原则

1. **输入与渲染绝对零阻塞（Zero-Stall Input Pipeline）**
   - **根因**：`unterm-app` 的主线程输入（`write_input` / `paste_input` / `pane_modes`）与后台 `statsbar` 的 4 个 `stats-facts` 进程树扫描线程共用同一个 `CORE_SHARED.client`（单 `Mutex<TcpStream>` 连接）。当后台线程调用 `session.activity` 扫描系统进程树（耗时 50–300ms）时，主线程按键写入会被阻塞在 Socket 锁外，造成偶发打字延迟。
   - **方案**：
     - **物理隔离 IPC 通道**：为 GUI 主线程建立专属的 **`input_client` 低延迟独立通道**，与后台巡检/统计线程的 `client` 彻底分离，确保键盘输入永不排队等待后台进程扫描。
     - **服务端写入即时推送 + `6ms` 高刷轮询**：`unterm-core` 处理 `session.write` / `session.paste` 后立即唤醒事件流，并将订阅态帧检测周期缩短至 `6ms`（120Hz 级）。
     - **前端状态内存化**：对 `pane_modes`、`pending_suggestions`、`insights` 及窗口标题会话元数据做内存级缓存，消除每帧与每次按键的冗余同步 RPC。

2. **终结“点设置跳外部浏览器”的割裂感（Native Settings Center）**
   - **根因**：点击顶栏或侧边栏底部的 `⚙ 设置`（或按 `Cmd+,`）完全依赖 `open http://127.0.0.1:19877` 唤起外部浏览器，不仅打断心流，且在系统默认浏览器异常时完全失效。
   - **方案**：
     - 构建 **原生内嵌设置中心（Native Settings Palette）**，点击 `⚙` 直接在当前窗口内弹出，支持一键实时调整：
       - 🎨 **切换配色主题**（含实时预览）
       - 🔤 **放大 / 缩小 / 重置终端字号**
       - 🐚 **切换默认 Shell**
       - 📐 **显示 / 隐藏左侧会话栏**
       - 🤖 **一键自动关联本机 AI Agent（Auto-Connect MCP：Claude / Codex / Gemini / `agy`）**
       - 🌐 **在浏览器中打开完整 Web 控制台与高级设置**（作为兜底入口保留）

3. **多 Agent 并行指挥闭环：侧边栏悬停预览 + 快速审批（Quick Peek & Approve）**
   - **根因**：同时开 5–8 个终端跑 `agy` / `claude` / `codex` 时，用户需要频繁切换标签页查看进度或按 `y/n` 审批；原“Agent 收件箱”命名费解且常为空框。
   - **方案**：
     - **全量一等公民识别 `agy` (Antigravity)**：在进程检测、标题提取、Fleet 编队和侧边栏图标中全面支持 `agy` / `antigravity`。
     - **侧边栏悬停实时预览卡（Sidebar Hover Peek）**：鼠标悬停在左侧边栏任意非当前标签页上时，右侧即时浮现该会话的**最后 6 行实时终端输出预览 + Agent 状态 + 快捷审批提示**，无需切换标签页即可掌控全局。
     - **活跃 Agent 巡检与跨窗格一键拍板（Quick Approve）**：
       - 在 `⚡ Cockpit`（活跃 Agent 巡检）面板中直接展示选中 Agent 的**实时终端尾部输出预览**；
       - 支持在巡检面板内直接按 **`Y`（允许/确认）** 或 **`N`（拒绝）** 向目标后台窗格发送确认指令，或按 **`W`** 一键创建隔离 Git Worktree 会话，按 **`C`** 打开 Web 驾驶舱。

---

## 二、架构模块改动矩阵

| 模块 | 文件 | 核心改动 |
| :--- | :--- | :--- |
| **IPC 双通道隔离与缓存** | `unterm-app/src/engine_backend.rs` | 新增专属 `input_client` 隔离键盘输入与后台 `stats-facts` 线程锁争用；缓存 `pane_modes` 与 `mcp_state` |
| **核心事件流低延迟推送** | `unterm-core/src/lib.rs` | `session.write` / `session.paste` 写入后即时触发 `ScreenUpdated` 检查；订阅态轮询周期降至 `6ms` |
| **原生设置中心 + 悬停预览 + 快速审批** | `unterm-app/src/window.rs`、`unterm-app/src/palette.rs` | `⚙` 唤起原生设置中心（含一键配置 AI MCP）；实现左侧边栏悬停终端预览卡（`append_sidebar_peek`）；`活跃 Agent 巡检` 内嵌尾部预览与 `Y`/`N` 跨窗格一键拍板 |
| **AI Agent 全链路支持 (`agy`)** | `unterm-services/src/cockpit/fleet.rs`、`unterm-app/src/sidebar.rs` | Fleet 编队检测与命令生成原生支持 `agy`；侧边栏与状态栏一等公民识别 |
| **多语言文案规范化** | `unterm-services/src/i18n/locales/*.json` | 统一“活跃 Agent 巡检”、原生设置项与快捷审批提示文案 |
