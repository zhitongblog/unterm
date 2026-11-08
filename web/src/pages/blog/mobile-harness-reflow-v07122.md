---
layout: ../../layouts/Doc.astro
title: "Unterm v0.71.22: Unterm Pocket, Agent Harness, Main-Screen Reflow & xterm Grid Alignment"
subtitle: "LAN/Tailscale QR pairing with scoped permissions, zero-pollution hidden Git checkpoints, soft-wrap reflow on resize, and strict per-cell glyph clipping."
kicker: Blog / Release
date: 2026-11-05
---

> 中文版: [Unterm v0.71.22: Unterm Pocket 移动端网关、Agent Harness、主屏重排与 xterm 网格对齐](/blog/mobile-harness-reflow-v07122-zh/)

**Unterm v0.71.22** ships two major tracks of engineering:

1. **Unterm Pocket & Agent Harness (`unterm-cli remote`)** — a local-first LAN / Tailscale mobile gateway with scoped permissions, localhost dev-server reverse proxying, zero-pollution hidden Git checkpoints, cross-agent handoffs, and foreground window + Accessibility (A11y) snapshots.
2. **Main-Screen Reflow & xterm Grid Alignment** — soft-wrap line reflow on width changes, cursor-preserving height shrink, strict xterm/ECMA-48 grid semantics (`DECLRMM`, `DECFRA`/`DECERA`/`DECSERA`, global `?2026`, `LNM`, `XTSAVE`/`XTRESTORE`), and per-cell glyph placement and clipping.

---

## 1. Unterm Pocket & Scoped Mobile Remote Access

When an AI coding agent is running a long task, you often want to monitor its progress, approve a gated action, or preview a dev server from your phone without routing your terminal through a third-party cloud relay.

In v0.71.22, Unterm introduces **Unterm Pocket** (`unterm-cli remote` and the **Remote / Mobile** tab in Web Settings):

- **LAN & Tailscale Only**: Connections are restricted at the socket layer to private RFC1918 LAN addresses and Tailscale (`100.64.0.0/10`) CIDRs.
- **One-Time 6-Digit Pairing & QR Code**: Pairing codes expire in 2 minutes and are consumed on first use, issuing an Ed25519-backed device record and SHA-256 hashed bearer token.
- **Three Permission Scopes (`full`, `approve`, `readonly`)**:
  - `readonly`: Watch live terminal panes, scrollback, and checkpoints without write access;
  - `approve`: Approve or reject pending agent actions and steer prompts, while blocking destructive signals or checkpoint rollbacks;
  - `full`: Full remote cockpit control, including localhost dev-server preview and checkpoint restoration.

---

## 2. Agent Harness: Preview Proxy, Hidden Git Checkpoints & Portable Handoffs

Alongside mobile access, v0.71.22 adds four Agent Harness primitives accessible from Web Settings, mobile devices, and `unterm-cli remote`:

1. **Localhost Dev-Server Discovery & Preview Proxy (`/preview/<port>/`)**: Automatically discovers listening local dev ports (`:3000`, `:4321`, `:5173`, `:8080`, etc.) and proxies them over your authenticated LAN/Tailscale session so you can test mobile layouts directly on your phone.
2. **Zero-Pollution Hidden Git Checkpoints (`refs/unterm/checkpoints/<id>`)**: Snapshots the entire working tree—including untracked files—using a temporary index (`GIT_INDEX_FILE`), `git write-tree`, and `git commit-tree`, storing the commit under `refs/unterm/checkpoints/<id>` without touching `HEAD`, `.git/index`, or `git status`. Restoring a checkpoint automatically creates a safety backup checkpoint first.
3. **Cross-Agent Portable Handoffs (`~/.unterm/handoffs/`)**: Packages a pane's recent terminal scrollback, Git status/diff summary, and your full new instruction into a token-budgeted Markdown bundle ready to hand off between Claude Code, Codex CLI, and Gemini CLI.
4. **Window + Accessibility (A11y) Snapshots (`~/.unterm/snapshots/`)**: Captures the foreground window PNG alongside its UI Automation / System Events Accessibility control tree (`.a11y.json`) and formats a one-line prompt snippet for visual debugging.

---

## 3. Main-Screen Reflow & xterm Grid Semantics

Resizing a terminal window or splitting a pane mid-session used to clip or misalign soft-wrapped lines. In v0.71.22, `unterm-engine` implements full **main-screen reflow**:

- **Logical Line Re-wrapping**: Soft-wrapped rows are rejoined into logical lines and re-wrapped to the new column width while preserving hard line breaks, wide CJK characters, grapheme clusters, and SGR attributes.
- **Cursor-Tracking Height Shrink**: When a window's height shrinks, the viewport scrolls excess top lines into scrollback rather than truncating the line the cursor sits on.
- **Strict xterm & ECMA-48 Alignment**:
  - Soft-wrap flags belong to the row (`ICH`, `DCH`, `ECH`, `SL`, `SR`, `EL 1`, `DECERA`, `DECFRA`, `DECSERA` preserve; `EL 0`, `EL 2`, `ED`, `DECSED` clear);
  - `DECLRMM` left/right margins and top/bottom scroll regions follow xterm cursor clamping rules;
  - `DECFRA` / `DECERA` / `DECSERA` cleanly split wide characters that straddle a rectangle boundary;
  - Synchronized output (`CSI ? 2026 h/l`) is now global state across main and alternate screens and holds during viewport scroll;
  - `LF` / `VT` / `FF` preserve the cursor column by default and support Line Feed / New Line Mode (`LNM`, `CSI 20 h/l`).

---

## 4. Per-Cell Glyph Placement, Clipping & Zero Startup Flash

- **Per-Cell Glyph Placement & Clipping**: `unterm-render` places, inverts, and clips glyphs strictly by the cell(s) their cluster belongs to, and rounds HarfBuzz 26.6 shaping offsets to the nearest pixel. Oversized icons no longer bleed into neighbouring columns.
- **Scrollback PNG Wide-Character Stride**: Fixed the scrollback PNG renderer so width-0 continuation cells are skipped and wide CJK characters advance 2 columns instead of 3.
- **Windows Zero-Flash Startup**: Enforces synchronous `LWA_ALPHA=0` layered cloaking and DWM cloaking before `ShowWindow` until the first GPU frame is presented.

---

## Upgrade Now

Run `unterm-cli update`, upgrade via Homebrew (`brew upgrade --cask unterm`) or Scoop (`scoop update unterm`), or download the latest release from [GitHub Releases](https://github.com/zhitongblog/unterm/releases/tag/v0.71.22).
