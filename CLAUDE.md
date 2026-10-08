# Unterm — 给在这个仓库里干活的 agent

## 这是什么

自研内核的终端，为 AI agent 直接操作而设计。

| crate | 是什么 |
|---|---|
| `unterm-core` | 独立 Core 进程。**会话住在这里**，0.68 起 MCP 也由它托管 |
| `unterm-engine` | next-core 终端模型 + 前端要实现的 trait |
| `unterm-app` | GUI 前端（winit + wgpu） |
| `unterm-render` | GPU 渲染 |
| `unterm-images` | 内嵌图片：kitty 图形协议、iTerm2 `OSC 1337 File=`、sixel 的解析/解码/栅格化与按绝对行锚定的图片表。内核只调用它，不持有像素 |
| `unterm-mcp` | 152 个 MCP 方法的 dispatcher 与 handler |
| `unterm-cli` | `unterm-cli`，兼 `mcp-stdio` 桥接 |
| `unterm-protocol` | 版本握手、state 目录、发现记录 |
| `unterm-gateway` | Action Gateway：每个方法的风险分级 |

0.60 起**已弃用 WezTerm 内核**（0.61.0 提交语："the kernel replacement,
finished"）。`wezterm-*` 只剩底层数据结构 crate（cell / surface /
escape-parser / bidi）。上游 MIT，义务跟随代码来源而非 crate 命名——
`LICENSE.md` 的双版权声明是最终方案，不再讨论。

历史线（考古用，勿在其中开发）：Tauri 2 + xterm.js 的 unterm-app 1.x
（WebView 渲染晃动不可修复）；`D:\code\unterm` 的 WezTerm fork（0.5x，
已冻结）。

## 硬规则

- **会话只能经 `unterm-core` 或它的 IPC 创建**。禁止在 GUI 内直接持有 PTY
  生命周期，禁止绕过 Core 保存 Screen 状态。
- **副作用入口走 Action Gateway**，不再往 `unterm-mcp/src/handler.rs` 堆
  全局状态。
- **新增 MCP 方法要过五道注册守卫**，每一道都是断言具体数字的测试，漏一处
  就红：`unterm-agents/src/mcp_meta.rs` 的 `MCP_METHODS`、
  `unterm-mcp/src/meta.rs`、`unterm-cli/src/reference.rs`、
  `unterm-gateway/src/lib.rs` 的风险分级、dispatcher 本身。

## 两个 state 目录不是一回事

坑过 supervisor（把活着的 Core 报成 absent）、MCP handler、以及五处文档。

| 目录 | 谁写 | 装什么 |
|---|---|---|
| `~/.unterm`（`state_dir`） | GUI 前端 | 实例注册表 `instances/<id>.json`、`server.json`、配置 |
| 平台数据目录（`core_state_dir`）<br>Windows `%LOCALAPPDATA%\Unterm` | Core | `core.json`：endpoint + token + mcp_port |

用 `state_path("core.json")` 找 Core 的记录**永远找不到**。
`UNTERM_STATE_DIR` 会同时覆盖两者，所以设了它的测试一律看不出这个 bug——
`unterm-protocol` 里那条**故意不设**它的测试就是为此。

## 单进程多窗口（0.68.2）

一个进程持有 N 扇窗，第二扇窗 587ms → 31ms（省掉重复的 `request_adapter`）。

- 窗口有自己的 id：`instance.new_window` 返回它，`instance.windows` 列出，
  `instance.focus` 接受它。id 由 GUI 一家发放，Core 是被告知。
- **一个会话属于一扇窗**。归属规则收在 `window_should_adopt`：已在本窗的
  留着、别的窗持有的不抢、分屏跟着源 pane 走、剩下的孤儿由前台窗收下。
- 关窗只关视图（D1），会话留在 Core 由剩下的窗接管；只有最后一扇窗才问
  退出（D2）。

设计与逐条验收：`docs/plans/2026-08-20-single-process-multi-window-design.md`。
macOS 的 D3 与崩溃隔离已于 2026-10-08 真机验收（见该文档第 8 节）：
关最后一扇空闲窗口在 macOS 上也是退出（改拍板，不留 Dock）；Cmd+Q 有程序
在跑时要确认，多扇窗也问。

## 无头

`unterm-core --headless` 不需要 GUI：监听、建会话、跑 MCP。它以实例名
`core` 出现在 `instance.list` 上（有 GUI 时按 mcp_port 去重），
`unterm-cli --instance core` 直达。写入默认被挡——没有窗口就没人能批准，
出路是 `mcp_trusted_agents` 或 `mcp_input_confirmation=never`。

## 自测要求

**每次开发完成后必须自测，确认功能正常再交付。** 不是「编译过了」就算完。

1. 编译无报错
2. 启动应用，确认终端正常渲染（GPU 渲染，零晃动）
3. 核心功能：输入输出、分屏、Tab 切换、TUI 程序无抖动
4. MCP 可连接（`unterm-cli session list`）
5. 改动涉及的功能逐项验证

**测试要串行**：`cargo test --workspace` 会有一批 `unterm-services` 的测试
失败——它们用 `std::env::set_var` 设进程级 state 目录，并行时互相踩。CI 跑
的是 `cargo test --release --workspace -- --test-threads=1`，本地也该这么跑。

改了 MCP 接口就要重编 **`unterm-core`**，不能只编 `unterm-app`——接口住在
Core 里。

## 打包与换装（Windows）

- **同版本号的 MSI 不替换文件**，装了等于没装。本机换装必须逐次升 patch
  版本号，共 5 处：`Cargo.toml`（workspace）、`unterm-agents`、
  `unterm-profile`、`unterm-settings`、`installer/Unterm.wxs`。
- 打包 `pwsh -File ci/build-msi.ps1`（需要 WiX 6 在 `.\.tools\wix.exe`）。
- 卸载/安装要 UAC；非交互会话里 `Start-Process -Verb RunAs` 会挂在等待确认。
- 免 UAC 想试新版：直接跑 `target/release/unterm.exe`。

## 其它踩过的

- **禁止用 PowerShell 读剪贴板**：`System.Windows.Forms.Clipboard` 会创建
  消息循环并抢走窗口焦点。用 Win32（`IsClipboardFormatAvailable` +
  `OpenClipboard` + `GetClipboardData(CF_DIB)`）。剪贴板图片是 DIB（BGR），
  需转 RGBA 再编码 PNG。
- **Core 的 TCP 连接并发安全**：轮询任务与主循环共享同一条连接，reader 和
  writer 必须用**同一个 Mutex** 包，保证每个请求-响应对是原子的。
- **exe 被占用**：先 `taskkill //F //IM unterm.exe`。`unterm-cli.exe` 也会
  被占——Claude Code 的 unterm MCP 桥接跑的就是
  `target/release/unterm-cli.exe mcp-stdio`。

## 约定

- 提交信息用中文，conventional commits：`feat:` / `fix:` / `refactor:` /
  `docs:` / `chore:` / `test:`
- 分支：`feat/xxx`、`fix/xxx`、`refactor/xxx`
- `web/` 子项目用 pnpm，不用 npm
