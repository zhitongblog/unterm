---
layout: ../../layouts/Doc.astro
title: "Unterm v0.71.22: Unterm Pocket 移动端网关、Agent Harness、主屏重排与 xterm 网格对齐"
subtitle: "局域网/Tailscale 扫码配对与三级权限控制、零污染隐藏 Git 检查点、窗口缩放软换行重排与单元格级字形裁切。"
kicker: Blog / Release
date: 2026-11-05
---

> English version: [Unterm v0.71.22: Unterm Pocket, Agent Harness, Main-Screen Reflow & xterm Grid Alignment](/blog/mobile-harness-reflow-v07122/)

**Unterm v0.71.22** 带来了两条核心工程演进：

1. **Unterm Pocket 与 Agent Harness（`unterm-cli remote`）**——纯本地优先的局域网 / Tailscale 移动端远程网关，支持扫码配对与三级权限控制、本地开发服务器端口发现与反向代理预览、零污染隐藏 Git 检查点、跨 Agent 便携交接以及窗口 + A11y 控件树快照。
2. **主屏重排（Reflow）与 xterm 网格对齐**——宽度变化时自动重排软换行、高度缩小时保留光标所在行、全面对齐 xterm / ECMA-48 网格与光标语义，以及严格按单元格边界定位与裁切字形。

---

## 1. Unterm Pocket：局域网 / Tailscale 手机端远程驾驶舱

当 AI 编程 Agent 正在执行耗时任务时，开发者经常需要在离开电脑时用手机查看终端输出、审批高风险操作，或在真机上预览本地开发服，但又不希望终端数据经过任何第三方云端中转。

在 v0.71.22 中，Unterm 推出了 **Unterm Pocket**（可通过 `unterm-cli remote` 或 Web Settings 中的 **Remote / Mobile** 标签页开启）：

- **严格限定局域网与 Tailscale**：在 Socket 层直接拒绝非私有局域网（RFC1918）与非 Tailscale（`100.64.0.0/10`）网段的连接请求，绝不暴露公网。
- **一次性 6 位配对码与二维码**：配对码有效期 2 分钟且单次有效，配对后基于 Ed25519 主机密钥与 SHA-256 哈希令牌持久化设备授权，随时可一键吊销。
- **三级权限范围（`full` / `approve` / `readonly`）**：
  - `readonly`（只读监看）：仅允许查看实时终端屏幕、回滚历史与检查点列表，禁止一切写入；
  - `approve`（仅审批与对话）：允许审批/拒绝 Agent 操作并发送对话输入，但拦截信号发送与回滚；
  - `full`（完全控制）：开放全部远程驾驶舱能力，包含检查点恢复与窗口快照。

---

## 2. Agent Harness：本地预览代理、隐藏 Git 检查点与跨 Agent 交接

围绕多 Agent 协作与演进安全，v0.71.22 在 Web Settings、手机端与 `unterm-cli remote` 中同步落地了四大 Agent Harness 能力：

1. **本地开发服务器端口发现与预览代理（`/preview/<port>/`）**：自动扫描本机正在监听的 Web 开发端口（如 `:3000`、`:4321`、`:5173`、`:8080`），手机端配对后可直接通过内置反向代理一键打开预览。
2. **零污染隐藏 Git 检查点（`refs/unterm/checkpoints/<id>`）**：利用临时索引（`GIT_INDEX_FILE`）、`git write-tree` 与 `git commit-tree` 将当前工作区（含未跟踪新文件）完整快照至隐藏引用 `refs/unterm/checkpoints/<id>`，完全不触碰 `HEAD`、`.git/index` 或工作区状态；执行回滚前还会自动生成安全备份检查点。
3. **跨 Agent 便携交接包（`~/.unterm/handoffs/`）**：在严格保留用户新指令的前提下，按 Token 预算自动截取最近终端输出与 Git 状态摘要，生成标准 Markdown 交接文件，方便在 Claude Code、Codex CLI 与 Gemini CLI 之间无缝接力。
4. **窗口 + A11y 控件树快照（`~/.unterm/snapshots/`）**：一键捕获前台窗口截图（`.png`）与 UI Automation / System Events 无障碍控件树（`.a11y.json`），并生成可直接粘贴给 Agent 的结构化提示词片段。

---

## 3. 主屏软换行重排（Reflow）与 xterm 网格语义对齐

在终端分屏或调整窗口大小时，软换行文本如果不能随之重排，就会导致历史输出被截断或错位。v0.71.22 在 `unterm-engine` 中实现了完整的**主屏重排**：

- **逻辑行拼接与重折行**：窗口宽度变化时，将软换行连接的物理行还原为逻辑行，再按新列宽重新折行，完整保留硬换行、CJK 宽字符、字形簇与 SGR 样式属性。
- **高度缩小时保护光标行**：当窗口高度缩小时，优先将顶部行推入回滚缓冲区，确保光标所在行始终留在可视区域内。
- **全面对齐 xterm / ECMA-48 边界语义**：
  - 软换行标记归属行级（`ICH`/`DCH`/`ECH`/`SL`/`SR`/`EL 1`/`DECERA`/`DECFRA`/`DECSERA` 保留，`EL 0`/`EL 2`/`ED`/`DECSED` 清除）；
  - `DECLRMM` 左右边距与上下滚动区的光标移动及钳位规则与 xterm 完全一致；
  - `DECFRA` / `DECERA` / `DECSERA` 矩形区域指令在边界处自动拆分跨界宽字符；
  - 同步输出模式（`CSI ? 2026 h/l`）提升为全局状态，跨主/备用屏切换与视口滚动期间均保持生效；
  - `LF` / `VT` / `FF` 默认保留光标列坐标，并完整实现 `LNM`（`CSI 20 h/l`）换行模式。

---

## 4. 单元格级字形定位裁切与 Windows 零闪烁启动

- **按源单元格定位、反色与裁切字形**：`unterm-render` 严格按照字形簇所属的单元格计算坐标、块状光标反色与裁剪矩形，并将 HarfBuzz 26.6 塑形偏移四舍五入至整像素，杜绝超宽图标侵入右侧相邻字符。
- **回滚长图宽字符步进修复**：修复回滚 PNG 渲染器未跳过宽度为 0 的宽字符延续格、导致中文字符前进 3 列的问题。
- **Windows 启动零闪烁**：在 `ShowWindow` 前强制同步施加 `LWA_ALPHA=0` 分层透明与 DWM Cloak，直到首帧 GPU 画面提交后再揭开窗口，彻底消除启动瞬间的黑影或白框闪烁。

---

## 立即升级

直接运行 `unterm-cli update`，或通过 Homebrew（`brew upgrade --cask unterm`）、Scoop（`scoop update unterm`）及 [GitHub Releases](https://github.com/zhitongblog/unterm/releases/tag/v0.71.22) 下载升级。
