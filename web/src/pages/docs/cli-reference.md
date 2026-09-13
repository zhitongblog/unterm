---
layout: ../../layouts/Doc.astro
title: unterm-cli reference
subtitle: Every subcommand and flag of the unterm-cli binary, with cron / CI / pipeline examples. Pipe --json through anywhere downstream that wants raw JSON-RPC.
kicker: Docs / CLI reference
date: 2026-07-20
---

## Connection model

`unterm-cli` is mostly a thin JSON-RPC client. MCP-backed subcommands open a TCP connection to the local Unterm MCP server, complete an `auth.login` handshake, and forward the call. Current builds prefer the headless Core MCP endpoint from `core.json` in the Core's platform data directory (`%LOCALAPPDATA%\Unterm`, `~/.local/share/Unterm`, `~/Library/Application Support/Unterm`); that endpoint owns terminal sessions and survives GUI restarts. A few user-owned settings/discovery commands (`theme`, `lang`, `reference`, and `settings open`) use local config files, compiled-in reference tables, or the GUI HTTP settings server instead.

When `unterm-core` starts it writes `core.json` — in the Core's platform data directory (`%LOCALAPPDATA%\Unterm`, `~/.local/share/Unterm`, `~/Library/Application Support/Unterm`), which is *not* `~/.unterm` — with its MCP port, token, pid, version, protocol, and schema. When a GUI starts it writes `~/.unterm/instances/<name>.json`, updates `~/.unterm/active.json`, and mirrors the active GUI endpoint to `~/.unterm/server.json` for older scripts. The compatibility file has three core fields:

```json
{
  "auth_token": "<uuid>",
  "mcp_port": 19876,
  "http_port": 19877
}
```

By default MCP-backed CLI commands resolve `core.json` first, then the live active GUI instance, then the newest live GUI instance under `~/.unterm/instances/`, then fall back to `server.json`, and finally to the legacy `~/.unterm/auth_token` + port `19876` for old builds. HTTP-backed commands such as `settings open` resolve a live GUI HTTP server first. You can pin a command to one window with `--instance <id>` or `UNTERM_INSTANCE=<id>`.

The token is per-launch — it rotates whenever the GUI restarts, and the files are written `0600` so other users on the host cannot read them.

A few consequences worth knowing before you wire scripts:

- **MCP-backed commands do not require the GUI when Core is running.** No Core or GUI means commands like `session`, `workspace`, `instance`, and `screenshot` will report that Unterm is not running. Settings UI commands still need a GUI HTTP settings server, although settings-style commands can read or write their local fallback files.
- **Everything is local.** Both servers bind `127.0.0.1` only. Nothing on the LAN can reach them. There is no telemetry; the CLI never phones home.
- **The MCP action surface is mirrored in the CLI.** If a method exists on MCP, it's either reachable from the CLI today or trivially exposable. User settings intentionally stay on the settings/config path, not the agent action path.
- **Multi-instance is first-class.** Use `unterm-cli --instance alpha session list` to target a specific window, or omit `--instance` to follow active/latest. This keeps agent scripts deterministic when several Unterm windows are open.

The wire format is line-delimited JSON-RPC 2.0 — one request per line, one response per line. If you ever need to bypass the CLI and talk to MCP directly (Python, Node, curl-with-netcat, whatever), the protocol is implemented and documented in `unterm-mcp/src/server.rs`.

## Global flags

These are accepted on every subcommand because they're declared `global = true` on the top-level `clap` parser:

| Flag | Purpose |
|---|---|
| `--json` | Print machine-readable JSON instead of the human-formatted table. MCP-backed commands return the JSON-RPC `result`; local commands with structured output, such as `setup-ai --dry-run`, return their own stable result object. Pure side-effect commands such as `settings open` ignore it. |
| `--lang <code>` | Force the CLI's interface locale for this single invocation. Does not write to `~/.unterm/lang.json`. Useful in scripts that need stable English output regardless of how the user has configured the GUI. Codes: `en-US`, `zh-CN`, `zh-TW`, `ja-JP`, `ko-KR`, `de-DE`, `fr-FR`, `it-IT`, `hi-IN`. |
| `--instance <id>` | Route MCP-backed commands to a specific live Unterm instance, for example `alpha` or `bravo`. Equivalent to setting `UNTERM_INSTANCE=<id>` for the invocation. |
| `-h`, `--help` | Print help for the current subcommand level. |
| `-V`, `--version` | Print the binary version (matches the GUI build, e.g. `unterm-cli 20260503-201120-8ceb3f23`). |

Historical note: builds before v0.61 also inherited a few flags from the WezTerm-era CLI (`--skip-config`, `--config-file`, `--config name=value`). Those are gone — since v0.61 `unterm-cli` is a standalone binary on the next-core kernel and ships only the flags documented here.

The `--json` flag is the one to remember. Everything below has `--json` examples next to the human ones because that is how you should be driving the CLI from a script.

## start

Launch the Unterm GUI from a script or shell. With no program arguments it starts the normal desktop app; with a trailing program it asks Unterm to start that command instead.

```text
unterm-cli start [--cwd DIR] [--title TITLE] [-- PROGRAM [ARGS...]]
```

## setup-ai

Register or unregister Unterm in local AI coding-agent config files. This writes managed MCP entries that point agents at `unterm-cli mcp-stdio`, plus optional context blocks and cockpit lifecycle hooks.

```text
unterm-cli setup-ai [--dry-run] [--remove] [--no-context] [--no-hooks] [--client ID]
```

Supported client ids are `claude-code`, `codex`, `gemini`, `cursor`, `windsurf`, and `opencode`. By default `setup-ai` detects all of them and updates only config directories that exist.

Use `--json --dry-run` when an installer, smoke test, or release script needs to inspect the planned client updates without modifying the user's agent config files.

## mcp-stdio

Run a stdio MCP bridge for agents that expect MCP over stdin/stdout rather than Unterm's local TCP JSON-RPC socket.

```text
unterm-cli mcp-stdio
```

## profile

Manage identity profiles: GitHub/AWS/npm tokens, git identity, SSH key routing metadata, and the default profile binding used by new panes.

```text
unterm-cli profile ...
```

## Terminal compatibility commands

These top-level families are retained for terminal compatibility and low-level workflows:

```text
unterm-cli ssh
unterm-cli imgcat
unterm-cli ls-fonts
unterm-cli show-keys
unterm-cli set-working-directory
```

`unterm-cli cli`, `connect`, `record`, and `replay` are legacy stubs: they
answer with a machine-readable JSON error pointing at their replacements
(`session`, `instance`, `server`, and `session record`/`session export`)
instead of silently doing nothing.

Use `unterm-cli reference --section cli --filter <name>` or `unterm-cli <name> --help` for the exact flags in the installed build.

## proxy

Read or change the system-wide proxy state. The shape mirrors the GUI's Settings → Proxy panel: there's a global on/off, a `mode` (`auto`/`manual`/`off`), an HTTP and a SOCKS endpoint, an optional list of named "nodes" you can switch between, and a `no_proxy` exclusion list.

```text
unterm-cli proxy status
unterm-cli proxy nodes
unterm-cli proxy switch <NAME>
unterm-cli proxy disable
unterm-cli proxy env
```

### `proxy status`

```sh
$ unterm-cli proxy status
Proxy: ON
Mode:  auto
HTTP:  http://127.0.0.1:7897
SOCKS: socks5://127.0.0.1:7897
Current node: (none)
Node count: 0
No-proxy: 127.0.0.1,192.168.0.0/16,...,localhost,*.local,<local>
```

```sh
$ unterm-cli --json proxy status
{
  "current_node": null,
  "enabled": true,
  "health": {
    "hint": "",
    "reachable": true,
    "source": "manual",
    "url": "http://127.0.0.1:7897"
  },
  "http_proxy": "http://127.0.0.1:7897",
  "mode": "auto",
  "no_proxy": "127.0.0.1,...,<local>",
  "node_count": 0,
  "socks_proxy": "socks5://127.0.0.1:7897"
}
```

The `health` block is what the GUI uses to render the green/red dot in the proxy chip — `reachable: false` means the upstream proxy didn't answer the last heartbeat. Take it as a hint, not a guarantee; the heartbeat is cheap and async.

### `proxy nodes`

Lists named nodes from your proxy config. The active node, if any, is marked with `*`.

```sh
$ unterm-cli proxy nodes
   NAME                     URL
*  cn-shanghai              http://127.0.0.1:7897
   us-east-tunnel           http://10.0.0.5:8118
```

When no nodes are configured the human formatter prints `(no proxy nodes configured)`. The JSON form returns an empty `nodes` array — easier to feed to `jq`.

### `proxy switch <NAME>`

Sets the current node. The argument is the node `name` (not the URL). The MCP server actually accepts `node_name` on the wire; the CLI translates the surface for you. Example:

```sh
$ unterm-cli proxy switch us-east-tunnel
Switched: true
Current node: us-east-tunnel
HTTP: http://10.0.0.5:8118
```

Switching also flips `enabled = true`. If you've previously `disable`d the proxy, `switch` is the easy way back on.

### `proxy disable`

Hard off. Sets `mode = off`, clears `enabled`. Re-enabling without losing your config is what `proxy switch` is for, since `disable` doesn't drop the node list — it just deactivates the global flag.

```sh
$ unterm-cli proxy disable
Proxy disabled.
```

### `proxy env`

Emits `export` lines for the current proxy as POSIX shell. If the proxy is off, prints a comment instead of fake env vars.

```sh
$ unterm-cli proxy env
export ALL_PROXY=socks5://127.0.0.1:7897
export HTTPS_PROXY=http://127.0.0.1:7897
export HTTP_PROXY=http://127.0.0.1:7897
export NO_PROXY='127.0.0.1,...,<local>'
```

The output is shell-quoted: values that contain anything outside `[A-Za-z0-9:/.,_=-]` get wrapped in single quotes, with embedded `'` escaped. Safe to `eval`.

**Real-world use case** — drop this in your `~/.zshrc` or a project `direnv` `.envrc` so any new shell inherits whatever proxy Unterm is currently using:

```sh
# ~/.zshrc
if command -v unterm-cli >/dev/null 2>&1; then
  eval "$(unterm-cli proxy env 2>/dev/null)"
fi
```

When you flip proxies in Unterm's GUI, you don't have to restart shells — open a new tab and the new shell picks it up. For long-lived shells that need re-sync, a simple alias `alias rsp='eval "$(unterm-cli proxy env)"'` is enough.

## session

> **Which pane: `--pane-id`.** Every command that takes a pane takes it under
> this one name — `session`, `exec`, `scrollback`, `screenshot`, `agent`. It
> used to differ per command (`--id` here, `--pane-id` there, `--pane`
> elsewhere), so the only spelling that worked everywhere was the least
> descriptive one. The older spellings are still accepted as aliases, so
> scripts written against them keep working.

Operates on a single live pane. "Session" here means one terminal tab/pane in the running GUI. The CLI wraps the MCP methods `session.list`, `session.create`, `session.input`, `screen.text`, `session.cwd`, `exec.status`, `screen.detect_errors`, `session.history`, `screen.search`, `session.suggest*`, `session.recording_start/stop/status`, and `session.export_markdown`.

```text
unterm-cli session list
unterm-cli session create [--cwd DIR] [--profile NAME] [-- COMMAND]
unterm-cli session split  [--pane-id <ID>] [--direction right|left|down|up] [--size-percent N] [--cwd DIR]
unterm-cli session focus  [--pane-id <ID>]
unterm-cli session resize [--pane-id <ID>] --cols N --rows N
unterm-cli session destroy --pane-id <ID>
unterm-cli session record start [--pane-id <ID>]
unterm-cli session record stop  [--pane-id <ID>]
unterm-cli session record status [--pane-id <ID>]
unterm-cli session export       [--pane-id <ID>] [-o FILE]
unterm-cli session input        [--pane-id <ID>] [--stdin] [--enter] <TEXT...>
unterm-cli session text         [--pane-id <ID>]
unterm-cli session cwd          [--pane-id <ID>]
unterm-cli session status       [--pane-id <ID>]
unterm-cli session errors       [--pane-id <ID>]
unterm-cli session history      [--pane-id <ID>] [--limit N]
unterm-cli session audit-log    [--pane-id <ID>] [--limit N]
unterm-cli session search       [--pane-id <ID>] [--max-results N] [--goto|--goto-match N] <PATTERN...>
unterm-cli session suggest post [--pane-id <ID>] [--rationale TEXT] [--ttl-ms N] <TEXT...>
unterm-cli session suggest status <SUGGESTION_ID>
unterm-cli session suggest cancel <SUGGESTION_ID>
unterm-cli session suggest list [--pane-id <ID>]
```

When `--pane-id` is omitted on any pane-scoped subcommand, the CLI auto-resolves it to the first pane returned by `session.list`. Convenient if you only have one tab open; brittle if you have several. Pass `--pane-id` explicitly in scripts.

### `session list`

```sh
$ unterm-cli session list
ID    COLS   ROWS   SHELL      TITLE
0     191    77     unknown    ✳ Claude Code
2     171    77     unknown    ⠂ Check current project progress
```

```sh
$ unterm-cli --json session list
{
  "sessions": [
    {
      "cols": 191, "rows": 77,
      "cursor": { "visible": false, "x": 0, "y": 7812 },
      "domain_id": 0, "id": 0, "is_dead": false,
      "shell": {
        "cwd": null,
        "process_name": "/Users/alexlee/.local/share/claude/versions/2.1.126",
        "shell_type": "unknown"
      },
      "title": "✳ Claude Code"
    }
  ]
}
```

The IDs are stable for the lifetime of a pane and monotonically increasing. They are *not* reused after a pane closes, so a script that snapshots IDs once and replays them later is safe — at worst you'll get "Session 7 not found" rather than acting on the wrong pane.

### `session create`

Creates a new tab in the target instance. With no arguments it opens the default shell. `--cwd` sets the starting directory, `--profile` overlays an identity profile's environment for this tab, and a trailing command runs through the platform shell.

```sh
$ unterm-cli session create --cwd /tmp -- 'printf "hello from unterm\n"; exec zsh'
Pane: 12
Title: zsh
```

```sh
$ unterm-cli --instance bravo --json session create --profile work -- 'gh auth status; exec zsh'
{
  "command": "gh auth status; exec zsh",
  "cols": 217,
  "id": 12,
  "profile": "work",
  "rows": 60,
  "session_id": "12",
  "title": "zsh"
}
```

### `session split` / `focus` / `resize` / `destroy`

Pane lifecycle helpers over MCP `session.split`, `session.focus`, `session.resize`, and `session.destroy`.

```sh
$ unterm-cli session split --pane-id 0 --direction right --size-percent 40 --cwd /tmp
Pane:      13
Title:     zsh
Direction: right
```

```sh
$ unterm-cli session focus --pane-id 13
true
```

```sh
$ unterm-cli session resize --pane-id 13 --cols 120 --rows 40
ok
```

`destroy` requires an explicit id so a script cannot accidentally close the first pane by omission:

```sh
$ unterm-cli session destroy --pane-id 13
true
```

### `session record start`

Begins a redacted markdown recording of a pane. Returns a UUID (`session_id`) and the on-disk paths the recording will land at.

```sh
$ unterm-cli session record start --pane-id 0
Session id: 8dee59d3-0e21-4ebf-a8cf-a2c356b53b70
Log path: /Users/alexlee/.unterm/sessions/_orphan/2026-05-03/tab-0-221510.log
Markdown (on stop): /Users/alexlee/.unterm/sessions/_orphan/2026-05-03/tab-0-221510.md
```

If the pane has a project cwd, recordings land under `<cwd>/.unterm/sessions/<date>/` instead of `~/.unterm/sessions/_orphan/<date>/`. The fallback is what you get when the pane has no detectable project root.

### `session record stop` / `record status`

`stop` finalises the markdown (no further blocks captured), prints summary stats, and is idempotent — calling it on a non-recording pane prints a benign "not recording" message rather than failing.

```sh
$ unterm-cli session record stop --pane-id 0
Session id: 8dee59d3-0e21-4ebf-a8cf-a2c356b53b70
Block count: 0
Markdown: /Users/alexlee/.unterm/sessions/_orphan/2026-05-03/tab-0-221510.md
Exit reason: recording_stopped
```

`status` is read-only and useful for "was I already recording?" guards in scripts:

```sh
$ unterm-cli --json session record status --pane-id 0
{ "enabled": false }
```

### `session export`

Snapshots a pane's accumulated block log to markdown without stopping recording (or even requiring recording to be active — the block buffer is always populated when OSC 133 is in play). Two flag modes:

- No `-o`: MCP picks the destination, the path is printed.
- `-o FILE`: the CLI passes the path through to MCP, and additionally copies the file to `FILE` on the local filesystem if MCP wrote elsewhere. End state: `FILE` always exists at the path you asked for.

```sh
$ unterm-cli session export --pane-id 0 -o /tmp/snapshot.md
/tmp/snapshot.md
```

**Real-world use case** — a "git pre-push" hook that exports the last 100 blocks of your build pane and attaches them to the commit message:

```sh
# .git/hooks/pre-push
PANE_ID=$(unterm-cli --json session list | jq '.sessions[] | select(.title|test("build|ci")) | .id' | head -1)
[ -z "$PANE_ID" ] && exit 0
unterm-cli session export --pane-id "$PANE_ID" -o ".git/last-build.md"
```

### `session input` / `session text`

`session input` writes text through MCP `session.input`. It does not append a newline unless you pass `--enter`, which appends carriage return to match a real Enter keypress.

```sh
$ unterm-cli session input --pane-id 0 --enter 'cargo test -p unterm-cli'
ok
```

Use `--stdin` when piping generated input:

```sh
$ printf 'echo from stdin' | unterm-cli session input --pane-id 0 --stdin --enter
ok
```

`session text` reads the visible viewport through MCP `screen.text`:

```sh
$ unterm-cli session text --pane-id 0
```

### `session cwd` / `status` / `errors` / `history` / `audit-log` / `search`

These are read-only probes for scripts and outer agents.

```sh
$ unterm-cli session cwd --pane-id 0
/Volumes/Dev/code/unterm
```

```sh
$ unterm-cli session status --pane-id 0
Status:     idle
Foreground: /bin/zsh
```

```sh
$ unterm-cli session errors --pane-id 0
ROW      PATTERN            TEXT
18291    error:             error: could not compile `unterm-cli`
```

```sh
$ unterm-cli session history --pane-id 0 --limit 50
cargo test -p unterm-cli
test result: ok. 4 passed; 0 failed
```

`history` reads recent non-empty pane scrollback lines through MCP `session.history`; it is not shell history from `~/.zsh_history` or equivalent.

```sh
$ unterm-cli session audit-log --limit 20
TIME                      METHOD                 PANE     AGENT      DETAIL
2026-06-19T09:50:00+08:00 exec.run               7        codex      cargo test
```

`audit-log` reads recent mutating MCP/CLI actions from `session.audit_log`; pass `--pane-id` to filter to one pane.

```sh
$ unterm-cli session search --pane-id 0 --max-results 5 error:
ROW      COL    TEXT
18291    0      error: could not compile `unterm-cli`
```

`search` scans pane scrollback through MCP `screen.search`. Add `--goto` to move the GUI viewport to the first match, or `--goto-match N` to jump to a specific match index.

Use `--json` for the raw MCP payloads when you need stable fields such as `cwd`, `status`, `foreground_process`, `has_errors`, `errors[]`, `entries[]`, audit fields, or `matches[]`.

### `session suggest`

Queues non-PTY suggestions through MCP `session.suggest`. The CLI does not type the text into the shell; the user accepts or dismisses it in the terminal UI.

```sh
$ unterm-cli session suggest post --pane-id 0 --rationale "next diagnostic step" -- cargo test -p unterm-cli
Suggestion: sg_1781805012345_1
Status:     queued
```

```sh
$ unterm-cli session suggest list --pane-id 0
SUGGESTION               PANE     AGENT      TEXT
sg_1781805012345_1       0        codex      cargo test -p unterm-cli
```

Use `status` to inspect a suggestion payload and `cancel` to withdraw a pending suggestion.

## exec

Run commands in a live pane through the MCP `exec.*` methods. This is the command-oriented layer above `session input`.

```text
unterm-cli exec run    [--pane-id <ID>] -- <COMMAND...>
unterm-cli exec wait   [--pane-id <ID>] [--timeout-ms N] -- <COMMAND...>
unterm-cli exec status [--pane-id <ID>]
unterm-cli exec cancel [--pane-id <ID>]
unterm-cli exec signal [--pane-id <ID>] SIGINT|SIGTSTP|SIGQUIT|EOF
```

`run` sends the command and returns immediately:

```sh
$ unterm-cli exec run --pane-id 0 -- cargo test -p unterm-cli
true
```

`wait` wraps the command with Unterm's shell-specific sentinel and prints the captured output when the sentinel appears:

```sh
$ unterm-cli exec wait --pane-id 0 --timeout-ms 60000 -- cargo test -p unterm-cli
```

`status` and `cancel` are pane-scoped probes/actions:

```sh
$ unterm-cli exec status --pane-id 0
Status:     running
Foreground: cargo

$ unterm-cli exec cancel --pane-id 0
true
```

`signal` exposes MCP `signal.send` directly for the other terminal control characters:

```sh
$ unterm-cli exec signal --pane-id 0 SIGTSTP
true
```

Supported values are `SIGINT`/`INT`, `SIGTSTP`/`TSTP`, `SIGQUIT`/`QUIT`, and `EOF`. These are written as control characters to the PTY; they are not POSIX `kill(2)` signals.

Use `--json` for the raw result fields: `{ sent }`, `{ output, exit_status, timed_out, marker }`, `{ status, foreground_process }`, `{ cancelled }`, or `{ sent, signal }`.

## sessions

Browse the persistent recording archive on disk (the markdown files written by `session record stop` and friends). The MCP-side methods are `session.recording_list` and `session.recording_read`.

```text
unterm-cli sessions list [--project <SLUG>]
unterm-cli sessions read <SESSION_ID>
```

### `sessions list`

```sh
$ unterm-cli sessions list
SESSION_ID                             BLOCKS STARTED                          PROJECT
95397675-cb95-4116-9c8e-64a0c32ce927   1      2026-04-30T22:03:26.889127+08:00 alexlee
0ec12e9d-a9ae-43f6-a654-4b484789727e   1      2026-05-01T09:40:29.205705+08:00 unterm
8ae4a612-08e2-4ba5-af1f-d815c80abdd2   1      2026-05-01T09:54:38.014989+08:00 unterm
```

Filter to a project:

```sh
$ unterm-cli sessions list --project unterm
```

The "project slug" is the basename of the directory the recording originated from (or `_orphan` for recordings without a detectable project). It's matched as an exact string, not a glob.

JSON form returns an array (not an object with a `sessions` key — note the asymmetry vs `session.list`):

```sh
$ unterm-cli --json sessions list | jq '.[0]'
{
  "block_count": 1,
  "bytes_raw": 4136,
  "ended_at": "2026-04-30T14:03:29.296032+00:00",
  "log_path": "/Users/alexlee/.unterm/sessions/alexlee/2026-04-30/tab-0-220326.log",
  "md_path": "/Users/alexlee/.unterm/sessions/alexlee/2026-04-30/tab-0-220326.md",
  "project_path": "/Users/alexlee/",
  "project_slug": "alexlee",
  "started_at": "2026-04-30T22:03:26.889127+08:00",
  "tab_id": 0,
  "unterm_session_id": "95397675-cb95-4116-9c8e-64a0c32ce927"
}
```

### `sessions read <SESSION_ID>`

Streams the recorded markdown to stdout. The argument is the UUID from `sessions list`, not the path. Pipe it however you like.

```sh
$ unterm-cli sessions read 95397675-cb95-4116-9c8e-64a0c32ce927 | head
---
unterm_session_id: 95397675-cb95-4116-9c8e-64a0c32ce927
tab_id: 0
project_path: /Users/alexlee/
project_slug: alexlee
shell: /bin/sh
hostname: 192.168.5.7
unterm_version: 20260502-121851-b3680e89
started_at: 2026-04-30T22:03:26.889127+08:00
ended_at: 2026-04-30T14:03:29.296032+00:00
```

The frontmatter is YAML; the body is fenced markdown blocks, one per OSC 133 prompt. Tokens, GitHub PATs, AWS keys, and 40+ char base64/hex strings are masked at recording time.

**Real-world use case** — feed a recent recording to a model for review:

```sh
LAST=$(unterm-cli --json sessions list --project unterm | jq -r '.[-1].unterm_session_id')
unterm-cli sessions read "$LAST" | claude -p "summarise what I did in this session"
```

## workspace

Save and restore named pane workspaces. This wraps `workspace.save`, `workspace.restore`, and `workspace.list` on MCP.

```text
unterm-cli workspace list
unterm-cli workspace save <NAME>
unterm-cli workspace restore <NAME> [--dry-run]
```

`workspace save` snapshots the current panes' titles and working directories into `~/.unterm/workspaces/<NAME>.json`. `workspace restore` opens new tabs for the saved working directories; it does not close or replace the panes you already have open.

`workspace list --json` includes `path`, `saved_at`, and `session_count` for each saved workspace, so scripts can pick the newest or largest workspace without reading the JSON files themselves.

Use `--dry-run` before restoring a large workspace:

```sh
$ unterm-cli --json workspace restore release-train --dry-run | jq '.planned[].cwd'
```

## instance

List, inspect, label, or focus live Unterm windows. This is the companion to the global `--instance <id>` flag: first discover the instance id, then route future commands to that window.

```text
unterm-cli instance list
unterm-cli instance info
unterm-cli instance set-title "release train"
unterm-cli instance set-title --clear
unterm-cli instance focus
```

`instance list` reads the live instance registry through MCP and omits auth tokens from the table. Use `--json` when a script needs to pick by cwd, title, pid, or port:

```sh
$ unterm-cli --json instance list | jq '.instances[] | {id,title,cwd,pid}'
```

Once you know the id, target that window explicitly:

```sh
$ unterm-cli --instance bravo session create --cwd ~/src/app -- 'cargo test; exec zsh'
```

## agent

Install, authenticate, configure, launch, or run AI coding-agent CLIs through Unterm's profile and MCP wiring. `agent launch` opens the vendor CLI interactively; `agent run` uses the vendor's non-interactive mode and waits for the task to finish. Since v0.55 this family also carries the Agent Cockpit state surface: `status`, `inbox`, `signal`, and `enable-hooks`.

```text
unterm-cli agent run <codex-cli|claude-code|gemini-cli|opencode> [--profile <id>] [--cwd <path>] [--stdin] [--dry-run] <prompt...>
unterm-cli agent status [--pane-id <ID>]
unterm-cli agent inbox
unterm-cli agent signal --event <working|waiting|done|idle> [--agent <name>] [--pane-id <ID>]
unterm-cli agent enable-hooks [--dry-run] [--remove]
unterm-cli agent whoami
unterm-cli agent trusted
unterm-cli agent trust <agent-name>
unterm-cli agent untrust <agent-name>
```

(The family also includes lifecycle management — `list`, `show`, `install`, `update`, `uninstall`, `auth`, `configure`, `import`, `plan`, `launch`, `manifest` — see `unterm-cli agent --help`; the GUI equivalent is Web Settings → AI Agents.)

`agent run` currently supports:

| Agent | Headless command used by Unterm |
|---|---|
| `codex-cli` | `codex exec <prompt>` |
| `claude-code` | `claude -p <prompt>` |
| `gemini-cli` | `gemini -p <prompt>` |
| `opencode` | `opencode run <prompt>` |

The command reuses the same auth mode and settings as `agent launch`. If the agent is signed in with its official subscription flow in Web Settings -> AI Agents, Unterm does not inject an API key or switch it to per-token billing.

| Flag | Purpose |
|---|---|
| `--profile <id>` | Run with a specific Unterm identity profile. |
| `--cwd <path>` | Run from a specific project directory. |
| `--stdin` | Read additional prompt text from stdin; useful for diffs, logs, and exported sessions. |
| `--dry-run` | Print the command and redacted environment instead of starting the agent. |

```sh
$ unterm-cli agent run codex-cli --cwd ~/src/app "review this diff and list risky changes"
```

```sh
$ unterm-cli agent run claude-code --profile work "summarise the last failing test output"
```

```sh
$ unterm-cli agent run gemini-cli --cwd ~/src/app "summarise this repository and suggest the next useful task"
```

```sh
$ unterm-cli agent run opencode --profile work "inspect the current project and suggest the next useful task"
```

```sh
$ git diff | unterm-cli agent run codex-cli --stdin "review this diff"
```

For automation, combine `--json` with `--dry-run` to get a structured launch preview with `agent`, `profile`, `cwd`, `exec`, `args`, redacted `env_set`, and `prompt_chars`:

```sh
$ unterm-cli --json agent run claude-code --dry-run "summarise this repo"
```

The trust commands wrap the MCP governance methods `agent.whoami`, `agent.list_trusted`, `agent.trust`, and `agent.untrust`. Use them to inspect or change which local MCP agent names can write to panes without a confirmation banner:

```sh
$ unterm-cli agent trusted
Runtime:       codex
Static config: -

AGENT                    WRITES
codex                    42
```

### `agent status` / `agent inbox`

The Agent Cockpit's read surface. `status` reports every pane with a detected agent, its state — `working`, `waiting`, `idle`, or `done` — how long it has been there, and a task hint. `inbox` is the same data filtered to agents that want attention, waiting-first, with tab/window locations so you (or an outer agent) can jump straight to the pane.

```sh
$ unterm-cli agent status
PANE       AGENT     STATE      FOR     TASK
3      ✋   claude    waiting    4400s   socal
```

```sh
$ unterm-cli --json agent status
{
  "agents": [
    {
      "agent": "claude",
      "fleet_id": null,
      "for_secs": 4400,
      "last_signal": "hook",
      "pane_id": 3,
      "state": "waiting",
      "task_hint": "socal"
    }
  ],
  "enabled": true
}
```

`--pane-id <ID>` narrows `status` to one pane. `last_signal` tells you which detection layer produced the verdict — `hook` means exact reporting from `enable-hooks`; without hooks the engine falls back to OSC and process-poll heuristics. The JSON form of `inbox` adds `pane_title`, `tab_id`, and `window_id` per item. MCP methods: `agent.status` and `cockpit.inbox`.

### `agent signal`

The write half: report an agent lifecycle event into the cockpit state engine. This is what the hooks installed by `enable-hooks` call; you can also call it from your own wrappers or CI glue.

```sh
$ unterm-cli agent signal --agent claude --event done
```

| Flag | Purpose |
|---|---|
| `--event <e>` | Required. One of `working`, `waiting`, `done`, `idle`. |
| `--agent <name>` | Agent name (`claude`, `codex`, `gemini`, `aider`, …). |
| `--pane-id <ID>` | Pane to attribute the event to. Defaults to `$WEZTERM_PANE`, which hook processes inherit from the shell Unterm spawned — so a hook running inside a pane attributes itself correctly with no flags. |

When no Unterm is running, `signal` exits quietly instead of failing — a hook must never break the agent it reports on. That makes it the one deliberate exception to the "all failures exit 1" rule at the bottom of this page.

### `agent enable-hooks`

Wires official lifecycle hooks into the global configs of the agents installed on this machine, so cockpit state is exact rather than inferred:

| Agent | File | What gets written |
|---|---|---|
| Claude Code | `~/.claude/settings.json` | `hooks` entries — `UserPromptSubmit` → `working`, `Notification` → `waiting`, `Stop` → `done` — each running `"<unterm-cli>" agent signal --agent claude --event <event>` with a 5s timeout. |
| Codex | `~/.codex/config.toml` | A `notify` command that fires `agent signal --agent codex --event done` on turn-complete, plus `[tui] notifications = true` so approval prompts reach the OSC layer for waiting detection. |
| Aider | `~/.aider.conf.yml` | `notifications: true` and `notifications-command: "<unterm-cli>" agent signal --agent aider --event waiting`, under a `# managed by …` marker comment. |

Only agents actually present are touched (`~/.claude` exists, `~/.codex` exists, `~/.aider.conf.yml` exists or `aider` is on `PATH`). Writes are merge-only: your existing settings survive, and a conflicting key — say, a Codex `notify` command of your own — makes that file report `skipped(…)` rather than being overwritten. The first write to each file leaves a `<file>.unterm-bak` backup next to it.

| Flag | Purpose |
|---|---|
| `--dry-run` | Print the per-file `written` / `unchanged` / `skipped(…)` actions without writing anything. |
| `--remove` | Remove exactly the hooks a previous run wrote, leaving the rest of each file alone. |

## fleet

Run one task across N agents in N isolated git worktrees — the Agent Cockpit's parallel-attempts primitive. `fleet launch` verifies the repo is clean, creates one branch and worktree per member, and opens one tab per member with the agent already running on the task. Backed by MCP `fleet.launch`, `fleet.list`, `fleet.clean`, and `fleet.retry`.

```text
unterm-cli fleet launch --agents <a,b,...> [--cwd DIR] <TASK...>
unterm-cli fleet list
unterm-cli fleet clean <ID> [--force]
unterm-cli fleet retry  --fleet <ID> --member <N|BRANCH>
```

### `fleet launch`

```sh
$ unterm-cli fleet launch --agents claude,claude,codex --cwd ~/src/app "fix the login redirect loop"
fleet fix-the-login-redirect-loop launched — 3 member(s)
  claude    fleet/fix-the-login-redirect-loop-1  /Users/alexlee/src/app.fleet/fix-the-login-redirect-loop-1
  claude    fleet/fix-the-login-redirect-loop-2  /Users/alexlee/src/app.fleet/fix-the-login-redirect-loop-2
  codex     fleet/fix-the-login-redirect-loop-3  /Users/alexlee/src/app.fleet/fix-the-login-redirect-loop-3
```

| Flag | Purpose |
|---|---|
| `--agents <a,b,...>` | Required. Comma-separated member agents (`claude`, `codex`, `gemini`, `aider` — whatever is on `PATH`). Repeats are fine: `claude,claude,claude` races three Claudes on the same prompt. Max 8 members. |
| `--cwd <DIR>` | Repo path; defaults to the active pane's cwd. Must be inside a git repository with a clean worktree — a dirty repo fails with `worktree not clean — commit or stash before launching a fleet`. |

The fleet id is a slug of the task. Member *n* gets branch `fleet/<id>-<n>` and worktree `<repo-parent>/<repo>.fleet/<id>-<n>`, both created from `HEAD` — that starting sha is the member's review baseline. Fleets persist in `~/.unterm/fleets.json`, so they survive a GUI restart; closing a member's pane does not remove the fleet, because the worktrees hold the actual work.

### `fleet list`

```sh
$ unterm-cli fleet list
fix-the-login-redirect-loop  (master)  task: fix the login redirect loop
  #1 claude    working    review=pending   fleet/fix-the-login-redirect-loop-1
  #2 claude    done       review=pending   fleet/fix-the-login-redirect-loop-2
  #3 codex     done       review=merged    fleet/fix-the-login-redirect-loop-3
```

Each member row shows the live cockpit state (`working`/`waiting`/`done`/`idle`) and the review state (`pending`/`merged`/`discarded`). `--json` adds worktree paths, checkpoint shas, and pane ids per member.

### `fleet clean <ID>`

Kills surviving member panes, removes the worktrees and `fleet/…` branches, and drops the record. Refuses while any member is still `pending` review — merge or discard each member first (`review merge` / `review discard`), or pass `--force` to throw un-reviewed work away.

```sh
$ unterm-cli fleet clean fix-the-login-redirect-loop
fleet fix-the-login-redirect-loop cleaned
```

### `fleet retry`

Restarts a `pending` member's agent in its **existing** worktree. The branch, checkpoint, and every committed *and* uncommitted change are kept — only the pane is replaced. The old pane is closed before the new agent starts, so two processes never edit the same worktree concurrently. Each retry bumps the member's `attempt` counter (visible in `fleet list --json`).

```sh
$ unterm-cli fleet retry --fleet fix-the-login-redirect-loop --member 2
retried member 2 of fix-the-login-redirect-loop (attempt 2)
```

| Flag | Purpose |
|---|---|
| `--fleet <ID>` | Required. Fleet id from `fleet list`. |
| `--member <N\|BRANCH>` | Required. Member index (1-based) or branch name. |

A member that is already `merged` or `discarded` cannot be retried, and the retry refuses if the worktree no longer exists or has been switched off its `fleet/…` branch. If spawning the new pane fails, the error is recorded on the member (`last_launch_error` in `--json`) and the member simply has no active pane — retry again once the cause is fixed.

## review

Inspect, merge, discard, or roll back agent-produced changes. Two kinds of things live here: fleet members (from `fleet launch`) and **auto checkpoints** — whenever an agent starts working in a non-fleet pane, the cockpit snapshots that repo as a dangling commit object, debounced to one per minute per repo. The snapshot uses a temporary index, so your working tree and staging area are never touched; the checkpoint index lives at `~/.unterm/checkpoints.json`, and the whole mechanism is gated by the `cockpit_auto_checkpoint` config option. Backed by MCP `review.list`, `review.diff`, `review.verify`, `review.merge`, `review.discard`, and `review.rollback`.

```text
unterm-cli review list
unterm-cli review diff     (--fleet <ID> --member <N|BRANCH>) | (--repo <PATH> --from <SHA>) [--stat]
unterm-cli review verify   --fleet <ID> --member <N|BRANCH> [--command CMD] [--timeout-secs N]
unterm-cli review merge    --fleet <ID> --member <N|BRANCH> [--force]
unterm-cli review discard  --fleet <ID> --member <N|BRANCH>
unterm-cli review rollback --repo <PATH> --sha <SHA> --yes
unterm-cli review open
```

### `review list`

```sh
$ unterm-cli review list
fleets: 1
  fix-the-login-redirect-loop  task: fix the login redirect loop
checkpoints in /Users/alexlee/src/notebook (20):
  9da648553665  2026-07-20T13:56:48.941578+00:00  by claude
  8346ebc23ec4  2026-07-20T13:41:00.530701+00:00  by claude
```

Human output shows the five newest checkpoints per repo; `--json` returns all of them with full shas and the pane that triggered each one.

### `review diff`

Line-level diff, in two addressing modes: a fleet member against its baseline (`--fleet` + `--member`, where the member is a 1-based index or a branch name), or a repo against one of its checkpoints (`--repo` + `--from <sha>`). `--stat` prints only the per-file summary, not the patch; new untracked files are marked `(new)`.

```sh
$ unterm-cli review diff --fleet fix-the-login-redirect-loop --member 2 --stat
  +42    -7     src/auth/session.rs
  +3     -0     tests/login.rs (new)
```

### `review verify`

Queues tests/validation for a fleet member, run **asynchronously** inside the member's isolated worktree. Without `--command`, the command is inferred from a small, auditable set of project markers; inference only reads marker files, never executes scripts discovered in the source tree. If nothing matches, the CLI errors and asks for an explicit `--command`.

| Marker | Inferred command |
|---|---|
| `Cargo.toml` | `cargo test` |
| `go.mod` | `go test ./...` |
| `pnpm-lock.yaml` | `pnpm test` |
| `yarn.lock` | `yarn test` |
| `package-lock.json` / `package.json` | `npm test` |
| `uv.lock` | `uv run pytest` |
| `pyproject.toml` / `pytest.ini` / `setup.cfg` | `python -m pytest` |
| `pom.xml` | `mvn test` |
| `gradlew` | `./gradlew test` (`gradlew.bat test` on Windows) |
| `*.sln` / `*.csproj` | `dotnet test` |

The npm/pnpm/yarn rows additionally require a non-empty `test` script in `package.json` — a placeholder-only project infers nothing rather than running a script that exits 1.

```sh
$ unterm-cli review verify --fleet fix-the-login-redirect-loop --member 2
verification verify-1753100000000-0 queued: cargo test
```

| Flag | Purpose |
|---|---|
| `--fleet <ID>` | Required. |
| `--member <N\|BRANCH>` | Required. Member index (1-based) or branch name. |
| `--command <CMD>` | Explicit verification command; takes precedence over inference. |
| `--timeout-secs <N>` | Deadline in seconds. Default 900 (15 min), clamped to a 2-hour maximum; on timeout the whole process tree is killed and the run is marked `timed_out`. |

Each run persists to `~/.unterm/verifications.json`: status (`pending` → `running` → `passed`/`failed`/`timed_out`), exit code, duration, and the last 64 KiB of combined stdout+stderr. `review list --json` attaches each member's latest verification plus a deterministic `score` and `rank` — a passed verification dominates the score, smaller diffs break ties — so `rank == 1` is the merge candidate.

### `review merge` / `review discard`

`merge` squash-merges a member's branch into the base repo and leaves the result **staged, uncommitted** — you own the commit message. The base repo must be clean first. **Verification gate:** a normal merge requires the member's *latest* verification to be `passed`; an unverified member, or one whose latest run failed or timed out, is refused with a pointer to `review verify`. `--force` overrides the gate — the override is audited, and the JSON result carries `"verification_forced": true` alongside whatever verification record existed. `discard` marks the member reviewed-and-rejected; its branch and worktree stay on disk until `fleet clean`.

```sh
$ unterm-cli review merge --fleet fix-the-login-redirect-loop --member 2
merged fleet/fix-the-login-redirect-loop-2 — staged in /Users/alexlee/src/app (commit it yourself)
```

| Flag | Purpose |
|---|---|
| `--fleet <ID>` | Required. |
| `--member <N\|BRANCH>` | Required. Member index (1-based) or branch name. |
| `--force` | `merge` only. Override the passed-verification gate. This action is audited. |

### `review rollback`

Destructive: restores a repo's worktree to a checkpoint — tracked files *and* untracked state become what they were at the checkpoint, discarding everything newer. `--yes` is required; without it the CLI refuses with an explanatory error instead of prompting.

```sh
$ unterm-cli review rollback --repo ~/src/app --sha 9da648553665 --yes
rolled back /Users/alexlee/src/app to 9da648553665
```

### `review open`

Opens the GUI Review page (`http://127.0.0.1:<http_port>#review`) in your default browser. Like `settings open`, this resolves the endpoint locally rather than round-tripping through MCP.

## settings

Open the Web Settings UI in your default browser. This subcommand does *not* hit MCP. It resolves the live GUI HTTP endpoint from `active.json` / `instances/` / `server.json`, then shells out to `open` / `xdg-open` / `cmd /C start`.

```text
unterm-cli settings open [--section <name>] [--print-only]
```

| Flag | Purpose |
|---|---|
| `--section <name>` | Open directly to a Web Settings section: `general`, `profiles`, `agents`, `mcp`, `appearance`, `proxy`, `scrollback`, `compat`, `recording`, `project`, `reference`, or `about`. |
| `--print-only` | Print the URL and exit; don't launch a browser. |

```sh
$ unterm-cli settings open --print-only
http://127.0.0.1:19877
```

```sh
$ unterm-cli settings open --section agents --print-only
http://127.0.0.1:19877#agents
```

```sh
$ unterm-cli settings open --section recording
http://127.0.0.1:19877#recording
# (browser tab opens)
```

The URL also points at a static SPA at `/`, plus a small REST surface for the same operations as MCP — handy if you want to drive Unterm from JavaScript in a browser without speaking JSON-RPC.

## screenshot

Capture the screen and save the PNG. Backed by the `capture.*` MCP methods.

```text
unterm-cli screenshot [--include-window] [--base64] [-o FILE]
unterm-cli screenshot --scrollback [--pane-id N] [--max-rows N] [--dpi N] [-o FILE]
unterm-cli screenshot --scroll-app APP [--scroll-title TEXT] [--scroll-pid PID] [--max-frames N] [-o FILE]
```

| Flag | Purpose |
|---|---|
| `--include-window` | Include Unterm's own window in the capture. Default: pass `include_window=false` to MCP, which the GUI honours best-effort (`screencapture` cannot literally exclude one window, so on macOS this currently maps to "capture full screen anyway" — treat the flag as a hint). |
| `--self` | Capture Unterm's own window instead of the whole screen. |
| `--base64` | Include `image.base64` in `--json` output for normal screen capture and `--self`. Long screenshot modes still return file paths. |
| `--scrollback` | Render the active pane's entire scrollback plus viewport into one tall PNG. This is headless re-rendering, not pixel stitching, so it works even when the window is occluded. |
| `--pane-id <N>` | Pane id for `--scrollback`; defaults to the active pane. |
| `--max-rows <N>` | Row cap for `--scrollback`; keeps the most recent rows. |
| `--dpi <N>` | Raster DPI for `--scrollback`, clamped to 48-288. |
| `--scroll-app <APP>` | macOS external long screenshot: find another app's window by app-name substring, scroll it, and stitch the frames. |
| `--scroll-title <TEXT>` | macOS external long screenshot: match the target window by title substring. |
| `--scroll-pid <PID>` | macOS external long screenshot: match the target window by owning process id. |
| `--max-frames <N>` | Frame cap for external scroll stitching. |
| `-o`, `--output <FILE>` | Local path to write the PNG to. The CLI copies from the MCP-side path if MCP writes elsewhere. End state: `FILE` exists. |

```sh
$ unterm-cli screenshot --output /tmp/cap.png
/tmp/cap.png
```

```sh
$ unterm-cli screenshot --scrollback --max-rows 20000 --output /tmp/pane-long.png
/tmp/pane-long.png
```

```sh
$ unterm-cli screenshot --scroll-app Safari --scroll-title "Release notes" --max-frames 40 --output /tmp/safari-long.png
/tmp/safari-long.png
```

```sh
$ unterm-cli --json screenshot | jq '.image'
{
  "path": "/Users/alexlee/.unterm/screenshots/screen_20260503_221502_301.png",
  "width": 2560,
  "height": 1440,
  "left": 0,
  "top": 0
}
```

The `--json` form additionally returns `captures[]` with the on-screen text content of every visible Unterm pane — handy if you want to capture both the pixels and the textual state in one round trip. Add `--base64` when you need the PNG inline rather than only as a path.

**Real-world use case** — a CI step that snaps the screen of a self-hosted runner whenever the build fails, for human triage:

```sh
# .github/scripts/on-failure.sh
unterm-cli screenshot --include-window -o "/tmp/ci-failure-${GITHUB_RUN_ID}.png"
gh run upload-artifact "/tmp/ci-failure-${GITHUB_RUN_ID}.png" --name screenshot
```

## scrollback

Dump a pane's scrollback plus visible viewport as text. This is the text-first companion to `screenshot --scrollback` when the next consumer is an LLM or script.

```text
unterm-cli scrollback [--pane-id ID] [--tail N] [--start-line N] [--end-line N] [--escapes] [-o FILE]
```

Use `--tail` for the common "give me the recent terminal output" case:

```sh
$ unterm-cli scrollback --tail 200 -o /tmp/recent-terminal.txt
/tmp/recent-terminal.txt
```

`--json` returns the raw `screen.scrollback_text` payload, including `first_row`, `row_count`, `scrollback_top`, and viewport metadata.

## upload

Upload a local file through the running Unterm GUI's MCP server. Backed by `upload.file`; credentials stay in `~/.unterm/upload.json`, and the MCP response only returns the public URL, provider, key, and size.

```text
unterm-cli upload setup [--force]
unterm-cli upload config-path
unterm-cli upload <FILE> [--provider oss|cos|qiniu] [--key OBJECT_KEY] [--raw]
```

Run `setup` once to create a starter config:

```sh
$ unterm-cli upload setup
/Users/alexlee/.unterm/upload.json
```

Edit that file with your OSS, COS, or Qiniu credentials. `setup` refuses to overwrite an existing file unless you pass `--force`.

```sh
$ unterm-cli upload /tmp/pane-long.png --raw
https://cdn.example.com/unterm/pane-long_20260619_001122.png
```

```sh
$ unterm-cli --json upload /tmp/pane-long.png | jq '{url, provider, key, size}'
```

## reference

Print the MCP methods, CLI subcommands, and live keybindings exposed by Unterm. When a control server is running, this calls `meta.surface` for MCP methods and any live keybinding data it can provide, while CLI subcommands come from the current `unterm-cli` binary so local help never lags behind an older running server. When no Core or GUI control server is reachable, it falls back to the compiled-in MCP/CLI reference tables and returns an empty `keybindings` list.

```text
unterm-cli reference [--section mcp|cli|keys] [--filter TEXT]
```

```sh
$ unterm-cli reference --section mcp --filter capture.scrollback
# excerpt
  capture.scrollback               Scrolling screenshot of a pane: render the ENTIRE scrollback to one tall PNG (headless re-render, works even occluded). Prefer screen.scrollback_text when only the text matters.
```

```sh
$ unterm-cli --json reference --section cli --filter agent | jq '.cli_commands[].name'
"agent"
```

If the JSON output contains `"source": "static_fallback"`, no control server was reachable. The MCP and CLI tables are still useful for onboarding or scripts, but live keybindings require a running GUI.

## server

Inspect the running MCP server. These commands are useful as preflight checks before an outer agent starts driving panes.

```text
unterm-cli server info
unterm-cli server health
unterm-cli server capabilities
unterm-cli server selftest [--session-id <ID>]
```

```sh
$ unterm-cli server health
Status:       ok
Panes:        3
Term:         xterm-256color
```

```sh
$ unterm-cli server selftest
true
CHECK                          OK
mux.available                  true
server.health                  true
```

Use `--json` for the full `server.health`, namespace capability map, or self-test check details.

## policy

Probe the current MCP write policy without changing it.

```text
unterm-cli policy check -- <COMMAND...>
```

```sh
$ unterm-cli policy check -- rm -rf build
true
Reason:       Policy disabled
```

Use `--json` for `{ allowed, reason }`.

## theme

List preset themes or switch to one. When Unterm is running, the CLI uses the local HTTP settings API so existing windows repaint immediately. If no GUI is available, it writes `~/.unterm/theme.json` for the next launch.

```text
unterm-cli theme list
unterm-cli theme switch <NAME>   # alias: theme set
```

The six built-in presets are `standard`, `midnight`, `daylight`, `classic`, `notion-dark`, and `notion-light`.

```sh
$ unterm-cli theme list
Active: classic

   ID         NAME           COLOR SCHEME                 DESCRIPTION
   standard   Standard       Unterm Dark                  Neutral high-contrast terminal style
   midnight   Midnight       Unterm Midnight              Low-glare blue-black workspace
   daylight   Daylight       Unterm Daylight              Readable light mode for bright rooms
*  classic    Classic        Classic Dark                 Plain high-contrast terminal colors
   notion-dark Notion Dark    Notion Dark                  Notion-inspired warm dark
   notion-light Notion Light  Notion Light                 Notion-inspired clean light
```

```sh
$ unterm-cli --json theme switch daylight
{
  "color_scheme": "Unterm Daylight",
  "id": "daylight",
  "name": "Daylight",
  "applied_live": true,
  "switched": true
}
```

Names are matched case-insensitively. Unknown names error out with a non-zero exit and a useful message ("Unknown theme 'foo'. Run …").

## lang

List, set, or print the active interface locale. Operates on `~/.unterm/lang.json` — also no MCP round-trip. Affects only the locale the CLI itself uses for human-formatted output and (after the GUI re-reads the file) the GUI's UI strings.

```text
unterm-cli lang list
unterm-cli lang set <CODE>
unterm-cli lang current
```

Supported codes: `en-US`, `zh-CN`, `zh-TW`, `ja-JP`, `ko-KR`, `de-DE`, `fr-FR`, `it-IT`, `hi-IN`.

```sh
$ unterm-cli lang list
    CODE     ACTIVE         NAME
*   en-US    *              English
    zh-CN                   简体中文
    zh-TW                   繁體中文
    ja-JP                   日本語
    ko-KR                   한국어
    de-DE                   Deutsch
    fr-FR                   Français
    it-IT                   Italiano
    hi-IN                   हिन्दी
```

```sh
$ unterm-cli --json lang current
{
  "code": "en-US",
  "name": "English"
}
```

`lang set` persists; the global `--lang <code>` flag does not. If you're scripting English-only output for downstream tools, prefer `--lang en-US` per-invocation rather than overwriting the user's preference.

## shell-completion

Emits a completion script for your shell. Sourceable.

```text
unterm-cli shell-completion --shell <bash|zsh|fish|elvish|powershell>
```

```sh
$ unterm-cli shell-completion --shell zsh > "${fpath[1]}/_unterm-cli"
$ exec zsh
```

This uses the standard clap generator (`clap_complete`), which means the completions cover everything in `unterm-cli --help`, including the MCP subcommands documented above.

---

## Scripting cookbook

A few patterns that come up often. They all assume `unterm-cli` is on `PATH` (the installer drops it next to the GUI binary; `brew install --cask unterm` symlinks it).

### Wait for a long-running build, then notify

```sh
#!/usr/bin/env bash
# Block until pane $PANE_ID has been idle for 10s, then send a notification.
PANE_ID="${1:?usage: wait-then-notify <pane-id>}"

last_y=""
idle_start=""
while :; do
  cur_y=$(unterm-cli --json session list \
    | jq -r --argjson id "$PANE_ID" '.sessions[] | select(.id == $id) | .cursor.y')
  now=$(date +%s)
  if [ "$cur_y" = "$last_y" ]; then
    [ -z "$idle_start" ] && idle_start=$now
    if [ $((now - idle_start)) -ge 10 ]; then break; fi
  else
    idle_start=""
    last_y=$cur_y
  fi
  sleep 2
done

osascript -e 'display notification "build finished" with title "unterm"'
```

The cursor `y` value monotonically increases as a pane scrolls, so freezing it for N seconds is a cheap "is idle" proxy. For a stricter signal, pair this with `session export` and check the tail for a known terminator string.

### Capture a screenshot from a CI runner for the failure report

```sh
# scripts/on-test-failure.sh
mkdir -p artifacts
unterm-cli screenshot --include-window -o "artifacts/failure-$(date +%s).png"
unterm-cli --json session list \
  | jq '.sessions[] | {id, title, lines: .cursor.y}' \
  > artifacts/pane-state.json
```

Self-hosted runners that boot Unterm at startup get a full visual + textual snapshot every time a check fails. Both files are well under GitHub's artifact size limits.

### Auto-rotate session recordings nightly

```sh
# crontab: 30 3 * * *  /usr/local/bin/rotate-unterm-sessions.sh
#!/usr/bin/env bash
THRESHOLD=$(date -v-30d +%Y-%m-%dT%H:%M:%S 2>/dev/null || date -d '30 days ago' -Iseconds)

unterm-cli --json sessions list \
  | jq -r --arg t "$THRESHOLD" '.[] | select(.started_at < $t) | .md_path' \
  | while read -r path; do
      [ -f "$path" ] && gzip -9 "$path"
    done
```

A cron entry at 3:30am scans the archive, gzips anything older than 30 days. The recordings index in MCP doesn't auto-evict — it's expected that you bring your own retention policy.

### Switch theme based on time of day from cron

```sh
# crontab:
#   0 7 * * *  /usr/local/bin/unterm-cli theme switch daylight >/dev/null
#   0 19 * * * /usr/local/bin/unterm-cli theme switch midnight >/dev/null
```

That's it — if Unterm is running, `theme switch` applies through the local settings API immediately. If it is not running, the command writes `theme.json` and the next launch starts with that theme.

For something fancier (latitude/longitude sunrise rather than wall-clock 7am), wrap a Python `astral` call around the same two CLI invocations.

### Drive a multi-pane lint dashboard

This pattern is the closest the CLI comes to "outer agent" territory — kick off the same command in every pane that matches a filter, then aggregate the tails.

```sh
#!/usr/bin/env bash
# lint-everything.sh — run `make lint` in every pane whose title contains "lint"
PANES=$(unterm-cli --json session list \
  | jq '[.sessions[] | select(.title | test("lint"; "i")) | .id]')

# session.send_text isn't yet wrapped by the CLI — direct MCP via netcat.
PORT=$(jq -r .mcp_port ~/.unterm/server.json)
TOKEN=$(jq -r .auth_token ~/.unterm/server.json)

# Use a small helper that speaks line-delimited JSON-RPC.
send() {
  python3 -c "
import json, socket, sys
s = socket.create_connection(('127.0.0.1', $PORT))
def call(m, p):
    s.sendall((json.dumps({'jsonrpc':'2.0','id':1,'method':m,'params':p}) + '\n').encode())
    return json.loads(s.makefile().readline())
print(call('auth.login', {'token': '$TOKEN'}))
print(call('$1', $2))
"
}

for id in $(echo "$PANES" | jq '.[]'); do
  send session.send_text "{\"id\": $id, \"text\": \"make lint\\n\"}"
done

# Wait then export tails.
sleep 30
mkdir -p /tmp/lint-report
for id in $(echo "$PANES" | jq '.[]'); do
  unterm-cli session export --pane-id "$id" -o "/tmp/lint-report/pane-$id.md"
done
```

For simple pane IO, prefer `unterm-cli session input` and `unterm-cli session text` over hand-written JSON-RPC snippets.

### Race three agents on one bug, keep the best diff

The full fleet loop, scripted: launch, wait until no member is still working, verify every attempt, then let the ranked review pick the winner — no eyeballing required.

```sh
#!/usr/bin/env bash
set -e
FLEET=$(unterm-cli --json fleet launch --agents claude,claude,codex --cwd ~/src/app \
  "fix the login redirect loop" | jq -r '.id')

# Block until no member reports state=working.
while unterm-cli --json fleet list \
  | jq -e --arg f "$FLEET" \
      '.fleets[] | select(.id == $f) | .members[] | select(.agent_state == "working")' \
      >/dev/null; do
  sleep 30
done

# Run tests in every member's worktree (command inferred: cargo test, npm test, …).
for n in 1 2 3; do
  unterm-cli review verify --fleet "$FLEET" --member "$n"
done

# Block until no verification is still pending or running.
while unterm-cli --json review list \
  | jq -e --arg f "$FLEET" \
      '.fleets[] | select(.id == $f) | .members[]
       | select(.verification.status == "pending" or .verification.status == "running")' \
      >/dev/null; do
  sleep 15
done

# rank 1 = best candidate: latest verification passed, smallest diff wins ties.
BEST=$(unterm-cli --json review list \
  | jq -r --arg f "$FLEET" \
      '.fleets[] | select(.id == $f) | .members[] | select(.rank == 1) | .n')
unterm-cli review diff --fleet "$FLEET" --member "$BEST" --stat

# Merge the winner (gated on its passed verification), discard the rest, tear down.
unterm-cli review merge --fleet "$FLEET" --member "$BEST"
for n in 1 2 3; do
  [ "$n" = "$BEST" ] || unterm-cli review discard --fleet "$FLEET" --member "$n"
done
unterm-cli fleet clean "$FLEET"
```

Note the "wait" condition uses the cockpit state, which is exact when `agent enable-hooks` has been run — a member that stops to ask a question shows `waiting`, not `working`, so the loop exits and you can go answer it. The merge is verification-gated: if even the rank-1 member's tests failed, `review merge` refuses rather than staging broken code (use `fleet retry` to give that member another attempt in the same worktree, or `--force` if you know better — the override is audited). `review merge` leaves the squash staged in the base repo; commit it with your own message.

### Notify when any agent wants attention

`agent inbox --json` is stable enough to poll from cron — waiting agents surface first, with the pane to jump to:

```sh
# crontab: * * * * *  /usr/local/bin/agent-inbox-notify.sh
#!/usr/bin/env bash
WAITING=$(unterm-cli --json agent inbox 2>/dev/null \
  | jq -r '.items[] | select(.state == "waiting")
           | "pane \(.pane_id): \(.agent) — \(.task_hint // "?")"')
[ -z "$WAITING" ] && exit 0
osascript -e "display notification \"$WAITING\" with title \"unterm cockpit\""
```

When Unterm isn't running the CLI exits non-zero with no stdout, so the guard makes the cron job a silent no-op. Swap `osascript` for `ntfy`/Slack webhooks on other platforms.

### Snapshot pane state into git history

```sh
# Drop me in $PROJECT/.git/hooks/post-commit
PANE=$(unterm-cli --json session list | jq -r '.sessions[0].id')
DIR=".unterm/per-commit"
mkdir -p "$DIR"
unterm-cli session export --pane-id "$PANE" -o "$DIR/$(git rev-parse --short HEAD).md"
git add "$DIR" 2>/dev/null
```

Every commit records the state of your active terminal as part of the project's `.unterm/` directory. Combined with `.unterm/sessions/...` recordings, you end up with a per-commit log of "what was I actually doing when I made this change". The redaction layer keeps tokens out of the markdown.

### Mirror proxy state into shell sessions

Worth repeating from earlier as a one-liner — the shell-init pattern that keeps every new shell aligned with the GUI:

```sh
# zsh
eval "$(unterm-cli proxy env 2>/dev/null)"
```

If `unterm-cli` isn't installed, the `eval` no-ops because there's no output. If no Core or GUI control server is running, the CLI exits non-zero with no stdout, same effect. Cheap and safe to drop into init scripts.

---

## Exit codes

`unterm-cli` follows the standard convention: `0` on success, `1` on any error. The one deliberate exception is `agent signal`, which exits `0` quietly when no Unterm is running, because it is designed to be called from other agents' notification hooks and must never break the agent it reports on.

The Rust source is `unterm-cli/src/main.rs::main()`, which propagates an `anyhow::Error` from each subcommand; a failed command exits the process with status `1`. There are no granular status codes today — every failure mode collapses to `1`. Distinguish them by message on stderr:

```sh
$ unterm-cli proxy switch nonexistent-node
ERROR  unterm_cli > MCP proxy.switch failed [-32603]: Proxy node 'nonexistent-node' not found; terminating
$ echo $?
1

$ unterm-cli session record start --pane-id 99999
ERROR  unterm_cli > MCP session.recording_start failed [-32603]: Session 99999 not found; terminating
$ echo $?
1

$ unterm-cli lang set bogus-locale
ERROR  unterm_cli > unknown locale 'bogus-locale'. Run `unterm-cli lang list` to see options.; terminating
$ echo $?
1
```

The bracketed code in MCP errors (e.g. `[-32603]`) is the JSON-RPC error code — `-32603` is the spec's "internal error". For known constraint failures (locale, theme name, missing pane) the message is unambiguous. Script defensively:

```sh
if ! out=$(unterm-cli proxy status 2>&1); then
  case "$out" in
    *"control server is not running"*) launchctl start com.unzooai.unterm; sleep 2; ;;
    *) echo "unterm-cli failed: $out" >&2; exit 1 ;;
  esac
fi
```

If you need machine-readable failure detail, use `--json` and inspect the resulting `error` field — but note that today the CLI translates JSON-RPC errors to `anyhow` errors before printing, so `--json` only formats the *success* result. For raw protocol-level errors, talk to MCP directly with the netcat / Python pattern above. That's the escape hatch when the CLI's surface isn't quite enough.

## provider

Capabilities that live in another process — today that means a browser
(Unzoo). Unterm finds it, binds it, and uses it under a lease you can take
back.

```bash
unterm-cli provider list [--rediscover]
unterm-cli provider bind unzoo
unterm-cli provider diagnose unzoo
unterm-cli provider acquire browser --actor claude --ttl 300
unterm-cli provider call <lease> tab_list --seq 1 --capability browser
unterm-cli provider leases [--live]
unterm-cli provider chain <lease>
unterm-cli provider approvals
unterm-cli provider pause|resume|unbind <provider>
unterm-cli provider revoke <lease>
```

`list` prints how each provider was found — `unzoo:rest-port`,
`descriptor:…`, `environment:UNTERM_PROVIDER_…` — because "why is it pointing
there" is the question you have when it points somewhere wrong. Nothing in
Unterm hardcodes a port; a provider that is not running is reported as
unreachable rather than confused with whatever process took its old one.

Every use of a lease needs its own `--seq`, higher than the last. A repeated
number is refused before anything happens: a replay noticed afterwards has
already done the thing it was replaying.

`chain` is the one to reach for after the fact — it prints the lease, the
grant that authorised it, the approval a human answered, and every call made
under it with response hashes.

`approvals` lists what is waiting. It cannot answer them: that happens in
Settings, deliberately, because an agent that could answer its own request
would never have to stop.

## scope

Workspaces: named roots that work is confined to, and that cannot see each
other.

```bash
unterm-cli scope create alpha ~/code/alpha
unterm-cli scope list
unterm-cli scope check <workspace> <path> [--access read|write]
unterm-cli scope archive <workspace>
```

`check` prints the **resolved** path as well as the verdict — that is what
the decision was actually made about, and it is rarely what you typed.
Symlinks, `..`, case on a case-folding volume, and Windows verbatim / UNC
prefixes are all resolved first. Creating a workspace inside another is
refused at creation, naming the one it would sit inside: an outer allow and
an inner deny describe the same files.

Exit status is non-zero when a path is denied, so this composes in scripts.

## artifact

What tasks produced, addressed by content.

```bash
unterm-cli artifact list [--task <id>]
unterm-cli artifact usage
unterm-cli artifact verify <id>
unterm-cli artifact forget <id>
```

Bytes live on disk under `~/.unterm/artifacts/sha256/`, never in the
database — a SQLite file with a screen recording in it is one nobody can
copy, back up or open. `usage` reports what deduplication saved. `verify`
recomputes the hash rather than trusting the index. `forget` is
reference-counted: the bytes go only when the last row mentioning them does.

## evidence

One task's whole story, for somebody who was not there.

```bash
unterm-cli evidence export <task-id> ./bundle
unterm-cli evidence verify ./bundle
unterm-cli evidence audit
```

A bundle is plain files plus a manifest of hashes — the task, its runs and
steps, the leases used and the authorisation behind each, every provider call
with request/response hashes, the artifacts, and the slice of the audit trail
that names this task. Nothing from any other task: a bundle is handed to
somebody, and a stray row from unrelated work is a leak.

`verify` recomputes. Two exports of an unchanged task carry the same record
hash, so "has this been edited" is answerable.

`audit` walks the hash-chain of the local action log and reports the first
break. Worth being precise about what that buys: the chain makes an edit
*visible*, not impossible. It is a file, and whoever can read it can edit it;
somebody who rewrites everything from the edit onwards leaves a chain that
verifies. The defence against that is a copy somewhere else.

## system

The processes Unterm is made of, and the data behind them.

```bash
unterm-cli system status
unterm-cli system diagnostics [--out FILE]
unterm-cli system snapshot [--version V] / snapshots / restore <id>
unterm-cli system upgrade --live PATH --staged PATH --to VERSION
unterm-cli system installs
unterm-cli system uninstall-plan [--remove-data]
unterm-cli system uninstall [--remove-data] [--yes]
unterm-cli system reconcile
```

`status` distinguishes alive from usable — a Core still replaying scrollback
is `starting`, not `ready` — and answers the question people actually have:
*can work happen without a window?*

`diagnostics` is safe to send. It is built by naming what goes in, so a field
nobody thought about is absent rather than leaked: versions, process health,
counts. No tokens, no prompts, no commands, and paths only as shapes
(`<path with 7 components>`).

`upgrade` swaps a staged binary in, runs it, and puts **both** the program
and the data back if it does not answer. Rolling back is itself snapshotted,
because somebody who rolls back by mistake has to be able to roll forward.

`uninstall-plan` describes and never removes; `uninstall` requires `--yes`
and keeps your data unless you ask otherwise. The plan says what each thing
is — "tasks.db — every task, run, step, approval and lease" — because "delete
my data?" is answerable only if the answer says what the data is.

## agent_session (MCP only)

Hosting a CLI agent as a session with structured events is currently an MCP
surface rather than a CLI one: `agent_session.start` / `events` /
`submit_input` / `interrupt` / `status` / `close`. See the
[MCP reference](/docs/mcp-reference/#governance-providers-capabilities-evidence).

---

Source for everything described here lives at [github.com/zhitongblog/unterm](https://github.com/zhitongblog/unterm) under `unterm-cli/src/`. Open issues / PRs there if a method exists on MCP that you'd like surfaced as a first-class CLI subcommand.
