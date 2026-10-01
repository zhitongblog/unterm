---
layout: ../../layouts/Doc.astro
title: Product roadmap
subtitle: "What is being built now, what comes after it, and what Unterm will not become. Current release: v0.71.16."
kicker: Docs / Product roadmap
date: 2026-10-01
---

## Strategy in one sentence

Unterm is a terminal that agents can drive and that shows you what the agents inside it are doing — local-first, MCP-driven, and vendor-neutral.

Most of what follows is terminal groundwork: the protocols modern command-line tools expect, and the install and update paths people expect from a daily driver. The agent surface (MCP, CLI, Agent Cockpit) already ships; see the [MCP reference](/docs/mcp-reference) and [Agent Cockpit](/docs/agent-cockpit).

## In progress

### Terminal protocols

- **Kitty keyboard protocol** — unambiguous key reporting, so programs can tell `Ctrl+I` from `Tab` and see key releases.
- **Synchronized output (mode 2026)** — a program can ask for a frame to be drawn only once it has finished writing it, so full-screen redraws stop tearing.
- **OSC 10 / OSC 11 colour queries** — programs can ask for the foreground and background colour and pick a light or dark palette to match.
- **OSC 9;4 progress** — a command's progress, reported by the program and shown by Unterm.
- **Full OSC 133 shell integration with automatic injection** — prompt, command and output marks set up in the shells Unterm starts without editing your shell config, so command boundaries and exit codes are known without extra setup.

### Windows

- **WSL and Git Bash as first-class shells** — found and offered when you open a tab.

### Install and update

- **In-app updates** — today Unterm only checks GitHub for a newer release and tells you; it will download and install it too.
- **Package managers** — Homebrew, Scoop and winget.
- **Crash reports via a pre-filled GitHub issue** — after a crash, Unterm offers to open an issue with the details already filled in. Nothing is sent unless you submit it yourself.

## Later

- **Inline images** — sixel, the kitty graphics protocol, and iTerm2's inline images.
- **SSH sessions** — remote panes that Unterm knows are remote, rather than a local `ssh` process.
- **Screen-reader accessibility.**

## What we are not doing

- No cloud AI subscription, account, or login.
- No chat panel inside the terminal. The agents you already use stay outside and drive it.
- No telemetry.

Progress is tracked in the [changelog](/changelog) and on [GitHub](https://github.com/zhitongblog/unterm/issues).
