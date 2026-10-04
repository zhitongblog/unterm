---
layout: ../../layouts/Doc.astro
title: Architecture overview
subtitle: How native next-core turns input into terminal state, GPU frames, and one shared automation surface.
kicker: Docs / Architecture
date: 2026-07-30
---

## One native core, five product surfaces

Unterm now ships its own Rust terminal kernel, `next-core`. The deleted WezTerm GUI and mux are no longer the product runtime. Five surfaces share the same live engine and product services:

- the native GPU terminal GUI;
- Agent Cockpit and Review;
- the localhost Web Settings and Review application;
- `unterm-cli`;
- the authenticated MCP control plane.

Each native window is one OS process. Startup in `unterm-app/src/main.rs` creates the MCP listener first, passes its token to the Web server, and then starts the GUI:

```text
                         unterm process
                    ┌─────────────────────┐
human input ───────►│ unterm-app          │
                    │ native window + wgpu│
                    └──────────┬──────────┘
                               │ HostEngine
                    ┌──────────▼──────────┐
                    │ unterm-engine       │
                    │ next-core runtime   │
                    │ tabs, panes, VT, PTY│
                    └──────────┬──────────┘
                               │
                               └────────────► child shell / TUI

agent / CLI ── JSON-RPC ──► unterm-mcp ─────► same HostEngine
browser ───── localhost ──► unterm-settings ► same services and handler
```

There is no second terminal model for automation. Human input, Composer input, CLI input, and MCP input all reach the same pane runtime. Screen reads and captures are taken from that runtime rather than reconstructed from logs.

## Process and discovery model

The process hosts:

```text
unterm
├── native event and render loop
├── next-core runtime pump
├── MCP listener on 127.0.0.1:19876 (with port fallback)
├── Web Settings listener on 127.0.0.1:19877 (with port fallback)
├── update checker
└── PTY I/O workers owned by next-core sessions
```

Both listeners are loopback-only. The MCP connection must call `auth.login` before any of the 152 authenticated methods. The Web application can bootstrap locally, but all `/api/*` routes require the same bearer token.

Runtime discovery is stored under `~/.unterm/`:

```text
~/.unterm/
├── instances/
│   ├── alpha.json
│   └── bravo.json
├── server.json
├── active.json
├── theme.json
├── lang.json
├── proxy.json
└── update_check.json
```

`unterm-services/src/server_info.rs` atomically claims NATO-style instance names and owns registration, liveness cleanup, active-instance routing, profile metadata, and window metadata. Authentication-bearing files are written with owner-only permissions on Unix. `unterm-cli --instance <name>` and peer-window operations resolve through this registry.

## The current crate stack

| Crate | Responsibility |
|---|---|
| `unterm-engine` | Native next-core kernel: PTYs, VT parsing, cell/history state, tabs, splits, selection, recording taps, scheduling, health, and the `HostEngine` facade. |
| `unterm-render` | WebGPU render backend and engine render-plan consumption. |
| `unterm-app` | Native window, input/IME, chrome, tabs, panes, overlays, Composer, Agent Cockpit, Fleet, Review entry points, and clipboard integration. |
| `unterm-services` | Shared product state: instance discovery, settings, i18n, proxy detection, launch environment, recording archive/redaction, capture, and cross-window messaging. |
| `unterm-agents` | Agent hooks and the authoritative MCP metadata inventory used by `meta.surface`. |
| `unterm-mcp` | Authenticated JSON-RPC listener and dispatch for the 152 public methods. |
| `unterm-settings` | Loopback Web Settings/Review server and bundled SPA. |
| `unterm-cli` | Human- and agent-friendly command-line client over the MCP protocol. |
| `unterm-profile` / `unterm-proxy` | Persistent launch profiles and proxy identities applied at spawn time. |

Some lower-level portability crates retained from the previous codebase remain where they are useful, but they are dependencies, not the terminal kernel or GUI architecture.

## Keystroke-to-pixel path

For a normal keypress:

```text
1. The OS delivers a native keyboard or IME event to unterm-app.
2. The key map handles product shortcuts; unbound input is encoded for the pane.
3. HostEngine queues the input on next-core's interactive runtime lane.
4. The pane PTY receives the bytes.
5. The child shell or TUI emits terminal output.
6. The incremental UTF-8 and VT parser mutates next-core screen/history state.
7. A render revision and dirty-row plan are published.
8. unterm-render shapes/rasterizes required glyphs and submits WebGPU commands.
9. The native window presents the frame.
```

Paste is chunked without breaking UTF-8 or bracketed-paste markers. Interactive input, output application, screen reads, and background work use explicit scheduler lanes so output flood does not starve focus, paste, or control operations. Runtime-pump and I/O telemetry are exposed through health and self-test methods.

## Terminal state ownership

Next-core owns:

- PTY session creation, exit state, CWD/activity metadata, and process diagnostics;
- tabs, split trees, focus, pane sizes, zoom, and per-pane viewport;
- visible cells, scrollback, soft wraps, styles, hyperlinks, cursor modes, and selection;
- application cursor/keypad modes, mouse reporting, OSC 7/8/52/133, and terminal queries;
- prompt blocks, unseen output, recording taps, render revisions, and health counters.

The native window remains responsible for OS-only behavior such as window focus, file dialogs, desktop clipboard access, screenshots, and UAC launch. Those capabilities cross an explicit host bridge; they are not hidden terminal-kernel dependencies.

## The MCP control plane

`unterm-mcp/src/server.rs` implements the loopback TCP server and authentication handshake. `unterm-mcp/src/handler.rs` dispatches product operations through `HostEngine` and shared services:

```text
agent → 127.0.0.1:<mcp_port>
            │
            ▼
auth.login(token)
            │
            ▼
McpHandler::handle(method, params)
            │
            ├──► HostEngine (terminal state)
            ├──► host-window bridge (native-only actions)
            └──► unterm-services (persistent product state)
```

The method inventory is declared in `unterm-agents/src/mcp_meta.rs`. `meta.surface`, `server.capabilities`, the CLI reference, and dispatch-coverage tests derive from or validate that inventory. Every public method is classified as either read-only or mutating; mutating calls pass policy checks where applicable and receive a redacted audit entry at the dispatch boundary.

## Web Settings and Review

`unterm-settings/src/server.rs` binds the local HTTP server and serves assets embedded from `unterm-settings/assets/settings/`. The browser UI covers theme, language, proxy, scrollback, compatibility, profiles/agents, recording, sessions, Review, and update state.

The Web layer uses the same `McpHandler` and the same `~/.unterm/` settings files as the CLI and native GUI. It does not maintain a parallel configuration database. Theme changes are persisted and published through `unterm-services/src/theme_state.rs`; every open native window observes the generation-stamped request and repaints without restart.

## Spawn-time profiles and proxy identity

Pane creation resolves launch context immediately before spawning:

```text
requested command / cwd
          │
          ├──► selected profile
          ├──► future-launch environment overlay
          └──► current proxy identity / rotation decision
                         │
                         ▼
                    next-core PTY
```

This makes Web, CLI, MCP, Fleet, split, restore, and native GUI launches consistent. Secrets can affect the child environment but are redacted from diagnostics, profiles, recordings, audit logs, and returned launch metadata.

## Where to contribute

- Add or change terminal semantics in `unterm-engine/src/next_core/`, with focused parser/state tests.
- Add an MCP operation to `unterm-agents/src/mcp_meta.rs` and `unterm-mcp/src/handler.rs`; update its read/write audit classification and CLI/Web presentation as needed.
- Add native UI behavior in `unterm-app/src/`.
- Add persistent cross-surface behavior in `unterm-services/src/`.
- Add Web routes in `unterm-settings/src/server.rs` and UI in `unterm-settings/assets/settings/`.
- Add CLI commands in `unterm-cli/src/` as thin clients over the public MCP method.
- Add translations in `unterm-services/src/i18n/locales/` and Web dictionaries in `web/src/i18n/`.

The design rule is simple: terminal truth belongs to next-core, OS-window behavior belongs to the host bridge, and shared product configuration belongs to services. GUI, Web, CLI, and MCP should present those same operations rather than reimplementing them.
