---
layout: ../../layouts/Doc.astro
title: MCP Method Reference
subtitle: Every method exposed by the local Unterm MCP server, with parameter shapes, return shapes, error codes, and a real example.
kicker: Docs / MCP reference
date: 2026-07-20
---

This page explains the JSON-RPC surface exposed by a running Unterm instance.
The current native `next-core` build exposes 151 authenticated methods plus
`auth.login`. The authoritative inventory lives in
`unterm-agents/src/mcp_meta.rs`, dispatch is in
`unterm-mcp/src/handler.rs`, and the connection handshake is in
`unterm-mcp/src/server.rs`. For a machine-readable list that always matches
the running binary, call [`meta.surface`](#meta) or run
`unterm-cli reference` in any shell.

For higher-level patterns (director/worker, multi-pane orchestration, recording for review) see the [agent integration guide](/docs/agent-integration/). This page is the wire-level companion — the doc you check when your client got back `-32603` and you want to know which field you fat-fingered.

## Connection and auth

### Where the port and token live

Current builds write `core.json` from `unterm-core` into the Core's platform data directory (`%LOCALAPPDATA%\Unterm`, `~/.local/share/Unterm`, `~/Library/Application Support/Unterm`) — not into `~/.unterm`, where the GUI registry lives; it is the preferred MCP discovery record because it owns sessions across GUI restarts. `unterm-cli mcp-stdio` resolves `core.json` first, then falls back through live GUI instance records and legacy files. The GUI registry files below remain valid for tools that need a specific window or an older active-GUI compatibility pointer.

On launch, every Unterm process writes its identity to three files under `~/.unterm/`:

- `~/.unterm/instances/<nato-name>.json` — the canonical record for *this* instance. NATO-phonetic ids: `alpha`, `bravo`, `charlie`, … cycling to `alpha2` when all 26 are taken simultaneously. Contains `mcp_port`, `http_port`, `auth_token`, `pid`, `started_at`, `title`, `cwd`, `version`, `platform`.
- `~/.unterm/server.json` — single-instance compat alias. Mirrors the *active* instance's metadata. Older agents that only know about one Unterm at a time read this and keep working.
- `~/.unterm/active.json` — pointer to the currently active instance id. Updated only when the previous active dies, not on every focus change. Disk-IO budget.

A multi-instance-aware agent should enumerate `~/.unterm/instances/*.json`, drop entries whose `pid` is no longer live, and pick the instance it wants by `title`, `cwd`, `started_at`, or whatever heuristic it prefers. A single-instance-aware agent just reads `server.json` and ignores the rest.

The MCP server preferred port is `19876` (HTTP settings server is `19877`). On collision, Unterm walks forward up to `PORT_RETRY_LIMIT` (5) ports before giving up, so in practice you'll see ports in `19876..=19881`. Both bind to `127.0.0.1` only — nothing on the LAN can reach them.

### Framing

The protocol is line-delimited JSON-RPC 2.0 over TCP:

- Each request is one line of JSON, terminated by `\n`.
- Each response is one line of JSON, terminated by `\n`.
- TCP `nodelay` is set on the server side, so small frames flush immediately.
- Empty lines are skipped.
- Parse errors return `{ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": "Parse error: ..." } }` and the connection stays open.

There is no batch support, no notifications (every request gets a response), no `params` schema validation beyond what the handler does itself.

### The auth handshake

The very first method on a new TCP connection MUST be `auth.login`, with the token from `instances/<id>.json`:

```json
{"jsonrpc":"2.0","id":1,"method":"auth.login","params":{"token":"5f3c2a1e-..."}}
```

Success returns:

```json
{"jsonrpc":"2.0","id":1,"result":{"status":"ok"}}
```

Wrong token returns error code `-32001` (`"Invalid auth token"`) and the connection is *not* dropped — you can retry with the right token. Calling any other method before `auth.login` returns `-32002` (`"Not authenticated. Call auth.login first"`). Once authenticated, the connection stays authenticated for its lifetime.

### Error codes

| Code | When |
|---|---|
| `-32700` | Parse error — request line wasn't valid JSON |
| `-32001` | Invalid auth token (bad credentials on `auth.login`) |
| `-32002` | Not authenticated (any method before `auth.login`) |
| `-32603` | Internal error — handler returned `Err`. The `message` field is the underlying anyhow error, e.g. `"Session 7 not found"` or `"Missing 'command'"` |

There is no `-32601` (method not found); unknown methods come back as `-32603` with message `"Unknown method: <name>"`.

### A complete handshake-and-call

```
> {"jsonrpc":"2.0","id":1,"method":"auth.login","params":{"token":"5f3c..."}}
< {"jsonrpc":"2.0","id":1,"result":{"status":"ok"}}
> {"jsonrpc":"2.0","id":2,"method":"session.list","params":{}}
< {"jsonrpc":"2.0","id":2,"result":{"sessions":[{"id":0,"title":"zsh","cols":120,"rows":30,...}]}}
```

That's the entire protocol. Everything below is just which methods you can put in the `method` field and what each one does.

---

## Session

The session namespace is the primary surface — every pane in the terminal is a "session" with a numeric id. Most other namespaces (`exec`, `screen`, `capture`, recording) take a session id as their first parameter.

A note on parameter naming: pane id can be passed as either `id` (numeric) or `session_id` (string). Both work everywhere a pane is required. The CLI tends to use `id`; older clients use `session_id`. They're aliases.

### `session.list`

Enumerate every live pane. No params.

**Returns:** `{ sessions: [{ id, title, cols, rows, cursor: { x, y, visible }, is_dead, domain_id, shell: { shell_type, process_name, cwd } }] }`

`shell_type` is one of `"powershell"`, `"cmd"`, `"bash"`, `"zsh"`, `"fish"`, `"nushell"`, `"unknown"` — derived by parsing the foreground process name.

```json
{"jsonrpc":"2.0","id":3,"method":"session.list","params":{}}
```

```json
{"jsonrpc":"2.0","id":3,"result":{"sessions":[
  {"id":0,"title":"alex@laptop ~/code/unterm","cols":120,"rows":30,
   "cursor":{"x":2,"y":29,"visible":true},
   "is_dead":false,"domain_id":0,
   "shell":{"shell_type":"zsh","process_name":"/bin/zsh","cwd":"file:///Volumes/Dev/code/unterm"}}
]}}
```

### `session.get` / `session.status`

Same method, two names. Get full state for one pane, including scrollback row count.

**Params:** `id` (number) or `session_id` (string), required.

**Returns:** `{ id, title, cols, rows, scrollback_rows, cursor: { x, y, visible }, is_dead, domain_id, shell }`

### `session.create`

Spawn a new tab/pane.

**Params:**

| Name | Type | Required | Description |
|---|---|---|---|
| `cols` | number | no | Terminal width, default `120` |
| `rows` | number | no | Terminal height, default `30` |
| `cwd` | string | no | Initial working directory; defaults to user home |
| `command` | string | no | Shell command string; runs through the platform shell |
| `argv` | array | no | Program argv array; starts `argv[0]` directly, no shell wrapping |
| `profile` | string | no | Identity profile whose resolved launch environment is applied |

**Returns:** pane identity/dimensions plus a redacted `launch` decision showing
the selected profile, proxy/env keys, command source, and launch policy.

```json
{"jsonrpc":"2.0","id":4,"method":"session.create",
 "params":{"cwd":"/Volumes/Dev/code/unterm","cols":160,"rows":48}}
```

```json
{"jsonrpc":"2.0","id":4,"result":{"id":7,"session_id":"7","title":"zsh","cols":160,"rows":48}}
```

### `session.split`

Split an existing pane in `left`, `right`, `up`, or `down` direction.

**Params:** `id`/`session_id` (target pane) and `direction`.

**Returns:** the new pane identity and split relationship.

### `session.focus`

Focus a pane, bring its tab to the front, and raise the window holding it.

**Params:** `id`/`session_id`.

**Returns:** `{ ok: true, id, window_id }` — `window_id` is the window that was raised, or `null` when the session was already in the window in front (or there is no window at all).

The raise matters once an instance has several windows: a front end follows the active session only in the window that is in front, so without it "look at this" about a session in the other window changed a flag and the user saw nothing at all.

### `session.input` / `exec.send`

Aliases. Write arbitrary bytes into the pane's stdin, exactly as if the user had typed them. Does *not* append a newline.

**Params:**

| Name | Type | Required | Description |
|---|---|---|---|
| `id` / `session_id` | number/string | yes | Target pane |
| `input` | string | yes | Raw characters to write |

**Returns:** `{ status: "ok" }`

If you want to submit a command, you almost always want `\r` (carriage return) at the end. Most shells treat `\n` as a literal line continuation; `\r` is what a real keypress sends.

### `session.paste`

Paste text using the terminal's paste path. Like all PTY writes, it is audited
and subject to the configured per-agent confirmation policy.

**Params:** `id`/`session_id`, plus `text`.

**Returns:** `{ status: "ok" }`.

### `session.resize`

Resize the pane's pty. Does what SIGWINCH would do — the running program receives the resize and reflows.

**Params:** `id`/`session_id` (yes), `cols` (yes), `rows` (yes).

**Returns:** `{ status: "ok" }`

### `session.destroy`

Kill the pane. Sends a kill to the underlying process and audits the action.

**Params:** `id`/`session_id`.

**Returns:** `{ status: "ok", destroyed: true }`

### `session.idle`

Heuristic check: is the foreground process the shell itself (idle) or a child (running)?

**Params:** `id`/`session_id`.

**Returns:** `{ idle: bool, foreground_process: string }`

`idle` is `true` when the foreground process name contains one of `powershell`, `pwsh`, `cmd`, `bash`, `zsh`, `fish`, `nu`. Anything else returns `false`. This is the call to use when polling "did my long-running build finish?"

### `session.cwd`

Get the pane's current working directory (from OSC 7 if the shell sets it, falls back to inspection).

**Params:** `id`/`session_id`.

**Returns:** `{ cwd: string }` — a `file://` URI string. May be empty if the shell doesn't emit OSC 7 and inspection failed.

### `session.env` / `session.set_env`

Read launch environment metadata for a pane, or set a future-launch overlay.

`session.env` is engine-specific:

- `next-core`: returns launch env variable names with values redacted: `{ supported: true, mutable: false, scope: "launch", variables: [{ name, value: null, redacted: true, scope: "launch" }] }`.
- compatibility engines may return `{ supported: false, value: null, message }`
  when launch metadata is unavailable.

`session.set_env` sets `{name, value}` in the process's future-launch overlay;
omit `value` or pass `null` to clear it. Existing shells are never mutated and
values are never returned through MCP.

### `session.history`

Return the last N lines of scrollback as a "history" list, with empty lines filtered out. This is *not* shell history (`~/.zsh_history`); it's pane scrollback.

**Params:**

| Name | Type | Required | Description |
|---|---|---|---|
| `id`/`session_id` | number/string | yes | Target pane |
| `limit` | number | no | Number of trailing rows to read, default `100` |

**Returns:** `{ entries: [{ text: string }, ...], count: number }`

### `session.audit_log`

Read the in-memory audit log. Every mutating method (`session.destroy`, `exec.run`, `signal.send`, `policy.set`, recording start/stop) appends an entry; reads do not. The log is process-local — restarting Unterm clears it.

**Params:**

| Name | Type | Required | Description |
|---|---|---|---|
| `limit` | number | no | Max entries to return, newest first, default `50` |
| `session_id` | string | no | Filter to a single pane id |

**Returns:** array of `{ timestamp (RFC3339), method, session_id, detail, allowed }`.

### Recording: `session.recording_start`, `_stop`, `_status`, `_list`, `_read`, `_attach_trace`, `session.export_markdown`

These wrap the `crate::recording` module. They write an OSC-133-aware redacted markdown transcript to disk (under `<cwd>/.unterm/sessions/<date>/` if there's a writable project directory, else `~/.unterm/sessions/_orphan/`). Tokens, GitHub PATs, and 40+ char base64/hex strings are masked before the file is written.

**`session.recording_start`** — begin recording one pane.

- Params: `id`/`session_id`.
- Returns: `{ session_id, log_path, md_path_when_done }` — `log_path` is the raw byte log being written live; `md_path_when_done` is where the final markdown will land when you call `_stop`.

**`session.recording_stop`** — finish, render the markdown, return paths and counts.

- Params: `id`/`session_id`.
- Returns: `{ session_id, ended_at, block_count, exit_reason, md_path }`. `block_count` is how many OSC 133 prompt boundaries were captured.

**`session.recording_status`** — non-mutating: is this pane currently being recorded?

- Params: `id`/`session_id`.
- Returns: whatever `crate::recording::recording_status` produces — typically `{ recording: bool, session_id, started_at, block_count }` (or just `{ recording: false }`).

**`session.recording_list`** — enumerate completed recordings on disk.

- Params: optional `project` (string) to filter by project path.
- Returns: array of `{ unterm_session_id, tab_id, project_path, project_slug, started_at, ended_at, block_count, bytes_raw, log_path, md_path }`.

**`session.recording_read`** — slurp one recording's rendered markdown back into memory.

- Params: `session_id` (string, required) — the recording's `unterm_session_id`, *not* a pane id.
- Returns: `{ markdown: string }`.

**`session.recording_attach_trace`** — associate an external trace id (e.g. an outer agent's correlation id) with a live recording. Useful when you want to correlate the markdown back to the agent's own logs after the fact.

- Params: `id`/`session_id` (pane), `trace_id` (string, required).
- Returns: `{ trace_ids: [...] }` — full list of trace ids attached so far.

**`session.export_markdown`** — render markdown for a pane. If recording is active, it exports from the live recording stream; otherwise it renders a one-off snapshot of the pane's current scrollback.

- Params: `id`/`session_id`, optional `path` (string) — destination file. If omitted, the recording module picks a default under the project's `.unterm/sessions/`.
- Returns: `{ session_id, path, bytes, block_count }`. Active recording export returns the recording id; inactive one-off export returns a freshly-generated UUID.

---

## Exec

Higher-level wrappers around `session.input` for the common case of "run a command". Most agents will reach for these instead of typing `\r` themselves.

Every exec method is policy-checked: if `policy.set` has been called with `enabled: true` and the command matches a `blocked_patterns` substring, the call returns `-32603` with message `"Command blocked by policy: <pattern>"`. See the [Policy](#policy) section.

### `exec.run`

Send a command and a carriage return. Returns immediately — does not wait for the command to finish.

**Params:**

| Name | Type | Required | Description |
|---|---|---|---|
| `id`/`session_id` | number/string | yes | Target pane |
| `command` | string | yes | Shell command to run |

**Returns:** `{ sent: true }`

```json
{"jsonrpc":"2.0","id":5,"method":"exec.run",
 "params":{"id":7,"command":"cargo test --workspace"}}
```

### `exec.run_wait`

Send a command, *append a shell-specific sentinel*, and poll the pane's text every 200ms until the sentinel appears. Returns the captured output diff.

**Params:**

| Name | Type | Required | Description |
|---|---|---|---|
| `id`/`session_id` | number/string | yes | Target pane |
| `command` | string | yes | Shell command to run |
| `timeout_ms` | number | no | How long to wait, default `30000` |

**Returns:** `{ output: string, exit_status: "completed" | "timeout", timed_out: bool, marker: string }`

The sentinel is a fresh UUID-based string (`__UNTERM_DONE_<uuid>__`) appended after the command via shell-appropriate syntax: `; echo …` for unix shells, `; Write-Output …` for PowerShell, `& echo …` for `cmd`. The `output` field is the diff between pre- and post-execution screen text, with the command line and the sentinel stripped.

This is the "blocking subprocess" pattern: simple, but heuristic — it can confuse multi-line prompts, programs that redraw the screen (htop, vim), or commands that themselves contain the sentinel as a literal. For those, prefer `exec.run` + manual polling with `screen.search`.

### `exec.status`

Probe whether the foreground process looks like a shell or like a running command.

**Params:** `id`/`session_id`.

**Returns:** `{ status: "idle" | "running", foreground_process: string }`

Same heuristic as `session.idle` but with a different return shape. Either is fine; pick whichever your client code is already using.

### `exec.cancel`

Send Ctrl+C (`\x03`) to the pane.

**Params:** `id`/`session_id`.

**Returns:** `{ cancelled: true }`

### `exec.send`

Alias for `session.input`. Documented under [Session](#session).

---

## Signal

### `signal.send`

Send a control signal as a control character to the pane. Cross-platform — the actual POSIX signal isn't sent; the appropriate Ctrl-character is.

**Params:**

| Name | Type | Required | Description |
|---|---|---|---|
| `id`/`session_id` | number/string | yes | Target pane |
| `signal` | string | yes | One of `SIGINT`/`INT`, `SIGTSTP`/`TSTP`, `SIGQUIT`/`QUIT`, `EOF` |

**Returns:** `{ sent: true, signal: string }` on success, or `-32603` `"Unsupported signal: ..."` for anything else.

The bytes sent: `SIGINT`→`\x03`, `SIGTSTP`→`\x1a`, `SIGQUIT`→`\x1c`, `EOF`→`\x04`. On Windows the same bytes go in; the shell decides what to do with them.

```json
{"jsonrpc":"2.0","id":6,"method":"signal.send","params":{"id":7,"signal":"SIGINT"}}
```

---

## Screen

Read or navigate what's on the pane. Reads are side-effect free;
`screen.scroll` only changes the visible viewport when `goto: true`, and
`screen.clear` explicitly discards history.

### `screen.read`

Visible viewport with absolute row indices and per-cell info.

**Params:** `id`/`session_id`.

**Returns:** `{ cells: [{ row, text }, ...], cursor: { x, y, visible }, cols, rows, scrollback_rows }`

Each cell entry covers one row, not one cell — the name reflects an older intention. `text` is the row trimmed of trailing whitespace.

### `screen.text`

Same as `screen.read` but the rows come back as a flat `lines: string[]` instead of `cells: [{row,text}]`. Use this when you don't care about absolute row numbers (you usually don't).

**Params:** `id`/`session_id`.

**Returns:** `{ lines: string[], cursor: { x, y }, cols, rows }`

### `screen.cursor`

Cursor position and shape only.

**Params:** `id`/`session_id`.

**Returns:** `{ x, y, visible, shape }` — `shape` is the `Debug` formatting of the underlying `CursorShape` enum, e.g. `"Default"`, `"BlinkingBlock"`, `"SteadyUnderline"`.

### `screen.scroll`

Read an absolute slice of the scrollback. Use this when you want history before what's visible.

**Params:**

| Name | Type | Required | Description |
|---|---|---|---|
| `id`/`session_id` | number/string | yes | Target pane |
| `offset` | number | no | Starting row, default `0` |
| `count` | number | no | Number of rows to read, default `100` |
| `goto` | bool | no | Also move the pane's logical viewport to `offset` |

**Returns:** `{ lines, offset, count, scrolled_to, goto_skipped }`.

### `screen.clear`

Discard a pane's scrollback. By default the current viewport remains visible;
pass `include_screen: true` to clear it as well.

**Params:** optional `id`/`session_id` (defaults to active pane), optional
`include_screen`.

**Returns:** `{ ok: true, id, include_screen }`.

### `screen.search`

Substring search across visible viewport + scrollback.

**Params:**

| Name | Type | Required | Description |
|---|---|---|---|
| `id`/`session_id` | number/string | yes | Target pane |
| `pattern` | string | yes | Literal substring (not regex) |
| `max_results` | number | no | Cap on matches, default `50` |
| `goto` | bool | no | Scroll the GUI viewport to the first match so the user sees it. Default `false`. |
| `goto_match` | number | no | Jump to the Nth match instead (0-based, clamped; implies `goto`). Call again with the next index to step through matches. |

**Returns:** `{ matches: [{ row, col, text }, ...], total: number, scrolled_to: { row, match_index } | null, goto_skipped?: { reason, engine, row, match_index } | null }`

`goto` only moves the user-visible GUI viewport when the active engine owns one. Headless or next-core engines still return matches and set `goto_skipped` instead of failing the search.

`row` is the **stable row index** — the same coordinate space as
`screen.scrollback_text`'s `first_row` / `start_line` — so a match stays
addressable as new output scrolls in. `col` is the character column of the
first occurrence in that line.

Match is `String::contains`, case-sensitive, no regex. If you need regex, do it client-side after fetching `screen.text`.

### `screen.scrollback_text`

Dump the scrollback plus visible viewport as text. This is the text-first companion to `capture.scrollback` for LLM hand-off.

**Params:**

| Name | Type | Required | Description |
|---|---|---|---|
| `id`/`session_id` | number/string | no | Target pane; defaults to the active pane |
| `escapes` | bool | no | Preserve ANSI color/style escapes |
| `start_line` | number | no | Absolute stable row index |
| `end_line` | number | no | Absolute stable row index, exclusive |
| `tail_lines` | number | no | Keep only the last N rows in the selected range |

**Returns:** `{ text, lines?, first_row, row_count, cols, escapes, scrollback_top, physical_top, viewport_rows }`

### `screen.detect_errors`

Run a hardcoded error-pattern scan over the visible viewport.

**Params:** `id`/`session_id`.

**Returns:** `{ has_errors: bool, errors: [{ row, text, pattern }] }`

The pattern list is fixed in the binary: `error:`, `Error:`, `ERROR:`, `error[`, `fatal:`, `Fatal:`, `FATAL:`, `panic:`, `PANIC:`, `not found`, `command not found`, `Permission denied`, `permission denied`, `No such file or directory`, `failed`, `FAILED`, `traceback`, `Traceback`, `Exception`, `exception:`, `segfault`, `Segmentation fault`. First match per row wins.

This is meant for "does this look like the build broke?" not for serious log analysis.

### `ghost.debug`

Read the ghost-text predictor's view of a pane. Read-only, never mutates state — this exists so a remote debugger can see whether the input buffer is tracking keystrokes, whether command commits are landing, and what (if anything) the predictor is currently proposing.

**Params:** `id` / `pane_id` (number, required).

**Returns:** `{ input_buffer, input_buffer_len, ghost, commit_count, recent_commits, global_commit_count, recent_global_commits }` — `ghost` is the currently proposed completion or `null`; `recent_commits` is the last 10 committed commands for this pane, `recent_global_commits` the cross-pane pool. If the pane has never seen a key event, returns `{ empty: true, pane_id }` instead.

---

## Capture

Screen and clipboard captures. PNG output goes to `~/.unterm/screenshots/`; clipboard images go to `~/.unterm/clipboard/`. Both directories are created on demand.

### `capture.screen`

Snapshot every pane's text plus a full-display PNG.

**Params:**

| Name | Type | Required | Description |
|---|---|---|---|
| `include_base64` | bool | no | Inline the PNG bytes as base64 in the response, default `false` |

**Returns:** `{ captures: [{ session_id, title, screen, type: "text" }, ...], image: { path, ... }, type: "image/png", text_snapshot: true }`

`image.path` is the absolute path to the PNG on disk. With `include_base64: true` the response also gets `image.base64`.

### `capture.window`

Snapshot one specific window — by partial title match or by pid. Returns one pane's text + the windowed PNG.

**Params:**

| Name | Type | Required | Description |
|---|---|---|---|
| `title` | string | no | Substring matched against pane titles and pane ids |
| `pid` | number | no | Process id of the window to capture |
| `include_base64` | bool | no | Inline base64 PNG, default `false` |

**Returns:** `{ session_id?, title?, screen?, image: {...}, type: "image/png", text_snapshot: bool }`

If neither `title` nor `pid` matches a known pane, only the image and `text_snapshot: false` come back — the OS-level windowed capture still runs.

### `capture.select`

Used to mean "interactive region selection". In headless MCP mode this is impossible — there's no GUI to draw the selection rectangle on — so the call falls back to a full-screen capture and notes that in the response.

**Params:** none.

**Returns:** `{ image: {...}, type: "image/png", mode: "screen_fallback", message: "Interactive region selection is not available in headless MCP mode; captured the screen instead." }`

### `capture.scrollback`

Render a terminal pane's entire scrollback plus viewport into one tall PNG. This is a headless re-render from the pane text model, not a stitched screen capture, so it works while the window is occluded. In `next-core`, this currently renders a plain-text PNG with default terminal colors; styled cell parity comes later.

**Params:**

| Name | Type | Required | Description |
|---|---|---|---|
| `id` | number | no | Pane id. Defaults to the active pane. |
| `max_rows` | number | no | Cap rows rendered; keeps the most recent rows. |
| `dpi` | number | no | Raster DPI, clamped to 48-288. |

**Returns:** `{ path, width, height, rows, cols, truncated, first_row, session_id, type: "image/png" }`

### `capture.window_scroll`

Scrolling long screenshot of another app's window. On macOS, Unterm finds the target window, synthesizes wheel events, and stitches frames by row matching. On Windows and Linux this currently returns an error; use `capture.window` for a single-frame capture there.

**Params:**

| Name | Type | Required | Description |
|---|---|---|---|
| `app` | string | no | App-name substring. |
| `title` | string | no | Window-title substring. |
| `pid` | number | no | Owning process id. |
| `under_cursor` | bool | no | macOS: target the window under the cursor instead of matching by app/title/pid. |
| `max_frames` | number | no | Frame cap, clamped to 2-120. |
| `settle_ms` | number | no | Delay between scroll frames, clamped to 100-2000ms. |
| `activate` | bool | no | Whether to activate the target window before capture. |
| `restore_scroll` | bool | no | Whether to restore the target window's scroll position after capture. |

**Returns:** `{ path, width, height, frames, window: { app, title, pid, window_id }, hint, type: "image/png" }`

### `capture.clipboard`

Read the OS clipboard. Cross-platform: Win32 `OpenClipboard`/`GetClipboardData` on Windows, `pbpaste` on macOS, `xclip`/`wl-paste` on Linux.

**Params:** none.

**Returns:** depends on what's on the clipboard.

- Text: `{ type: "text", content: "..." }`
- Image: `{ type: "image", format: "png", image_path, width, height, bit_depth, size_bytes, base64 }` — the image is always saved to `~/.unterm/clipboard/clipboard_<timestamp>.png` and base64 is always included for images (in contrast to `capture.screen`/`capture.window` where base64 is opt-in).

Errors if the clipboard is empty or contains an unsupported format.

---

## Upload

### `upload.file`

PUT a local file to a user-configured object-storage bucket (Aliyun OSS, Tencent COS, or Qiniu Kodo) and return the public URL. Designed to pair with `capture.*`: screenshot, then `upload.file`, then paste the URL into chat / an issue / a doc — no dragging PNGs around.

Credentials live in `~/.unterm/upload.json` (set up interactively with `unterm-cli upload setup`). They are never logged and never appear in the response — only the URL, provider name, and storage key come back.

**Params:**

| Name | Type | Required | Description |
|---|---|---|---|
| `path` | string | yes | Local file to upload |
| `provider` | string | no | `"oss"`, `"cos"`, or `"qiniu"`; defaults to `default_provider` from the config |
| `key` | string | no | Object key; defaults to a derived `<prefix><timestamp>-<basename>` |

**Returns:** `{ url, provider, key, size }`

Errors if no provider is resolvable, the provider block is missing from the config, or the upload itself fails.

---

## Proxy

Read and write the proxy configuration that lives at `~/.unterm/proxy.json`. The values flow through to environment variables (`HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`, `NO_PROXY`) for child processes spawned by Unterm. Auto-detection runs from `system_proxy::detect()` when the user is in `mode: "auto"`.

### `proxy.status`

Current proxy state plus a live reachability probe when enabled.

**Params:** none.

**Returns:** `{ enabled, mode, http_proxy, socks_proxy, no_proxy, current_node, node_count, health }`

`health` is `null` when disabled, otherwise `{ source, url, reachable, hint? }`. `source` is `"manual"` (user set explicit URL), or whatever `system_proxy::detect` says (typically `"system"`, `"clash"`, or `"auto"`). `hint` is a human-readable message when `reachable: false`.

### `proxy.nodes`

List configured proxy nodes (named upstream URLs) and which one is current.

**Params:** none.

**Returns:** `{ current_node: string|null, nodes: [{ name, url, latency_ms, available }, ...] }`

Latencies and availability are populated by `proxy.speedtest`; reading `proxy.nodes` doesn't probe anything fresh.

### `proxy.switch`

Activate one of the configured nodes by name. Sets `enabled: true`, `mode: "manual"`, and writes through to `proxy.json`.

**Params:** `node_name` (string, required).

**Returns:** `{ switched: true, current_node, http_proxy }`

Errors with `"Proxy node '<name>' not found"` if the name doesn't match.

### `proxy.speedtest`

Probe one node (or all of them) and write `latency_ms` + `available` back to disk.

**Params:**

| Name | Type | Required | Description |
|---|---|---|---|
| `node_name` | string | no | Limit to one node; omit to probe all |
| `timeout_ms` | number | no | Per-node TCP connect timeout, default `3000` |

**Returns:** `{ results: [{ name, url, available, latency_ms }, ...] }`

The probe is a `TcpStream::connect_timeout` to the host:port parsed out of the URL. SOCKS, HTTP, and HTTPS URLs all work — no actual proxy protocol is exercised, just the TCP layer.

### `proxy.configure`

Write a full proxy config in one call: enabled flag, mode, manual URLs, full node list, and current node. This is the one to call when setting up proxy from scratch.

**Params:**

| Name | Type | Required | Description |
|---|---|---|---|
| `enabled` | bool | no | Default `true` |
| `mode` | string | no | `"manual"` or `"auto"`, default `"manual"` (ignored if `enabled: false`, gets forced to `"off"`) |
| `http_proxy` | string | no | URL like `"http://127.0.0.1:7890"` |
| `socks_proxy` | string | no | URL like `"socks5://127.0.0.1:7891"` |
| `no_proxy` | string | no | Comma-separated bypass list |
| `nodes` | array | no | `[{ name, url|http_proxy }, ...]` — replaces existing node list |
| `current_node` | string | no | Picks a node by name; sets its url as `http_proxy` |

**Returns:** `{ configured: true, status: <result of proxy.status> }`

### `proxy.disable`

Turn the proxy off. Equivalent to `configure` with `enabled: false`.

**Params:** none.

**Returns:** `{ disabled: true }`

### `proxy.env`

Resolve the proxy state to environment-variable form, doing the same auto-detection logic that Unterm uses when spawning child processes.

**Params:** none.

**Returns:** `{ enabled, env: { HTTP_PROXY?, HTTPS_PROXY?, ALL_PROXY?, NO_PROXY? } }`

When disabled, `env` is empty. When enabled, manual URLs win over auto-detected ones; if both are missing, only `NO_PROXY` ends up populated.

---

## Workspace

Save and restore named layouts of pane (cwd, title) tuples to `~/.unterm/workspaces/<name>.json`. Restore opens new tabs for the saved cwd entries, and also supports `dry_run` when a caller only wants to inspect what would be opened.

### `workspace.save`

Snapshot the current set of panes.

**Params:** `name` (string, required).

**Returns:** `{ saved: true, name, sessions: number }`

### `workspace.restore`

Restore a saved workspace by opening each saved cwd as a new tab. Existing panes are left alone.

**Params:** `name` (string, required), `dry_run` (bool, optional, default `false`).

**Returns:** `{ restored, dry_run, name, path, workspace, planned: [{ saved_id, title, cwd }], created: [{ saved_id, cwd, created }], failed: [{ saved_id, cwd, error }] }`

### `workspace.list`

Enumerate saved workspaces. Each entry includes enough metadata for an agent to pick a candidate without reading files from disk directly.

**Params:** none.

**Returns:** `{ workspaces: [{ name, path, saved_at, session_count, error? }, ...] }`

---

## Orchestrate

Multi-pane convenience methods. These are thin wrappers over `session.create` + `session.input` — you can build the same patterns by hand if you prefer.

### `orchestrate.launch`

`session.create` plus a 500ms wait plus an `exec.run`-style command send. Used when you want to open a pane and immediately run something in it.

**Params:** same as `session.create` (`cwd`, `cols`, `rows`) plus `command` (string, optional).

**Returns:** same as `session.create` — `{ id, session_id, title, cols, rows }`.

If `command` is omitted, this is identical to `session.create`. If supplied, the command is sent with a carriage return after a 500ms shell-init delay.

### `orchestrate.broadcast`

Send the same command to multiple panes in one call.

**Params:**

| Name | Type | Required | Description |
|---|---|---|---|
| `command` | string | yes | Command to send (followed by `\r` per pane) |
| `sessions` | array of strings | yes | Pane ids as decimal strings |

**Returns:** `{ results: [{ session_id, sent?, error? }, ...] }`

Bad ids and missing panes don't fail the whole call — they show up as `error` entries in `results`.

### `orchestrate.wait`

Poll one pane until its text contains a substring, or the timeout fires.

**Params:**

| Name | Type | Required | Description |
|---|---|---|---|
| `id`/`session_id` | number/string | yes | Target pane |
| `pattern` | string | yes | Literal substring (no regex) |
| `timeout_ms` | number | no | Default `10000` |

**Returns:** `{ matched: true, pattern }` on hit, `{ matched: false, timed_out: true, pattern }` on timeout. Polls every 200ms.

This is the "wait for the inner agent's prompt marker before sending the next instruction" primitive. Compare with `exec.run_wait` which does its own sentinel injection.

---

## Theme

There is no `theme.*` namespace on the MCP server. Theme switching is done through the HTTP settings server at `127.0.0.1:<http_port>` — the same `~/.unterm/server.json` file lists that port. The HTTP server exposes the Tailwind+Alpine settings SPA at `/` and REST endpoints such as `/api/theme`, behind the same auth token. From a shell, `unterm-cli theme list` / `unterm-cli theme switch <id>` use that API.

If you see references to `theme.list` / `theme.switch` in older docs, those are HTTP endpoints, not MCP methods. The MCP wire protocol does not currently surface theme management. The same is true of the interface language (`unterm-cli lang …`) and of `unterm.conf`: there is no `lang.*` or `settings.*` namespace.

---

## Instance

Instances, and the windows inside them.

An *instance* is one Unterm front end, with a NATO-phonetic id and its own MCP port + auth token. To drive a peer instance you connect to *its* MCP port directly with *its* token — there is no cross-instance forwarding through your local connection.

Since v0.68 one instance can hold **several windows**, so an instance id no longer names a window. Windows have ids of their own (`1`, `2`, …), handed out by `instance.new_window` and accepted by `instance.focus`; `instance.windows` lists them. A window id belongs to the front end that issued it and means nothing to a peer.

A headless Core has no windows at all: it publishes itself as the instance `core`, `instance.windows` returns `[]`, and everything session-shaped keeps working.

### `instance.list`

Enumerate every live Unterm instance on this machine. Stale entries (PID dead) are filtered out by the storage layer.

**Params:** none.

**Returns:** `{ instances: [{ id, pid, started_at, mcp_port, http_port, title, cwd, version, platform, lifecycle }, ...], registry }`

The entry for the instance you are connected to also carries `windows` — the same list `instance.windows` returns. Peers' windows are not included: a window lives behind its own front end's port, so reporting a peer's windows from here would be reporting what this process cannot see.

Note that this *omits* `auth_token` — the listing tells you a peer exists and where to find it, but to actually talk to a peer you read the peer's `~/.unterm/instances/<id>.json` file directly to grab its token. The exception is `core`, a headless Core: it publishes its own record instead of registering there, and `unterm-cli --instance core` reaches it.

### `instance.info`

This instance's own metadata, *including* its auth token. Useful for confirming "yes, I'm talking to the right window".

**Params:** none.

**Returns:** `{ id, pid, started_at, mcp_port, http_port, auth_token, title, cwd, version, platform }`

### `instance.set_title`

Pin a custom display title for this instance. Overrides the auto-derived `Unterm — <id> — <project>` window title and shows up in `instance.list` so peers can route to the right window. Pass `null` (or omit `title`) to clear the override.

**Params:** `title` (string, optional). Empty string is treated as "clear".

**Returns:** `{ ok: true, title: string|null }`

### `instance.focus`

Bring one of this instance's windows to the foreground. **Cross-instance focus is intentionally not supported here** — to focus a peer, connect to that peer's MCP port directly and call `instance.focus` there. Keeps the auth model simple (each instance only ever acts on itself with its own token).

**Params:** `window_id` (number, optional). Omit it to mean the window in front, which is what every caller meant before an instance could have more than one. Naming a window this front end does not have is an error rather than a redirect to whichever window is handy.

**Returns:** `{ ok: true, window_id, mux_window_id }`

`ok` means the window was found and the platform was asked. Asking is not getting: Windows refuses the foreground to a process that does not already hold it, and the taskbar is asked for attention instead — so read `instance.windows` for what actually happened. Between windows of the front end the user is already in, it goes through.

With every window parked in the tray there is no window to raise, and the call asks the event loop to rebuild one rather than failing. An agent's "look at this" should bring the terminal back, and that is exactly when the terminal is not on screen.

### `instance.windows`

Every window this front end is showing.

**Params:** none.

**Returns:** `{ windows: [{ id, title, focused, tabs, panes }, ...] }`

`panes` is the sessions in that window, which is how you tell which window a session is in. `focused` is the one with the OS focus. A headless Core returns `[]`.

### `instance.new_window`

Open another window on this front end.

**Params:** none.

**Returns:** `{ ok: true, window_id }`

The id is reserved and returned at once; the window itself is built on the next turn of the event loop, which is the only place a window can be made. The id is therefore usable before the window is on screen — but it does not appear in `instance.windows` until it is.

Since v0.68 this opens a window in the process that is already running instead of launching a second one, which is the difference between 31 ms and 587 ms.

### `instance.lifecycle`

Read registration, liveness, ownership, and shutdown diagnostics for the current instance. This is the structured form of "is the registry entry healthy, who owns this window, and what would shutdown/unregister touch?" It is intended for agents that need to distinguish stale metadata from a live window before routing work.

**Params:** none.

**Returns:** `{ instance, registry, lifecycle }`, with tokens and other sensitive values redacted.

### `instance.close`

Unregister the current instance from the product registry, or return the unregister plan without mutating anything. This is not a pane-kill primitive; use `session.destroy` for panes. Use `dry_run: true` before automation that is cleaning stale instance state.

**Params:** `dry_run` (bool, optional, default `false`).

**Returns:** `{ ok, dry_run, instance, registry }`

---

## Agent identity and trust

Every MCP connection is anonymous until it says otherwise. `agent.identify` tags the connection with a name so audit-log entries and confirmation banners group by agent ("claude-code: 47 writes") instead of by connection id. The trust methods manage the persistent list of agents whose PTY writes skip the confirmation banner — the same list the user builds by pressing Alt+A on a banner.

A note on the write gate, since it interacts with identity: when `mcp_input_confirmation` is `"always"` or `"first_time_per_agent"`, PTY-writing calls (`session.input`, `exec.*`) park the calling worker on a GUI confirmation banner unless the agent's name is on the static Lua `mcp_trusted_agents` list or the runtime trust list. An un-identified connection gets gated as `"anonymous"`. Identify first; it costs one call.

### `agent.identify`

Self-tag the calling connection. Call this right after `auth.login`.

**Params:**

| Name | Type | Required | Description |
|---|---|---|---|
| `name` | string | yes | Agent name, non-empty, ≤ 64 chars — e.g. `"claude-code"` |
| `version` | string | no | Agent version string |
| `capabilities` | string[] | no | Free-form capability tags |

**Returns:** `{ status: "ok", name, first_time, identified_at }` — `first_time` is `true` if this name has never been seen by this Unterm process before. The identification itself is audited.

### `agent.whoami`

Echo back the connection's current identity.

**Params:** none.

**Returns:** the identity object `{ name, version, capabilities, peer_addr, identified_at }`, or `{ name: "anonymous", peer_addr }` if `agent.identify` was never called on this connection.

### `agent.list_trusted`

Read-only snapshot of the trust state.

**Params:** none.

**Returns:** `{ runtime: string[], static_config: string[], audit_counts: [{ agent, writes }] }` — `runtime` is the persisted-plus-this-session trust list (Alt+A / `agent.trust`), `static_config` is the user's `mcp_trusted_agents` Lua config, and `audit_counts` is a per-agent write count derived from the audit log, highest first.

### `agent.trust`

Promote an agent name to the persistent trust list — equivalent to the user pressing Alt+A on a confirmation banner. Future PTY writes from that agent skip the banner, across restarts.

**Params:** `name` (string, required).

**Returns:** `{ ok: true, name, added }` — `added` is `false` if it was already trusted.

### `agent.untrust`

Remove an agent from the persistent trust list; its next write triggers the banner again. Cannot remove names from the static Lua `mcp_trusted_agents` config — the user has to edit the file for that (both lists are visible via `agent.list_trusted` so you can tell why an untrust "didn't take").

**Params:** `name` (string, required).

**Returns:** `{ ok: true, name, removed }`

---

## Agent status and the Inbox

The Agent Cockpit's state engine tracks one status per pane that runs an AI agent (claude, codex, gemini, aider, opencode, …), folded together from four signal layers — official hook events (`agent.signal`) > OSC parsing (title / progress / notification / bell) > foreground-process detection > screen-text heuristics. A weaker layer never overrides a stronger layer's recent verdict, and `waiting` is sticky: only user input in that pane, a hook event, or an OSC transition clears it, so an Inbox entry can't flicker away on a stray process poll.

States: `working` (running a turn), `waiting` (needs the user — the Inbox condition), `done` (turn just finished; decays to idle after `cockpit_done_hold_secs`), `idle` (agent at its prompt).

All three methods respect the `cockpit_enabled` config option — when the cockpit is off they return `{ enabled: false, ... }` with empty data rather than erroring.

### `agent.status`

The cockpit's view of which agent runs in which pane and what it's doing.

**Params:** `session_id` (string, optional) — with it, one pane's status (or `null` if that pane hosts no tracked agent); without it, every tracked pane in Inbox order (waiting first, longest-waiting at the top, then working, done, idle).

**Returns:** `{ enabled: true, agents: [status, ...] }` (no pane given) or `{ enabled: true, agent: status | null }` (pane given), where each status is:

```json
{"pane_id": 3, "agent": "claude", "state": "working", "for_secs": 42,
 "task_hint": "Fix the login bug", "last_signal": "osc-title", "fleet_id": null}
```

`for_secs` is how long the pane has been in the current state. `task_hint` comes from the agent's OSC title when available. `last_signal` names the signal that produced the current state (`hook`, `osc-title`, `osc-progress`, `osc-notify`, `bell`, `process`, `screen-text`, `user-input`, `done-decay`) — useful when debugging why the cockpit thinks what it thinks.

### `agent.signal`

Report an agent lifecycle event from an official hook — Claude Code hooks, Codex notify, Aider's notifications-command. This is the strongest state signal; it beats everything the passive layers infer. `unterm-cli agent enable-hooks` wires the supported agents' hook configs to call this automatically (hooks pass `$WEZTERM_PANE` so the event lands on the right pane).

**Params:**

| Name | Type | Required | Description |
|---|---|---|---|
| `event` | string | yes | One of `working`, `waiting`, `done`, `idle` |
| `id` / `session_id` | number/string | no | Target pane; defaults to the active pane |
| `agent` | string | no | Agent name; defaults to the connection's `agent.identify` name, else `"agent"` |

**Returns:** `{ ok: true, pane_id, agent, event }`. Invalid events return `-32603` with `"Invalid 'event' ...: expected working|waiting|done|idle"`. The call is audited.

```json
{"jsonrpc":"2.0","id":9,"method":"agent.signal",
 "params":{"session_id":"3","agent":"claude","event":"waiting"}}
```

### `cockpit.inbox`

Every tracked agent joined with its tab/window location, sorted waiting-first, so a client can jump straight to whichever agent wants attention. This is the wire form of the Shift+Ctrl+A Inbox overlay.

**Params:** none.

**Returns:** `{ enabled: true, items: [...] }` — each item is an `agent.status` entry plus `pane_title`, `tab_id`, and `window_id`:

```json
{"jsonrpc":"2.0","id":10,"result":{"enabled":true,"items":[
  {"pane_id":3,"agent":"claude","state":"waiting","for_secs":95,
   "task_hint":"Fix the login bug","last_signal":"osc-notify","fleet_id":"fix-the-login-bug",
   "pane_title":"✳ Fix the login bug","tab_id":2,"window_id":0}
]}}
```

---

## Fleet

Run one task across N agents in N isolated git worktrees, one tab each. `fleet.launch` adds a worktree + branch per member beside the repo (`../<repo>.fleet/<fleet-id>-<n>/`, branch `fleet/<fleet-id>-<n>`), opens a tab per member, and types the agent's launch command into the fresh shell. Fleets persist in `~/.unterm/fleets.json`, so the Review page and `fleet.clean` survive a restart — panes dying does *not* remove a fleet; the worktrees hold the work product until every member is merged or discarded.

`fleet.launch`, `fleet.retry`, and `fleet.clean` are audited write operations — they appear in `session.audit_log` — and member panes are subject to the same PTY-write gate as `session.input` (the launch command is typed into a pane).

### `fleet.launch`

Launch a fleet. Blocking (worktree creation + tab spawns); the repo must be clean — commit or stash first, or the call errors.

**Params:**

| Name | Type | Required | Description |
|---|---|---|---|
| `task` | string | yes | The task prompt, passed verbatim (shell-quoted) to each agent |
| `agents` | string[] | yes | One entry per member, e.g. `["claude","claude","codex"]`. 1–8 members. Repeats are fine — that's the A/B pattern |
| `cwd` | string | no | Any path inside the target repo; defaults to the active pane's cwd |

Built-in launch commands: `claude <task>`, `codex <task>`, `gemini -i <task>`, `aider --message <task>`; any other name runs as `<name> <task>`.

**Returns:** the full fleet record:

```json
{"id":"fix-the-login-bug","task":"Fix the login bug","base_repo":"/Volumes/Dev/code/app",
 "base_branch":"master","created_at":"2026-07-20T09:00:00Z",
 "members":[
   {"agent":"claude","agent_cmd":"claude 'Fix the login bug'",
    "worktree":"/Volumes/Dev/code/app.fleet/fix-the-login-bug-1",
    "branch":"fleet/fix-the-login-bug-1","pane_id":12,
    "checkpoint":"<sha the worktree started from>","review":"pending"}
 ]}
```

`checkpoint` is the base HEAD sha — the review baseline for `review.diff`. A member whose tab failed to spawn gets `pane_id: null` but keeps its worktree — relaunch it with `fleet.retry`.

### `fleet.list`

All fleets with member branches, worktrees, and review states.

**Params:** none.

**Returns:** `{ fleets: [...] }` — same fleet shape as `review.list` below, i.e. each member carries `n` (1-based index), `agent`, `branch`, `worktree`, `checkpoint`, `review` (`"pending"` / `"merged"` / `"discarded"`), `pane_id`, `agent_state` (live cockpit state, or `null` if the pane is gone), `attempt` (launch count, incremented by `fleet.retry`), `last_started_at`, and `last_launch_error` (most recent pane-spawn failure; `null` after a successful launch/retry).

### `fleet.clean`

Remove a fleet: kill surviving panes, remove worktrees and branches, drop the record. Refuses while members are still `pending` review unless `force: true` — merge or discard them first. Audited.

**Params:** `id` / `fleet_id` (string, required), `force` (bool, optional, default `false`).

**Returns:** `{ ok: true, id }`

### `fleet.retry`

Restart a failed or stalled member in its **existing** isolated worktree — after a crashed agent, a closed tab, or a member whose tab never spawned. The worktree, branch, and checkpoint are retained, along with every committed, staged, unstaged, and untracked change; only the pane association is replaced, so a retry never deletes or resets work. The member's previous pane (if any) is closed before the new agent starts, so two processes never edit the same worktree concurrently. Audited.

Only `pending` members can be retried — `merged`/`discarded` ones error. The call also errors if the worktree no longer exists or is no longer on the member's fleet branch. The new attempt is persisted *before* the tab is spawned: if spawning fails, the member shows `pane_id: null` with the error in `last_launch_error`, and can simply be retried again.

**Params:**

| Name | Type | Required | Description |
|---|---|---|---|
| `fleet_id` | string | yes | Fleet id |
| `member` | string | yes | 1-based member index (`"2"`) or branch name / suffix |

**Returns:** the updated member record — the `fleet.launch` member shape plus `attempt` (launch count, incremented on every retry), `last_started_at` (RFC 3339 timestamp of the latest launch/retry), and `last_launch_error` (`null` after a successful retry).

```json
{"jsonrpc":"2.0","id":12,"method":"fleet.retry",
 "params":{"fleet_id":"fix-the-login-bug","member":"1"}}
```

---

## Review

Checkpoints, diffs, rollback, and merge for agent-produced changes. Two kinds of baseline feed this namespace: fleet members diff against their worktree's start commit, and "loose" (non-fleet) agent runs get automatic checkpoints — whenever the cockpit sees an agent start working in a repo, it snapshots the entire worktree (tracked + untracked) as a dangling commit via a temporary index, without ever touching HEAD, the real index, or the files. The last 20 checkpoints per repo are remembered in `~/.unterm/checkpoints.json`, debounced to one per minute.

`review.rollback`, `review.merge`, and `review.verify` are **audited operations** — every call lands in `session.audit_log` with the repo/fleet detail. `review.rollback` and `review.merge` mutate the working tree, so treat them with the same care as any policy-gated write; `review.verify` executes a command in the member's worktree. `review.rollback` is destructive by design; confirm with the user before calling it.

### `review.list`

Review overview — everything the Review page needs in one call.

**Params:** none.

**Returns:** `{ fleets: [...], checkpoints: [{ repo, checkpoints: [{ sha, at, agent, pane_id }] }] }` — fleets in the shape described under `fleet.list`, checkpoints newest-first per repo. Unlike `fleet.list`, each member is additionally enriched with its latest `verification` record (or `null`), a deterministic `score` (verification status dominates; smaller diffs break ties), and a 1-based `rank` within the fleet — this is also how you poll a `review.verify` run for completion.

### `review.diff`

Line-level diff of a worktree against a checkpoint. Untracked files are synthesized as additions, so agent-created files show up (plain `git diff <sha>` misses them).

**Params:** either the fleet form or the repo form:

| Name | Type | Required | Description |
|---|---|---|---|
| `fleet_id` | string | fleet form | Fleet id |
| `member` | string | fleet form | 1-based member index (`"2"`) or branch name / suffix |
| `repo` | string | repo form | Any path inside the repo |
| `from` | string | repo form | Checkpoint sha to diff against |

**Returns:** `{ repo, from, files: [{ path, added, deleted, untracked }], patch }` — `added`/`deleted` are line counts as strings (`"?"` for untracked), `patch` is one concatenated unified diff.

```json
{"jsonrpc":"2.0","id":11,"method":"review.diff",
 "params":{"fleet_id":"fix-the-login-bug","member":"1"}}
```

### `review.verify`

Run a validation command asynchronously in a fleet member's isolated worktree — the evidence behind the `review.merge` gate. With no `command`, a conventional one is inferred from a small, auditable set of project markers; Unterm never executes scripts discovered by scanning source files:

| Marker | Inferred command |
|---|---|
| `Cargo.toml` | `cargo test` |
| `go.mod` | `go test ./...` |
| `pnpm-lock.yaml` / `yarn.lock` / `package-lock.json` or `package.json` | `pnpm test` / `yarn test` / `npm test` — only if `package.json` has a non-empty `scripts.test` |
| `uv.lock` | `uv run pytest` |
| `pyproject.toml`, `pytest.ini`, or `setup.cfg` | `python -m pytest` |
| `pom.xml` | `mvn test` |
| `gradlew` / `gradlew.bat` | `./gradlew test` (`gradlew.bat test` on Windows) |
| `*.sln` / `*.csproj` | `dotnet test` |

If nothing matches, the call errors and asks for an explicit command.

The call returns immediately with a `pending` record; the command runs on a worker thread in the worktree (via `sh -c` on Unix, `cmd.exe /C` on Windows). Status flows `pending → running → passed | failed | timed_out` — `passed` means exit code 0. On timeout the **entire process tree** is killed (process-group `SIGKILL` on Unix, `taskkill /T /F` on Windows), so hung test suites can't leak children. The captured stdout+stderr log is bounded to the last 64 KiB. Records persist in `~/.unterm/verifications.json`; runs left in-flight when Unterm exits are marked `failed` on the next start. Audited.

**Params:**

| Name | Type | Required | Description |
|---|---|---|---|
| `fleet_id` | string | yes | Fleet id |
| `member` | string | yes | 1-based member index or branch name / suffix |
| `command` | string | no | Explicit validation command; takes precedence over inference. Must be non-empty |
| `timeout_secs` | number | no | Default 900 (15 min), clamped to 1–7200 (2 h) |

**Returns:** the verification record: `{ id, fleet_id, member, worktree, command, inferred, status, exit_code, duration_ms, log, started_at, finished_at }` — `member` is normalized to the branch name, `inferred` says whether the command came from marker inference. Poll `review.list` for completion: each member there carries its latest `verification` record.

```json
{"jsonrpc":"2.0","id":13,"method":"review.verify",
 "params":{"fleet_id":"fix-the-login-bug","member":"1","timeout_secs":600}}
```

### `review.rollback`

Restore a repo's worktree to a checkpoint: files *and* untracked state become exactly the snapshot's content (`git clean -fd` removes files that didn't exist then). Destructive for anything newer than the checkpoint — confirm first. The sha is validated before anything runs, so a typo can't half-execute. Audited.

**Params:** `repo` (string, required), `sha` (string, required).

**Returns:** `{ ok: true, repo, sha }`

### `review.merge`

Squash-merge a fleet member's branch into the base repo, leaving the result **staged, not committed** — the user owns the commit. The base repo must be clean, or the call errors. Marks the member `merged`. Audited.

**Verification gate:** a normal merge requires the member's *latest* verification (`review.verify`) to be `passed` — an unverified member, or one whose latest run is `failed`/`timed_out`/still running, errors instead of merging. `force: true` overrides the gate and merges anyway; the override is deliberately surfaced only through this audited API/CLI path (the Review UI stays gated), and the response records both the override and whatever verification record exists.

**Params:** `fleet_id` (string, required), `member` (string, required — index or branch name), `force` (bool, optional, default `false` — merge without a passed verification).

**Returns:** `{ ok: true, fleet, branch, base_head, member_head, merged_at, staged_files, staged_in, verification, verification_forced }` — `verification` is the verification record the merge was gated on (the latest record or `null` when forced without one), `verification_forced` echoes `force`.

### `review.discard`

Mark a fleet member's work as discarded. Nothing is deleted yet — the worktree and branch are removed by the next `fleet.clean`.

**Params:** `fleet_id` (string, required), `member` (string, required).

**Returns:** `{ ok: true, fleet, member }`

---

## Profile

Identity profiles: named bundles of secrets (GitHub PAT, AWS keys, npm token, …), git identity, and SSH config, with one profile bound per Unterm window. The MCP surface is deliberately read-only and never exposes secret *values* — only names, counts, and expiry metadata. Creating and editing profiles happens via `unterm-cli profile` or the GUI.

### `profile.list`

**Params:** none.

**Returns:** `{ profiles: [{ id, display_name, accent_color, description, secret_count, expiration_count, is_default }], default }`

### `profile.current`

Which profile is bound to *this* Unterm window — i.e. what identity the next command in any of its panes runs under. Check this before triggering anything destructive on the wrong account.

**Params:** none.

**Returns:** `{ instance, profile }` — `profile` is the profile id, or `null` when the window isn't profile-bound.

### `profile.audit`

Report secrets expiring within 7 days, plus a healthy count for the rest, without revealing any values. This is the call behind "rotate your GitHub PAT" reminders.

**Params:** none.

**Returns:** `{ warnings: [{ profile, display_name, env_name, expires_on, days_remaining }], healthy_count }`

---

## Meta

### `meta.surface`

Full inventory of this build's automation surface in one call: every MCP method with its summary and param shapes, every CLI subcommand, and the live effective keybindings. This is the machine-readable version of `unterm-cli reference`, and the preferred feature-detection call for new clients (over `server.capabilities`). Both are generated from the same single source of truth, so they cannot drift from dispatch.

**Params:** none.

**Returns:** `{ version, mcp_methods: [{ name, namespace, summary, params: [{ name, kind, required, summary }] }], cli_commands: [...], keybindings: [{ table, key, mods, action }] }`

---

## Server

Self-description methods. These are the calls an agent makes first, before doing anything else, to figure out what it's connected to.

### `server.info`

Server identity. Static, doesn't reach into the mux.

**Params:** none.

**Returns:** `{ name: "Unterm MCP Server", version, engine: "next-core", protocol: "json-rpc-2.0" }`

### `server.health`

Health probe — asks the native next-core engine for readiness and includes MCP/server configuration, session-registry, runtime-pump, and terminal-I/O diagnostics.

**Params:** none.

**Returns:** `{ status: "ok"|"degraded", engine, engine_health: { engine, ready, status, detail, pane_count }, mcp: { bind, port, auth }, mux: { available, pane_count }, terminal: { initial_cols, initial_rows, color_scheme, term } }`

`mux` is retained as a compatibility field. When the selected engine is next-core, `mux.available` is `false` even if the health status is `ok`.

Note: the `mcp.port` field in the response is the *preferred* port (`19876`), not the actually-bound one. To get the actually-bound port, read `~/.unterm/server.json` or `instance.info`.

### `server.capabilities`

Machine-readable capability map: one key per namespace, each value a list of fully-qualified method names. Since v0.55 the map is derived from the same `MCP_METHODS` table that backs `meta.surface`, so it can never drift from what dispatch actually accepts. Kept for back-compat — new clients should prefer [`meta.surface`](#meta), which also carries per-method summaries, param shapes, CLI subcommands, and live keybindings.

**Params:** none.

**Returns:** an object like:

```json
{
  "session": ["session.list", "session.create", ...],
  "exec": ["exec.run", "exec.send", "exec.run_wait", "exec.status", "exec.cancel"],
  "agent": ["agent.identify", "agent.whoami", ..., "agent.status", "agent.signal"],
  "cockpit": ["cockpit.inbox"],
  "fleet": ["fleet.launch", "fleet.list", "fleet.clean", "fleet.retry"],
  "review": ["review.list", "review.diff", "review.verify", "review.rollback", "review.merge", "review.discard"],
  "...": ["..."]
}
```

---

## System

OS-level introspection and elevation.

### `system.info`

Process and platform metadata.

**Params:** none.

**Returns:** `{ name: "Unterm", version, engine: "next-core", platform, arch, active_sessions, hostname }`

`platform` is `std::env::consts::OS` (`"macos"`, `"linux"`, `"windows"`); `arch` is `std::env::consts::ARCH` (`"x86_64"`, `"aarch64"`).

### `system.launch_admin` (Windows only)

Spawn a fresh elevated Unterm window via PowerShell `Start-Process -Verb RunAs`. UAC prompt fires; user has to consent.

**Params:**

| Name | Type | Required | Description |
|---|---|---|---|
| `dry_run` | bool | no | If `true`, return the would-be command without executing |
| `shell` | string | no | `"powershell"`, `"pwsh"`, `"powershell7"`, etc. — picks which shell the elevated session runs |

**Returns:** `{ status: "launched"|"dry_run", requires_uac: true, command: [...] }`

On non-Windows platforms, returns `-32603` `"Administrator launch is only supported on Windows"`. The `selftest.run` self-check uses `dry_run: true` so it doesn't actually trigger UAC.

---

## Policy

Optional command-execution policy applied to `exec.run` and `exec.run_wait`. Disabled by default.

### `policy.set`

Set the policy. Replaces any previously set policy wholesale.

**Params:** the params object IS the policy:

| Name | Type | Required | Description |
|---|---|---|---|
| `enabled` | bool | yes | Whether to enforce the policy |
| `blocked_patterns` | string[] | yes | Substrings; any match blocks |
| `allowed_patterns` | string[] | yes | Stored but not currently enforced |

**Returns:** `{ set: true }`

```json
{"jsonrpc":"2.0","id":7,"method":"policy.set",
 "params":{"enabled":true,"blocked_patterns":["rm -rf /","sudo "],"allowed_patterns":[]}}
```

### `policy.check`

Dry-run a command against the current policy without executing it.

**Params:** `command` (string, required).

**Returns:** `{ allowed: bool, reason?: string }`

When enforcement is on and the command matches, returns `{ allowed: false, reason: "Blocked by pattern: <pattern>" }`. When the policy is disabled, always returns `{ allowed: true, reason: "Policy disabled" }`.

This is what `exec.run`/`exec.run_wait` call internally before sending input. If the check fails, those methods return `-32603` with `"Command blocked by policy: <pattern>"`.

---

## Selftest

### `selftest.run`

Run a battery of internal probes — mux availability, server health, capabilities listing, policy check, admin launch (dry-run on Windows; expected to fail on macOS/Linux), proxy status, window capture, and (if you pass `session_id`) per-pane checks.

**Params:** `session_id` (string, optional) — when present, adds `session.status`, `screen.text`, `screen.detect_errors`, recording-status checks scoped to that pane.

**Returns:** `{ ok: bool, checks: [{ name, ok, detail }, ...] }`

`ok` is true iff every check is true. Each `detail` is the full method response (or an `{ error }` object on failure). The `unterm-cli server selftest` subcommand is a thin wrapper around this method.

```json
{"jsonrpc":"2.0","id":8,"method":"selftest.run","params":{"session_id":"0"}}
```

---

## Method index

Every method, alphabetical, with one-line descriptions. Use this as a flat lookup when you know the name and just want to confirm what it does.

| Method | Purpose |
|---|---|
| `agent.identify` | Self-tag the calling connection for audit grouping |
| `agent.list_trusted` | Runtime, static-config, and per-agent write-count trust snapshot |
| `agent.signal` | Report an agent lifecycle event from an official hook (strongest state signal) |
| `agent.status` | Cockpit agent state per pane (`working`/`waiting`/`done`/`idle`) |
| `agent.trust` | Add an agent to the persistent trust list (writes skip confirmation) |
| `agent.untrust` | Remove an agent from the persistent trust list |
| `agent.whoami` | Read the connection's own identity tag |
| `auth.login` | Authenticate the connection with the token from `~/.unterm/instances/<id>.json` |
| `capture.clipboard` | Read the OS clipboard as text or PNG |
| `capture.screen` | Snapshot every pane's text plus a full-display PNG |
| `capture.scrollback` | Render a pane's entire scrollback into one tall PNG |
| `capture.select` | Falls back to `capture.screen` (interactive selection unavailable in headless mode) |
| `capture.window` | Snapshot one window by title or pid |
| `capture.window_scroll` | Scroll + stitch a long screenshot of another app's window (macOS) |
| `cockpit.inbox` | All tracked agents joined with tab/window location, waiting-first |
| `exec.cancel` | Send Ctrl+C to a pane |
| `exec.run` | Send a command + carriage return; return immediately |
| `exec.run_wait` | Send a command, inject a sentinel, poll until done; return captured output |
| `exec.send` | Alias for `session.input` |
| `exec.status` | Return `"idle"` or `"running"` based on foreground process name |
| `fleet.clean` | Remove a fleet's worktrees, branches, and panes once reviewed |
| `fleet.launch` | One task × N agents × N isolated git worktrees, one tab each |
| `fleet.list` | All fleets with member branches, worktrees, and review states |
| `fleet.retry` | Restart a pending fleet member in its existing worktree without losing changes |
| `ghost.debug` | Read-only ghost-text predictor state for a pane |
| `instance.focus` | Raise one of this instance's windows, by id or whichever is in front |
| `instance.info` | This instance's own metadata, including `auth_token` |
| `instance.list` | Enumerate every live Unterm instance on this machine |
| `instance.new_window` | Open another window on this front end and return its id |
| `instance.set_title` | Pin a custom display title for this instance |
| `instance.windows` | Every window this front end is showing, with its tabs and sessions |
| `meta.surface` | Inventory of MCP methods + CLI subcommands + keybindings |
| `orchestrate.broadcast` | Send the same command to multiple panes |
| `orchestrate.launch` | `session.create` + initial command |
| `orchestrate.wait` | Poll a pane's text for a substring with a timeout |
| `policy.check` | Dry-run a command against the current policy |
| `policy.set` | Replace the command-execution policy |
| `profile.audit` | Secrets expiring within 7 days, without revealing values |
| `profile.current` | The identity profile bound to this Unterm window |
| `profile.list` | List identity profiles (names/counts only, no secret values) |
| `proxy.clash_select` | Point a Clash Selector group at a node via the controller API |
| `proxy.clash_set_controller` | Set/clear a manual Clash controller (host:port + secret) |
| `proxy.clash_status` | Read Clash/mihomo switchable groups + nodes with live delay |
| `proxy.configure` | Write a full proxy config in one call |
| `proxy.disable` | Turn the proxy off |
| `proxy.env` | Resolve proxy state to env-var form |
| `proxy.nodes` | List configured proxy nodes |
| `proxy.rotation` | Get/set endpoint auto-rotation (fail over to the fastest live node) |
| `proxy.set_nodes` | Replace the proxy node list for the rotation pool |
| `proxy.speedtest` | TCP-probe one node or all of them; persist latencies |
| `proxy.status` | Current proxy state plus reachability probe |
| `proxy.switch` | Activate one of the configured nodes by name |
| `review.diff` | Line-level diff of a worktree vs a checkpoint, untracked files included |
| `review.discard` | Mark a fleet member's work as discarded (removed on `fleet.clean`) |
| `review.list` | Review overview: fleets + auto checkpoints per repo |
| `review.merge` | Squash-merge a fleet member into the base repo, leaving it staged (verification-gated, audited write) |
| `review.rollback` | Restore a repo's worktree to a checkpoint (destructive, audited write) |
| `review.verify` | Run an async verification command in a fleet member's worktree (audited) |
| `screen.cursor` | Cursor position and shape |
| `screen.detect_errors` | Hardcoded error-pattern scan over the visible viewport |
| `screen.read` | Visible viewport with absolute row indices |
| `screen.scroll` | Read an absolute slice of scrollback |
| `screen.scrollback_text` | Dump scrollback + viewport as text for LLM hand-off |
| `screen.search` | Substring search across viewport + scrollback; can jump the GUI viewport |
| `screen.text` | Visible viewport as a flat `lines[]` |
| `selftest.run` | Run an internal battery of probes |
| `server.capabilities` | Namespace → method-list map (back-compat; prefer `meta.surface`) |
| `server.health` | Health probe + mux/terminal stats |
| `server.info` | Server name, version, engine, protocol |
| `session.audit_log` | Read the in-memory audit log of mutating calls |
| `session.create` | Spawn a new pane in the active window |
| `session.cwd` | Get the pane's current working directory |
| `session.destroy` | Kill the pane |
| `session.env` | Read launch env variable names where supported; values are redacted |
| `session.export_markdown` | One-off render of pane scrollback to redacted markdown |
| `session.focus` | Bring a pane into focus |
| `session.get` | Full pane state (alias `session.status`) |
| `session.history` | Trailing N lines of scrollback as `entries[]` |
| `session.idle` | True if foreground process looks like a shell |
| `session.input` | Write raw bytes into pane stdin (alias `exec.send`; confirmation-gated) |
| `session.list` | Enumerate every live pane |
| `session.recording_attach_trace` | Associate an external trace id with a live recording |
| `session.recording_list` | Enumerate completed recordings on disk |
| `session.recording_read` | Read one recording's rendered markdown |
| `session.recording_start` | Begin recording a pane to redacted markdown |
| `session.recording_status` | Non-mutating: is this pane being recorded? |
| `session.recording_stop` | Finish, render the markdown, return paths and counts |
| `session.resize` | Resize the pane's pty |
| `session.set_env` | Stub: env-var write not supported in this build |
| `session.split` | Split the current pane left/right/up/down |
| `session.status` | Alias for `session.get` |
| `session.suggest` | Propose text without touching the PTY — user accepts with Tab |
| `session.suggest_cancel` | Withdraw a pending suggestion |
| `session.suggest_list` | List active suggestions across all panes |
| `session.suggest_status` | Lifecycle state of a pending suggestion |
| `signal.send` | Send a control signal as a control character |
| `system.info` | Process and platform metadata |
| `system.launch_admin` | Spawn an elevated Unterm (Windows only) |
| `upload.file` | PUT a local file to configured OSS/COS/Qiniu; return the public URL |
| `workspace.list` | Enumerate saved workspaces with metadata |
| `workspace.restore` | Open new tabs from a saved workspace; supports dry-run planning |
| `workspace.save` | Snapshot the current set of panes |

## Governance: providers, capabilities, evidence

Six namespaces added in v0.68. They exist because a terminal an agent drives
needs answers to questions a terminal has never had to answer before: what
may this agent reach, who said so, where is it allowed to write, and can
somebody who was not here check what happened.

### `provider.*` — capabilities in another process

Unterm as a *consumer*: finding a browser (Unzoo), binding it, and using it
under a lease.

| Method | What it does |
| --- | --- |
| `provider.list` | What can be reached, how each stands, how it was found |
| `provider.bind` | Contact it; the first bind pins who answered |
| `provider.pause` / `resume` / `unbind` | Stop using it; leases go too |
| `provider.diagnose` | Handshake, lease, evidence, idempotency, replay — reported per check |
| `provider.acquire` | Ask for a lease on `browser`, `profile` or `computer` |
| `provider.call` | Do one thing under a lease. Every use needs its own `seq` |
| `provider.leases` / `revoke_lease` | The keys handed out, and taking one back |
| `provider.chain` | Lease → grant → approval → the calls made under it |

Endpoints are **read from what the provider advertised**, never hardcoded —
a provider that is not running must not be confused with whatever process
took the port it had last week. A lease carries an `epoch` and a `seq`: a
repeated sequence number is refused *before* anything is performed, because
a replay noticed afterwards has already done the thing it was replaying.

### `terminal.*` — Unterm as a governable provider

The mirror. `terminal.manifest` publishes who this terminal is and what it
can do, with **risk taken from the same table the gateway refuses by** — not
from `[MUTATION]` tags in tool descriptions, which are prose and drift from
behaviour. `terminal.accept_lease` checks a lease and never issues one: a
provider that could issue its own could authorise itself.

`terminal.invoke` is an envelope, not a second execution path — it checks the
lease and the task context, records the call, and dispatches through the same
handler every other door uses.

### `agent_session.*` — hosting a CLI agent

`start` / `events` / `submit_input` / `interrupt` / `status` / `close`. Events
are `session.started`, `turn.started`, `output.delta`, `tool_call.requested`,
`tool_call.result`, `usage.updated`, `turn.completed`, `error` and
`session.closed`. The `task_id` / `run_id` / `step_id` you pass in come back
on every event untouched — nothing here invents one.

Which parser reads the agent's output is decided by the program behind the
command, seen through `cmd /c`, `env`, `npx`, `node script.js`, `sh -c` and
`pwsh -Command`: Claude Code's `--output-format stream-json`, Codex's
`exec --json`, or `generic` for anything else, which reports each line as
`output.delta` and claims nothing more. Pass `adapter` (`claude`, `codex`,
`generic`) to choose it yourself. `status` reports `usage: null` when the
agent never said what it used, rather than zeros.

Events are buffered behind a cursor (poll, reconnect where you left off); the
*ending* is persisted, so "what happened to the agent I started before the
Core restarted" has an answer.

### `approval.*` — the questions waiting for a person

`approval.list` is readable by anyone. **`approval.decide` refuses every
caller that arrived over the network** — it is answerable only in Unterm's own
Settings. This is a deliberation boundary, not a security one: an agent with a
shell can read the token and run the CLI. What it buys is that an agent asking
for permission has to stop, and a human has to notice.

### `scope.*` — workspaces that cannot see each other

`create` / `list` / `check` / `archive`. A workspace is a named root; every
*other* root is denied explicitly, archived ones included. Symlinks, `..`,
case on a case-folding volume, Windows verbatim and UNC prefixes all resolve
before anything is compared, and a session that `cd`s out stops being inside.

### `artifact.*`, `audit.verify`, `task.*_evidence`, `system.*`

What tasks produced (content-addressed, deduplicated, reference-counted), a
hash-chained audit trail whose edits disagree with the next line, evidence
bundles for somebody who was not there, and the process/snapshot/uninstall
surface. See the [CLI reference](/docs/cli-reference/) for the shell-level
versions.

That's 151 authenticated methods plus `auth.login`. If you find a method in the codebase that isn't listed here, file an issue — the `MCP_METHODS` table in `unterm-agents/src/mcp_meta.rs`, exposed by `meta.surface` and dispatched in `unterm-mcp/src/handler.rs`, is the source of truth and this page should track it.
