---
layout: ../../layouts/Doc.astro
title: "v0.73: Why We Built a Pure-Rust GPU Terminal for Claude Code, Codex & Agy CLI"
subtitle: "How Unterm solves the 8 biggest native terminal pain points — from <2ms Windows input and zero cmd flashes to Git Worktree Fleet Arena, 16 languages, and direct drag-selection delete."
kicker: Blog / Release v0.73
date: 2026-11-27
---

Traditional terminals — Terminal.app, Windows Terminal, iTerm2, and Electron-based shells — were designed for a human typing one command at a time. The moment you run **Claude Code**, **OpenAI Codex CLI**, and **Antigravity (`agy`)** all day across 6 panes, every legacy bottleneck surfaces at once.

Between `v0.72.0` and `v0.73.1`, we focused relentlessly on making **Unterm** the fastest, cleanest, most reliable runtime and native assistant for AI Coding CLIs on macOS, Windows, and Linux — still 100% local, MIT-licensed, and $0 forever.

## The 8 Native Terminal Pain Points — Solved at the Kernel

1. **Typing lag, dropped keys & CJK IME glitches (`<2ms` input):** Dedicated non-blocking coalescing input worker (`unterm-input-tx`), Windows 1ms system timer (`timeBeginPeriod(1)`), DX12 Mailbox 1-frame latency, Win32 `WM_CHAR` orphan key recovery, and automatic orphan IME cleanup.
2. **Full-screen TUI tearing, flicker & broken `Shift+Enter`:** Pure-Rust GPU renderer with global `?2026` synchronized output, main-screen soft-wrap reflow on resize, ECMA-48 CJK wide-cell splitting, and Kitty keyboard protocol + `modifyOtherKeys`.
3. **Windows `cmd.exe` / `OpenConsole.exe` black-box flashes & `Ctrl+C` crashes:** Enforced `CREATE_NO_WINDOW` across every background subprocess and MCP CLI bridge, plus a dedicated hidden-console `Ctrl+C` helper and zero-spawn sidebar Git state caching.
4. **Parallel AI agents overwriting each other in the same repo:** **Native Fleet Arena & Git Worktree Studio** spins up 2–3 agents (Claude Code, Codex, Agy) in isolated Git worktrees in one click, shows live `+added -deleted` diffs, and 1-click squash-merges the winner.
5. **Lost in tabs waiting on `[y/n]` approval prompts:** **Agent Cockpit** live status dots, 6-line sidebar hover peeks, `Ctrl+Shift+I` cross-pane `Y`/`N` quick-approve without switching tabs, plus **Unterm Pocket** QR mobile approvals over LAN/Tailscale.
6. **Context overflow, scattered token bills & re-explaining tasks:** **Daily Telemetry HUD** aggregates tokens, USD cost, peak context %, and configurable daily budget alerts (`UNTERM_DAILY_BUDGET_USD`, 80% warning / 100% exceeded), with 1-click `/compact` and portable **Handoff (`H`)**.
7. **Digging through build logs & browser errors to feed AI:** **OSC 133** 1-click jump to last failed command block, `Ctrl+S` (`✦AI`) search-to-agent context feed, plus **Unzoo Browser** 1-click frontend error self-healing.
8. **Risky uncommitted edits & clunky terminal text editing:** Zero-pollution hidden Git checkpoints (`refs/unterm/checkpoints/*`), inline Diff single-file revert (`Cmd+Shift+G`), and — new in **`v0.73.1`** — **direct drag-selection delete (`Backspace`/`Delete`) & cut (`Cmd+X` / `Ctrl+Shift+X`)** inside active input lines with accurate CJK wide-character and soft-wrapped multi-line handling.

## Global 16-Language Parity

Desktop UI, `unterm-cli`, Web Settings, OS locale auto-detection, and `unterm.app` now ship with **16 global languages**: English, 简体中文, 繁體中文, 日本語, 한국어, Español, Português (Brasil), Deutsch, Français, Italiano, Русский, العربية (RTL on web), हिन्दी, Tiếng Việt, Bahasa Indonesia, and Türkçe.

## Get Started in 10 Seconds

```bash
# macOS / Linux
curl -fsSL https://unterm.app/install.sh | sh

# Wire Claude Code, Codex CLI, Agy & Cursor in one command
unterm-cli setup-ai && unterm-cli agent enable-hooks
```

- Download `v0.73.1`: [GitHub Releases](https://github.com/zhitongblog/unterm/releases/latest) · [AtomGit China Mirror](https://atomgit.com/zhitongblog/unterm/releases)
- Dedicated guides: [For Claude Code](/for/claude-code) · [For Codex CLI](/for/codex) · [For Antigravity (agy)](/for/agy) · [Unterm vs Windows Terminal](/compare/windows-terminal)
