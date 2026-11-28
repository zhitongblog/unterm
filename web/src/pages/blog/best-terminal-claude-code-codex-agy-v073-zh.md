---
layout: ../../layouts/Doc.astro
title: "v0.73：为什么我们用纯 Rust 为 Claude Code、Codex 与 Agy CLI 重写了一款 GPU 终端"
subtitle: "从根治 Windows 15.6ms 打字延迟与闪 cmd 黑窗，到 Fleet Arena 多 Agent 竞速、全球 16 语言支持与输入框拖选直接删除。"
kicker: 博客 / Release v0.73
date: 2026-11-27
---

传统终端（Terminal.app、Windows Terminal、iTerm2 与 Electron 终端）都是为「人工敲一行、看一屏输出」设计的。当你全天候同时运行 **Claude Code**、**OpenAI Codex CLI** 与 **Antigravity (`agy`)** 时，底层瓶颈立刻暴露无遗。

从 `v0.72.0` 到 `v0.73.1`，我们把 **Unterm** 全面打磨成 **Claude Code · Codex · Agy CLI 的最佳纯 Rust GPU 运行环境与原生助手**——坚持 100% 本地运行、MIT 开源、永久免费。

## 从内核根治原生终端跑 AI Coding CLI 的 8 大痛点

1. **<2ms 零延迟输入与中文 IME 自愈**：独立非阻塞合并输入线程（`unterm-input-tx`）+ Windows 1ms 系统时钟（`timeBeginPeriod(1)`）+ DX12 Mailbox 零帧排队 + Win32 `WM_CHAR` 孤儿字符回收。
2. **全屏 TUI 零闪屏撕裂与快捷键完整支持**：纯 Rust GPU 渲染引擎完整实现全局 `?2026` 同步输出、主屏软换行 Reflow、ECMA-48 宽字符精确切分与 Kitty 键盘协议（`Shift+Enter` 稳稳换行不误发）。
3. **Windows 零 cmd 黑窗弹窗**：全链路子进程与 MCP 桥接强制 `CREATE_NO_WINDOW` + 独立隐藏控制台 `Ctrl+C` 助手 + 零子进程侧边栏 Git 缓存。
4. **Fleet Arena 隔离 Git Worktree 多 Agent 竞速**：一键并行启动 2–3 个 Agent 零冲突改代码，实时看 `+added -deleted`，一键择优 Squash 合并并自动清理。
5. **Agent Cockpit + Unterm Pocket 手机审批**：实时状态点 + 悬停 6 行预览 + `Ctrl+Shift+I` 跨窗格按 `Y`/`N` 放行，离座扫码手机实时审批。
6. **Daily Telemetry HUD + 每日费用预算告警**：跨会话聚合 Token、估算费用、上下文水位（85% 一键 `/compact`），支持配置 `UNTERM_DAILY_BUDGET_USD` 80%/100% 预警与跨 Agent 便携接力（`H`）。
7. **OSC 133 失败命令块跳转 + Search-to-AI + Unzoo Browser 自愈**：一键跳至上次报错命令块、搜索条按 `Ctrl+S`（`✦AI`）直投上下文，联动 Unzoo Browser 自动回流前端控制台报错。
8. **隐藏 Git 检查点一键回滚 + 输入框拖选直接删除（`v0.73.1` 新增）**：在终端与 Agent 输入行里用鼠标拖选一段文字，直接按 `Backspace`/`Delete` 精准删除选区（或 `Cmd+X` / `Ctrl+Shift+X` 剪切），精确处理中英文宽字符与软换行多行输入，自动避让历史输出区和 `vim`/`less`/`htop`。

## 桌面端、CLI 与官网同步支持全球 16 种主要语言

全面覆盖 English、简体中文、繁體中文、日本語、한국어、Español、Português (Brasil)、Deutsch、Français、Italiano、Русский、العربية（网页端 RTL）、हिन्दी、Tiếng Việt、Bahasa Indonesia 与 Türkçe。

## 10 秒上手

```bash
# macOS / Linux 一键安装（中国大陆高速镜像）
curl -fsSL https://unterm.app/install.sh | sh -s -- --cn

# 一键接入本机所有 AI Coding CLI
unterm-cli setup-ai && unterm-cli agent enable-hooks
```

- 下载 `v0.73.1`：[GitHub Releases](https://github.com/zhitongblog/unterm/releases/latest) · [AtomGit 中国大陆免登录高速直链](https://atomgit.com/zhitongblog/unterm/releases)
- 专属指南：[Claude Code 最佳终端](/zh-CN/for/claude-code) · [Codex CLI 最佳终端](/zh-CN/for/codex) · [Antigravity (agy) 最佳终端](/zh-CN/for/agy) · [Unterm vs Windows Terminal](/zh-CN/compare/windows-terminal)
