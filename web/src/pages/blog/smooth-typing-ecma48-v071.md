---
layout: ../../layouts/Doc.astro
title: "Unterm v0.71.21: Zero Dropped Keystrokes, Flawless TUI Rendering"
subtitle: "Recovering orphaned Win32 WM_CHAR messages, ECMA-48 cursor column compliance, and lock-free GUI frame caching."
kicker: Blog / Release
date: 2026-10-20
---

> 中文版: [Unterm v0.71.21: 彻底告别掉字、TUI 撕裂与界面卡顿](/blog/smooth-typing-ecma48-v071-zh/)

Over the past few weeks, power users and AI agent developers surfaced three real-world rough edges:
1. **Occasional dropped keystrokes** when typing at high speeds or committing CJK IME candidate phrases on Windows;
2. **Horizontal tearing, ghost prompt duplication, or errant cursor jumps** during multi-line rendering in modern TUI applications (Claude Code, Google Antigravity `agy`, Bubble Tea, reedline) when resizing windows or streaming output;
3. **Subtle micro-stalls on the GUI thread** during rapid window focusing or pane switching under heavy output.

None of these showed up in basic synthetic benchmarks, but in daily production workflows with AI coding agents, they were noticeable pain points.

**Unterm v0.71.21** eliminates these issues with deep architectural fixes in the Windows message queue layer, strict ECMA-48 protocol compliance, and a lock-free IPC caching engine.

---

## 1. The Ghost Keystroke: Win32 Message Queues and Orphaned WM_CHAR

Cross-platform Rust GUI frameworks (such as `winit`) model keyboard input cleanly as KeyDown, KeyUp, and text input events. However, the Win32 message loop has subtle timing characteristics:

When a key is struck, Windows posts `WM_KEYDOWN`, and `TranslateMessage` subsequently posts `WM_CHAR`. But under two very common scenarios:
- **Fast typing (100+ WPM)**: `WM_KEYUP` or the next key's `WM_KEYDOWN` can interleave ahead of the pending `WM_CHAR`;
- **IME candidate commits**: Chinese/Japanese IME composition commit sends bursts of synthesized characters into the message queue.

If an event loop observes an `Unidentified` key without an immediate character payload and drops it, the trailing `WM_CHAR` is orphaned and lost, resulting in dropped letters or punctuation.

### The Fix: Double-Guard Orphan Recovery

In v0.71.21:
1. **Message Queue Lookahead**: On receiving an unidentified key event, `recover_unidentified_windows_key` inspects the Win32 message queue for pending `WM_CHAR` entries;
2. **Physical Scancode Fallback**: If `WM_CHAR` is not yet present, it translates physical scan codes against the active keyboard layout into an ASCII fallback;
3. **Optimized IME Lock Ordering**: Removes lock contention between candidate commits and the main render loop.

**Result**: 0 dropped characters in 150+ WPM typing tests and automated million-keystroke synthetic stress runs.

---

## 2. Screen Tearing in TUIs: Strict ECMA-48 Compliance

Developers increasingly drive AI coding agents (Claude Code, `agy`, Aider) directly in their terminal. These tools rely heavily on modern TUI libraries (Go's Bubble Tea, Rust's crossterm/reedline, Python's prompt_toolkit) for rich multi-line streaming and live progress bars.

When resizing the terminal or streaming verbose multi-line code diffs, users previously reported occasional visual tearing or misaligned prompts.

Our VT100 / ECMA-48 audit pinpointed the root causes:

### (1) Cursor Column Reset on IL (Insert Line) and DL (Delete Line)
When TUIs scroll a partial block of lines within the viewport, they issue `CSI <n> L` (Insert Line) or `CSI <n> M` (Delete Line).
Many terminal emulators erroneously reset the cursor column to 0 as part of this operation. However, **ECMA-48 §8.3.67 and §8.3.32 explicitly mandate**:
> *Insert Line and Delete Line must leave the cursor column position unaltered.*

Resetting the column caused subsequent partial redrawing (e.g. status tags or inline autocomplete) to shift leftward, causing horizontal tearing across the screen.

### (2) Pending-Wrap Boundaries and Clamped DSR Reports
When typing to the final column of a terminal row, the terminal enters a "pending-wrap" state. An off-by-one boundary check during backspaces could inadvertently skip characters or delete across row boundaries.
Furthermore, Device Status Report (DSR 6n) cursor coordinate queries now strictly clamp returned positions to the active screen geometry, preventing TUI layout engines from panicking during fast window resizes.

In v0.71.21:
- Cursor column is strictly preserved across `IL` / `DL` operations;
- DSR cursor position responses are boundary-clamped;
- Backspaces in pending-wrap states preserve exact character geometry;
- Synchronized Output (Mode 2026) ensures atomic redraws without ghost flicker.

---

## 3. Zero GUI Stalls: Lock-Free FrameCache

In Unterm's architecture, the terminal core engine runs in an isolated daemon while the GUI provides 120 FPS hardware-accelerated rendering.

Previously, certain GUI operations (like window focus updates or session enumerations) made synchronous queries to the Core process. If the Core was busy streaming massive PTY throughput, the GUI main thread could experience a slight hitch.

In v0.71.21, we migrated session state synchronization to a **lock-free atomic `FrameCache`**:
- The Core daemon pushes state updates asynchronously;
- The GUI rendering pipeline reads exclusively from local, non-blocking cache;
- Zero blocking cross-process mutexes or IPC waits on the UI thread, ensuring a rock-solid 120 FPS frame rate under all workloads.

---

## 4. More Improvements

- **Win11 Native DWM Cloaking for Screen Capture**: AI agents capturing terminal snapshots (`screen.shot`) now use native DWM cloaking, eliminating window flicker or focus loss.
- **Cross-Platform Packages Ready**: Updated across Homebrew, Scoop, and GitHub Releases with native notarized DMG, Windows MSI/ZIP, and Linux AppImage/DEB.

---

## Getting Started

To update an existing installation:
```bash
unterm-cli update
```

To install freshly via package managers:
```bash
# macOS (Homebrew)
brew install --cask zhitongblog/tap/unterm

# Windows (Scoop)
scoop bucket add zhitongblog https://github.com/zhitongblog/scoop-bucket
scoop install unterm
```

Report any issues or join discussions on [GitHub](https://github.com/zhitongblog/unterm).
