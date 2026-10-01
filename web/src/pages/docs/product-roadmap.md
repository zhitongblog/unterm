---
layout: ../../layouts/Doc.astro
title: Product roadmap
subtitle: "What is being built now, what comes after it, and what Unterm will not become. Current release: v0.71.17."
kicker: Docs / Product roadmap
date: 2026-10-01
---

## Strategy in one sentence

Unterm is a terminal that agents can drive and that shows you what the agents inside it are doing — local-first, MCP-driven, and vendor-neutral.

Most of what follows is terminal groundwork: the protocols modern command-line tools expect, and the install and update paths people expect from a daily driver. The agent surface (MCP, CLI, Agent Cockpit) already ships; see the [MCP reference](/docs/mcp-reference) and [Agent Cockpit](/docs/agent-cockpit).

## Shipped in v0.71.17

### Terminal protocols

- **Kitty keyboard protocol** (the "disambiguate" level, which is what agents and modern TUIs ask for) and xterm's **modifyOtherKeys**: Shift+Enter, Esc and Ctrl/Alt chords reach the program as themselves. Unterm answers the protocol's query with exactly the levels it implements.
- **Synchronized output (mode 2026)**: an update is shown whole or not at all, for at most 150 ms, so full-screen redraws stop tearing.
- **Colour queries** (OSC 4, 10, 11, 12) answered from the theme the window is drawing, and **XTVERSION**.
- **OSC 9;4 progress**, drawn as a bar under the tab in the sidebar.
- **Shell integration, injected automatically** into zsh, bash, fish and PowerShell: prompt and command marks with exit codes, so a failed command marks its tab. See [Shell integration](/docs/shell-integration).

### Windows

- **Git Bash** found wherever Git is installed, and **each WSL distribution** listed by name in the shell menu.

### Install and update

- **In-app updates**: `unterm-cli update`, the command palette's **Install update**, and Web Settings download, verify and install the new release, then restart Unterm. See [`update`](/docs/cli-reference#update).
- **Homebrew and Scoop**: `brew install --cask zhitongblog/tap/unterm`, `scoop bucket add zhitongblog https://github.com/zhitongblog/scoop-bucket` then `scoop install unterm`. winget (`zhitongblog.Unterm`) is submitted and listed once Microsoft accepts it.
- **Crash reports via a pre-filled GitHub issue**: nothing is sent unless you submit it yourself.

## Later

- **Inline images** — sixel, the kitty graphics protocol, and iTerm2's inline images.
- **SSH sessions** — remote panes that Unterm knows are remote, rather than a local `ssh` process.
- **Screen-reader accessibility.**

## What we are not doing

- No cloud AI subscription, account, or login.
- No chat panel inside the terminal. The agents you already use stay outside and drive it.
- No telemetry.

Progress is tracked in the [changelog](/changelog) and on [GitHub](https://github.com/zhitongblog/unterm/issues).
