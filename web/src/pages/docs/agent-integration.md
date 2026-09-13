---
layout: ../../layouts/Doc.astro
title: Driving Unterm from outside
subtitle: How to wire up Claude Code, Cursor, Aider, or your own scripts to control an Unterm window over MCP — the pattern Unterm was designed around.
kicker: Docs / Agent integration
date: 2026-05-03
---

## The mental model

Most terminals embed their AI _inside_ the terminal — Warp's AI lives in a closed cloud orchestrator, ChatGPT desktop opens its own panel, etc. Unterm picks the opposite end: keep AI completely outside the binary, expose the terminal itself as a surface that _any_ external agent can grip via MCP. The terminal is the hand. Whatever's holding it is up to you.

Concretely, every Unterm window opens two local servers on launch:

- **MCP server** on `127.0.0.1:<auto-port>` — line-delimited JSON-RPC over TCP, auth-token gated, exposes every product operation (create pane, send input, read screen, capture screenshot, record, manage proxy, query instances).
- **HTTP settings server** on `127.0.0.1:<auto-port>` — REST endpoints for human-driven config (theme, font, profile) and a Tailwind+Alpine SPA at the root. Theme switching, font tweaks, and profile edits are HTTP-only — they're not on MCP because they're settings the user owns, not actions an agent should script.

Both GUI ports plus the GUI auth token are written to `~/.unterm/server.json` on launch for older scripts. The headless Core writes `core.json` — in the Core's platform data directory (`%LOCALAPPDATA%\Unterm`, `~/.local/share/Unterm`, `~/Library/Application Support/Unterm`), not in `~/.unterm` — with the MCP endpoint that owns terminal sessions across GUI restarts. Modern MCP clients should be registered through `unterm-cli mcp-stdio`: the bridge prefers Core discovery, falls back through the live GUI instance registry, authenticates to the right local TCP server, and keeps the static client config working across restarts and multiple windows.

For the full schema of every MCP method see the [MCP reference](/docs/mcp-reference). For the layout of `~/.unterm/` see the [configuration guide](/docs/configuration). For driving more than one window at a time see the [multi-instance guide](/docs/multi-instance). For shell scripting, see the [CLI reference](/docs/cli-reference).

## Quick start: connecting Claude Code

Claude Code is the canonical example because it ships native MCP support. The supported setup path is:

1. Install Unterm and launch it once.
2. Run the idempotent installer:

```sh
unterm-cli setup-ai
```

3. Restart Claude Code. Type `/mcp` and you should see `unterm` listed as a connected server with its tool list.

`setup-ai` writes a managed `unterm` stdio MCP server into supported local clients, including Claude Code, Codex CLI, Gemini CLI, Cursor, Windsurf and OpenCode when their config directories exist. For Claude Code that means a `mcpServers.unterm` entry in `~/.claude.json`:

```json
{
  "mcpServers": {
    "unterm": {
      "type": "stdio",
      "command": "C:\\Program Files\\Unterm\\unterm-cli.exe",
      "args": ["mcp-stdio"]
    }
  }
}
```

Use the actual `unterm-cli` path on your machine. The bridge self-discovers the active Unterm control server at connection time, preferring the Core endpoint and falling back to GUI instance records, so you do not hard-code the port or token into Claude's config. Cursor, Gemini, Windsurf and OpenCode use the same bridge with their own config-file shapes; Codex uses `[mcp_servers.unterm]` in `~/.codex/config.toml`.

If you want to drive multiple Unterm windows from one agent, see the [multi-instance guide](/docs/multi-instance) — each window gets its own port and token, and there's a small registry under `~/.unterm/instances/` to enumerate them.

## Headless task runner

For one-shot work, you do not need to open an interactive agent pane first. `unterm-cli agent run` starts a supported coding CLI in its non-interactive mode, waits for it to finish, and still applies the same Unterm profile, cwd, auth mode, launch flags, and MCP bridge wiring as `agent launch`.

```sh
unterm-cli agent run codex-cli --cwd ~/src/app "review this diff and list risky changes"
unterm-cli agent run claude-code --profile work "summarise the last failing test output"
unterm-cli agent run gemini-cli --cwd ~/src/app "summarise this repository and suggest the next useful task"
unterm-cli agent run opencode --profile work "inspect the current project and suggest the next useful task"
```

This is the lowest-friction way to let users spend through the official subscriptions they already have. If Codex, Claude Code, or Gemini CLI is signed in through its vendor account flow, Unterm reuses that local CLI state; it does not convert the run into an API-key workflow or inject a per-token credential. API keys remain an explicit fallback inside Web Settings -> AI Agents.

`--stdin` lets an outer workflow feed context without building an MCP client:

```sh
git diff | unterm-cli agent run codex-cli --stdin "review this diff"
```

Use `--dry-run` to inspect the exact vendor command and redacted environment before running it:

```sh
unterm-cli agent run gemini-cli --dry-run "summarise this repository"
```

## Pattern 1 — Director and worker

The killer use case for an MCP-controllable terminal: an _outer_ agent supervises an _inner_ agent running inside an Unterm pane. The outer agent dispatches concrete work, the inner agent does the actual coding, the outer one watches progress and steps in when it stalls.

Concretely, in your outer Claude Code session:

```js
// 1. open a fresh pane in the project dir and launch the inner agent.
//    orchestrate.launch = session.create + a command line typed into the pane.
const pane = await mcp.unterm.orchestrate.launch({
  cwd: "/Volumes/Dev/code/some-project",
  command: "claude code",
})

// 2. wait for the inner agent's prompt marker to appear (or time out).
await mcp.unterm.orchestrate.wait({
  id: pane.id,
  pattern: "▶▶ bypass permissions on",
  timeout_ms: 30_000,
})

// 3. dispatch the task by typing into the pane.
await mcp.unterm.session.input({
  id: pane.id,
  input: "implement the feature described in TODO.md, run the tests, commit when green\n",
})

// 4. poll output every 30s, intervene if stuck.
while (true) {
  const hit = await mcp.unterm.screen.search({ id: pane.id, pattern: "all green" })
  if (hit.total > 0) break

  const tail = await mcp.unterm.screen.scroll({ id: pane.id, offset: -200, count: 200 })
  if (looksStuck(tail.lines)) {
    await mcp.unterm.session.input({
      id: pane.id,
      input: "you appear stuck — try X\n",
    })
  }
  await sleep(30_000)
}
```

The outer agent has the strategic picture (which task next, when to escalate, when to switch from coding to commit prep), the inner agent has the keyboard. Both can be Claude — they're just running with different system prompts and different scopes.

A few notes on the methods used:

- `orchestrate.launch` is `session.create` plus a typed `command` line. If you don't need to run anything (just want a blank shell pane), use `session.create` directly — it accepts `cwd`, `cols`, `rows`.
- `session.input` types characters as if the user typed them, including control codes. Append `\n` to actually submit.
- `orchestrate.wait` blocks server-side until a regex-free substring match hits the visible screen, or `timeout_ms` elapses. Returns `{ matched: bool, timed_out?: bool, pattern }`. Cheaper than polling from the client.
- `screen.search` scans visible-plus-scrollback for a substring and returns `{ matches: [{row, col, text}], total }`. Use this for "did the run finish yet" checks where you don't want to block. Pass `goto: true` (or `goto_match: N`) to also scroll the user-visible viewport to the match when the active engine has a GUI viewport; headless and next-core engines return matches with `goto_skipped` instead.

## Pattern 2 — Long-running watcher

Run a long task in a pane, have an external agent notice when it finishes and act:

```js
const pane = await mcp.unterm.orchestrate.launch({
  cwd,
  command: "make ci-full",
})

// session.idle returns { idle: bool, foreground_process: string }. The
// heuristic is "is the foreground process the user's shell again" — i.e.
// whatever was running has exited and we're back at a prompt.
while (true) {
  const status = await mcp.unterm.session.idle({ id: pane.id })
  if (status.idle) break
  await sleep(60_000)
}

// pull the visible screen + scrollback for analysis.
const { lines } = await mcp.unterm.screen.scroll({
  id: pane.id,
  offset: 0,
  count: 5000,
})
const transcript = lines.join("\n")

if (/PASS \d+ tests/.test(transcript)) {
  notify("CI green, ready to merge")
} else {
  notify(`CI red, last 50 lines:\n${lines.slice(-50).join("\n")}`)
}
```

Same pattern works for `cargo build`, `npm run test`, long ETL jobs, or any "kick off something, wait, decide" loop. The agent doesn't have to be a chat session — a cron job calling `unterm-cli` works the same way.

`session.idle` is a heuristic — it returns `idle: true` when the foreground process name matches a known shell (`bash`, `zsh`, `fish`, `pwsh`, `nu`, etc.). It's not a guarantee that the previous command "really finished," just that control returned to the shell. For correctness-sensitive flows, also check the screen for an explicit completion marker (`exit code: 0`, `PASS \d+ tests`, your project's own marker).

## Pattern 3 — Multi-pane orchestration

When an outer agent fans out work across several projects, each pane is one project's "workspace" and the outer agent aggregates across them:

```js
const repos = ["solomd", "unflick", "unterm"]

const panes = await Promise.all(
  repos.map((r) =>
    mcp.unterm.session.create({ cwd: `/Volumes/Dev/code/${r}` })
  )
)

// fan-out: send the same command to every pane in one round trip.
await mcp.unterm.orchestrate.broadcast({
  command: "make lint",
  sessions: panes.map((p) => String(p.id)),
})

// fan-in: wait for each pane to return to its shell, then read tail.
await Promise.all(
  panes.map(async (p) => {
    while (!(await mcp.unterm.session.idle({ id: p.id })).idle) {
      await sleep(5_000)
    }
  })
)

const reports = await Promise.all(
  panes.map((p) =>
    mcp.unterm.screen.scroll({ id: p.id, offset: -100, count: 100 })
  )
)
```

`orchestrate.broadcast` takes `{ command, sessions }` (sessions is an array of pane id strings) and types `command + \r` into each. It returns per-session success/error so you know which pane the input actually reached.

If you want to broadcast to panes across _different Unterm windows_ — e.g. one window per project root — see the [multi-instance guide](/docs/multi-instance) for how to discover peer instances and dispatch to each one's MCP port.

## Pattern 4 — Recording for review

Every pane can be recorded into a markdown transcript with built-in token redaction. An outer agent triggers a recording, drives a session, then ships the markdown for human review or AI fine-tune:

```js
const start = await mcp.unterm.session.recording_start({ id: pane.id })
// start.session_id  — the recording id, distinct from pane id
// start.log_path    — raw NDJSON log being appended to
// start.md_path_when_done — the markdown path that will exist on stop

await mcp.unterm.session.input({
  id: pane.id,
  input: "<commands the agent runs>\n",
})
// ... agent works ...

const result = await mcp.unterm.session.recording_stop({ id: pane.id })
// result.session_id  — recording id (matches start.session_id)
// result.ended_at    — ISO 8601 stop timestamp
// result.block_count — number of OSC 133 prompt blocks captured
// result.exit_reason — "user_stop" | "pane_dead" | etc.
// result.md_path     — the rendered markdown transcript on disk
```

Recordings live under `<cwd>/.unterm/sessions/<date>/` when there's a writable project directory; otherwise they fall back to `~/.unterm/sessions/_orphan/`. Tokens, GitHub PATs, and 40+ char base64/hex strings are masked before write.

`session.recording_status` returns the live state (running/stopped, byte count) for a pane. `session.recording_list` enumerates past recordings — pass `{ project: "<path>" }` to filter by project root. `session.recording_read` returns the rendered markdown body so you can ship it to a follow-up LLM call without round-tripping through the filesystem. There's also `session.recording_attach_trace` for stitching an external trace ID into the markdown frontmatter, useful when an outer agent wants to correlate one pane's recording with its own run log.

## What's _not_ on MCP

A few things that look like they should be MCP methods aren't, on purpose:

- **Theme, font, profile, keybinding edits.** These live on the HTTP settings server (`/api/theme`, `/api/font`, etc.) and the SPA at `127.0.0.1:<http_port>/`. The `unterm-cli theme list` / `unterm-cli theme switch <name>` commands hit those HTTP endpoints, not the MCP server. The split is deliberate: MCP is the action surface for agents, HTTP is the configuration surface for the user. An agent should not be retheming your terminal mid-session.
- **Workspace save/restore is on MCP, scoped to pane layout snapshots.** `workspace.save`, `workspace.restore`, and `workspace.list` exist. `workspace.list` returns the saved workspace path, timestamp, and pane count, but workspaces are still about reopening working directories and titles rather than full settings/profile state.
- **Cross-instance window focus.** An instance's MCP server only ever acts on _its own_ windows. `instance.focus` raises one of them — by `window_id` since v0.68, or whichever is in front when you omit it. To raise a peer's, you connect to that peer's MCP port (discoverable via the [multi-instance registry](/docs/multi-instance)) and call `instance.focus` there. The OS-level raise itself is implemented as of v0.68; on Windows the platform may answer with a taskbar flash rather than the foreground when Unterm is not the app in front.

## MCP method reference (most-used)

Every method below appears in the dispatch table at `unterm-mcp/src/handler.rs`. All are under the `unterm` namespace.

| Method | Purpose |
|---|---|
| `session.list` | Enumerate panes — id, title, cols/rows, cursor, shell |
| `session.create` | Open a new pane with given `cwd` / `cols` / `rows` |
| `session.input` | Type characters into a pane (as if user typed) |
| `session.idle` | Heuristic "is the pane back at its shell" check |
| `session.history` | Last N scrollback lines as `{entries, count}` |
| `session.recording_start` / `_stop` | Record to redacted markdown transcript |
| `screen.text` | Visible viewport as lines + cursor position |
| `screen.scroll` | Read N lines starting at scrollback `offset` |
| `screen.search` | Substring search across visible + scrollback |
| `orchestrate.launch` | `session.create` + type a command line |
| `orchestrate.wait` | Block until `pattern` appears on screen, or timeout |
| `orchestrate.broadcast` | Send the same command to many panes at once |
| `capture.screen` / `capture.window` | PNG capture of all panes / a specific window |
| `proxy.status` / `proxy.switch` | Read or change the active proxy node |
| `instance.list` / `instance.focus` | Enumerate peer instances / raise one of this instance's windows |
| `instance.windows` / `instance.new_window` | List this front end's windows / open another and get its id |
| `server.info` / `server.capabilities` | Server identity, ports, version, method list |

The full enumeration (every method, every parameter, every return shape) is in the [MCP reference](/docs/mcp-reference). The dispatch table in `handler.rs` is the source of truth — if it's not in the dispatch, it's not a real method.

## Security model

- Both servers bind to `127.0.0.1` only. Nothing on the LAN can reach them; no port forwarding by default.
- Every request must carry the auth token from `server.json` — generated fresh on each launch, written with 0600 permissions. Connections without it get 401 immediately.
- No telemetry, no analytics, no login. The auth token never leaves your machine. Recordings stay on disk under your project dir.
- The `session.create`, `session.input`, and `orchestrate.launch` methods can run arbitrary shell commands — treat the auth token like an SSH key, not like an API key. If you're checking `~/.unterm/` into version control by accident, you'll be sad.

## CLI parity

Everything an MCP client can do, `unterm-cli` can do too — it's a thin JSON-RPC client over the same MCP server, no duplicated business logic. Useful for cron jobs, shell scripts, CI steps that don't carry an MCP client:

```sh
unterm-cli session list --json | jq '.[] | select(.cwd | endswith("unflick"))'
unterm-cli session record start --pane-id 0
unterm-cli proxy status
unterm-cli screenshot --include-window --output /tmp/cap.png
```

Pipe `--json` through anywhere downstream that wants raw JSON-RPC. Theme and font management are also on the CLI (`unterm-cli theme list`, `unterm-cli theme switch <name>`) — those subcommands hit the HTTP settings server rather than the MCP server, but from the user's point of view it's all one binary.

For the full subcommand list see the [CLI reference](/docs/cli-reference).

---

Source for everything described here lives at [github.com/zhitongblog/unterm](https://github.com/zhitongblog/unterm) under MIT. File issues / PRs there.
