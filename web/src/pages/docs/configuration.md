---
layout: ../../layouts/Doc.astro
title: Configuration files reference
subtitle: The ~/.unterm/ directory schema — every JSON file, every field, every default. Edit by hand or via the Web Settings UI / unterm-cli — both write the same files.
kicker: Docs / Configuration
date: 2026-05-03
---

Unterm keeps its settings in two kinds of file under `~/.unterm/`:

- **`unterm.conf`** — the terminal's own config: fonts, colours, window size and padding, the shell, `[env]` variables, `[keys]` bindings, MCP confirmation and Agent Cockpit options. Plain `key = value` lines with `[section]` headers, read when Unterm starts. Every key is listed in the [config file reference](/docs/config-reference). If you still have an `unterm.lua` from an older version and no `unterm.conf` yet, Unterm converts it on start and logs any lines it could not translate.
- **Small JSON files**, one per product surface — proxy, theme, language, scrollback, recording, agent handshake. Each has one writer, a documented schema, and degrades gracefully when missing. You can edit any of them by hand, or let Web Settings (Settings ▼ → Open Web Settings) and `unterm-cli` write them — both paths produce the same on-disk format.

This page documents the JSON files and the runtime state next to them. Why this shape: a bunch of one-purpose files is easier for an outside agent to read and write than one big document. An MCP client that wants to flip the proxy touches `proxy.json` and nothing else.

## The directory

On macOS and Linux the root is `~/.unterm/`. On Windows it's `%USERPROFILE%\.unterm\` (typically `C:\Users\<you>\.unterm\`). Resolved at runtime via `dirs_next::home_dir()`.

A populated tree looks like this:

```
~/.unterm/
├── unterm.conf              # terminal config (see /docs/config-reference)
├── server.json              # active instance pointer (ports + auth token)
├── active.json              # same shape as server.json; modern multi-instance pointer
├── instances/               # one file per running Unterm process
│   ├── alpha.json
│   ├── bravo.json
│   └── …
├── auth_token               # legacy plaintext token mirror (matches server.json:auth_token)
├── update_check.json        # latest GitHub release vs. current binary
├── last_session.json        # window geometry + tab CWDs from last close
├── first_run.json           # presence flag — onboarding hint shown once
├── onboarded.json           # which UI surfaces the user has seen
├── projects.json            # MRU list of recently picked project dirs (max 12)
│
├── proxy.json               # HTTP/SOCKS proxy settings + nodes
├── theme.json               # active visual theme
├── lang.json                # active UI locale
├── scrollback.json          # per-pane scrollback line cap
├── compat.json              # TERM_PROGRAM masquerade
├── recording.json           # session recorder + redaction flags
├── tasks.db                 # durable task store; Agent Cockpit fleets live here (an old fleets.json is imported once)
├── checkpoints.json         # Agent Cockpit: pre-agent-work snapshots per repo (dangling-commit SHAs)
├── verifications.json       # Agent Cockpit: fleet-member verification results (status, exit code, log)
│
├── sessions/                # recordings (when project dir isn't writable)
│   ├── _orphan/
│   ├── <project-slug>/
│   │   └── <YYYY-MM-DD>/
│   │       ├── tab-<id>-<HHMMSS>.log
│   │       └── tab-<id>-<HHMMSS>.md
│   └── index.json
│
└── screenshots/             # region screenshots from selection capture
    ├── region_hidden_<YYYYMMDD>_<HHMMSS>_<ms>.png
    ├── region_visible_<YYYYMMDD>_<HHMMSS>_<ms>.png
    ├── screen_<YYYYMMDD>_<HHMMSS>_<ms>.png      # capture.screen MCP method
    └── window_<YYYYMMDD>_<HHMMSS>_<ms>.png      # capture.window MCP method
```

The first half of this doc covers files Unterm rewrites on every launch — runtime state, not intent. Don't hand-edit them. The second half covers intent files — Unterm reads them on demand and never overwrites unless you go through Web Settings or the CLI.

## Files written automatically

These describe what's currently running, not what you want to be true. They're documented here so an external agent reading them knows how to interpret the bytes.

Current builds add one more runtime pointer to the older GUI registry: `core.json`. `unterm-core` writes it with the Core MCP port, auth token, pid, product version, protocol version, data schema version, process role, and start time. MCP-backed CLI commands and `unterm-cli mcp-stdio` prefer `core.json` so automation survives GUI restarts. `server.json`, `active.json`, and `instances/<id>.json` remain the GUI-instance registry and compatibility surface for older scripts or tools that specifically need a window's HTTP settings port.

### `server.json` — legacy single-instance pointer

The original handshake file. Every external agent that wants to talk to Unterm reads this to find the local TCP port and auth token. Mirrored from the most recently launched instance whose ancestor is still alive. Format is identical to `instances/<name>.json` because `ServerInfo` is a Rust type alias for `InstanceInfo`.

```json
{
  "id": "alpha",
  "mcp_port": 19876,
  "http_port": 19877,
  "auth_token": "ff547121-8325-486b-bf25-2f0ba209dbff",
  "pid": 56611,
  "started_at": "2026-05-02T13:51:22.899534+08:00",
  "title": null,
  "cwd": null,
  "version": "0.5.5",
  "platform": "macos"
}
```

| Field | Type | Description |
|---|---|---|
| `id` | string | NATO-phonetic instance name: `alpha`, `bravo`, `charlie`, … `zulu`. When all 26 base names are taken simultaneously, a digit suffix is appended (`alpha2`, `bravo2`, …). |
| `mcp_port` | u16 | TCP port the MCP JSON-RPC server bound. Starts at `19876` and probes up to 5 next ports before falling back to OS-assigned. May briefly be `0` between MCP startup and HTTP startup. |
| `http_port` | u16 | TCP port the HTTP settings server bound. Starts at `19877` with the same fallback logic. May be `0` for ~50ms after launch before the HTTP server stamps it in place. |
| `auth_token` | string | UUIDv4 generated fresh per launch. Required on every MCP / HTTP request. Treat it like an SSH key. |
| `pid` | u32 | OS process ID of the Unterm GUI process. Used by sibling instances to detect crashed peers. |
| `started_at` | string | RFC 3339 timestamp of when MCP started. Used to pick "most recent" when active.json is stale. |
| `title` | string \| null | User-overridable display label. `null` = use auto-derived `Unterm — <id> — <project>`. Set via `unterm-cli instance set-title …`. |
| `cwd` | string \| null | Last-seen working directory of the active pane. Refreshed periodically by the foreground update loop; best-effort. Agents that want a live value should call `session.cwd` instead. |
| `version` | string | Cargo package version baked into this binary, e.g. `"0.5.5"`. |
| `platform` | string | `std::env::consts::OS` — one of `macos`, `linux`, `windows`. |

Older releases wrote a 5-field subset (mcp_port, http_port, auth_token, pid, started_at). Both old and new readers cope — the new fields all carry `#[serde(default)]`.

### `active.json` — modern multi-instance pointer

Same schema as `server.json`. When you have several Unterm windows open, `active.json` points at the most recently launched live one, and only updates when the previous active dies — focus changes don't touch disk.

When to read `active.json` vs. `server.json`: prefer `server.json` for compatibility (every release has had it). Use `active.json` only if you're explicitly opting into multi-instance pointer semantics.

### `instances/<id>.json` — per-process metadata

One file per running Unterm. Same schema as `server.json`. Created via O_EXCL atomic open (so two simultaneous launches can't claim the same NATO name) and deleted on graceful shutdown. Stale files from crashes get cleaned up by the next-launching instance, which scans the dir and removes entries whose PID is no longer alive.

Enumerate from outside via `ls ~/.unterm/instances/*.json` and parse, or call `instance.list` over MCP.

### `auth_token` — legacy plaintext token

A bare UUID, no JSON wrapper. Same value as `server.json:auth_token`. Older agents and the early `unterm-cli` read this file directly; current MCP-backed code reads `core.json` first and keeps this only as the final legacy fallback. Kept in sync so legacy readers never see a stale value.

If you commit it to a public repo, the worst case is local-machine attack — the MCP server binds 127.0.0.1 only, so off-host the token has no power. Regenerate by relaunching Unterm.

### `update_check.json` — GitHub release poll cache

Written by a background thread that hits `api.github.com/repos/zhitongblog/unterm/releases/latest` every six hours (first check 20s after launch). Three UI surfaces — the ▼ menu, Web Settings, and the status bar — read this same file rather than each polling GitHub independently.

```json
{
  "checked_at": "2026-05-03T19:51:47.557905+08:00",
  "current": "20260502-121851-b3680e89",
  "current_pkg": "0.5.5",
  "latest_tag": "v0.11",
  "upgrade_available": false
}
```

| Field | Type | Description |
|---|---|---|
| `checked_at` | string | RFC 3339 timestamp of the last successful API hit. |
| `current` | string | Build stamp baked into the binary — `<date>-<time>-<sha>`. Useful for matching against CI artifacts. |
| `current_pkg` | string | Cargo package version, e.g. `"0.5.5"`. Compared semver-numerically against `latest_tag`. |
| `latest_tag` | string | Verbatim tag string from the GitHub `tag_name` field, e.g. `"v0.11"` or `"v1.2.3"`. |
| `upgrade_available` | bool | True only when `latest_tag` parses to three numeric components and is strictly greater than `current_pkg`. Any parse error means false — we never raise a phantom upgrade banner. |

Delete the file to force a recheck on next launch (or POST `/api/updates/check` to the HTTP server).

### `last_session.json` — window geometry restore

Written when the last Unterm window closes; read on next launch to put the window back where you left it.

```json
{
  "x": 0,
  "y": 0,
  "width": 854,
  "height": 552,
  "dpi": 72,
  "tabs": [
    { "cwd": "file:///Users/alexlee/", "title": "" }
  ],
  "saved_at": "2026-05-02T18:14:19.802475+08:00"
}
```

| Field | Type | Description |
|---|---|---|
| `x`, `y` | i32 | Client-area position in screen pixels. Negative is allowed (multi-monitor setups). |
| `width`, `height` | usize | Client-area size in physical pixels. Sizes below 800×480 are ignored on load (treated as runaway-resize garbage). |
| `dpi` | usize | DPI at save time, used to preserve apparent size across monitor swaps. |
| `tabs` | array | One entry per tab that was open. Currently captures `cwd` (file:// URL) and `title` only. |
| `saved_at` | string | RFC 3339 timestamp. |

The tiny-size guard exists because early builds occasionally persisted a 1×1 window after a malformed resize event, leaving users to launch into an unusable speck.

### `first_run.json` and `onboarded.json` — UI hint flags

`first_run.json` is just `{"first_run": true}`. Its presence (not contents) is the signal: when missing on launch, Unterm writes a one-line discovery hint into the initial pane and then creates the file so the hint never appears again.

`onboarded.json` tracks per-feature first-time state for the Settings menu's "new" badges:

```json
{ "session_recording": true }
```

Delete either file to re-enable the corresponding hint.

### `projects.json` — recent project picker MRU

A bare JSON array of absolute path strings, capped at 12 entries, populated when you pick a project directory through the file dialog. Most-recent at index 0. Hand-edit to seed the picker if useful.

```json
[
  "/Volumes/Dev/code/unflick/",
  "/Users/alexlee/Downloads/",
  "/Users/alexlee/Documents/"
]
```

## Files you might edit

Intent files. Web Settings writes them when you change something there; you can also edit them directly. The CLI writes the same shapes via `unterm-cli proxy …`, `unterm-cli theme …`, and similar.

### `proxy.json` — HTTP/SOCKS proxy

Toggling the proxy on at the ▼ menu writes this file. When enabled, every spawned shell gets `HTTP_PROXY` / `HTTPS_PROXY` / `ALL_PROXY` / `NO_PROXY` injected from the resolved values, so `git`, `curl`, `npm`, and your AI agents all route correctly without you per-shell-exporting anything.

```json
{
  "enabled": true,
  "mode": "auto",
  "http_proxy": "http://127.0.0.1:7897",
  "socks_proxy": "socks5://127.0.0.1:7897",
  "no_proxy": "localhost,127.0.0.1,::1",
  "current_node": null,
  "nodes": []
}
```

| Field | Type | Default | Valid values | Description |
|---|---|---|---|---|
| `enabled` | bool | `false` | `true` / `false` | Master switch. When false, no proxy env vars are injected and the rest of the file is irrelevant. |
| `mode` | string | `"off"` | `"auto"` / `"manual"` (anything not `"manual"` is treated as auto) | Auto runs `system_proxy::detect()` at every spawn and overlays detected URLs over the on-disk values. Manual trusts the on-disk URLs verbatim. |
| `http_proxy` | string \| null | `null` | URL like `http://host:port` | HTTP/HTTPS upstream. Used for `HTTP_PROXY`, `HTTPS_PROXY`, `http_proxy`, `https_proxy` env vars. |
| `socks_proxy` | string \| null | `null` | URL like `socks5://host:port` | SOCKS upstream. Used for `ALL_PROXY` / `all_proxy`. |
| `no_proxy` | string | `"localhost,127.0.0.1,::1"` | Comma-separated host patterns | Hosts to bypass the proxy. Falls back to the default if blank. |
| `current_node` | string \| null | `null` | Name of an entry in `nodes` | Active proxy profile selected by `proxy.switch`. Optional — for users with multiple upstream profiles. |
| `nodes` | array | `[]` | Array of `{name, url, latency_ms?, available}` | Named alternative profiles. The Web Settings UI exposes a list for one-click switching. |

**When you'd edit by hand vs. let the UI do it:** auto mode just works for most users — install Clash/Surge/V2Ray/your-favorite, click the ▼ menu's proxy toggle once. Hand-edit when you need a non-detectable URL (e.g., a remote SOCKS over an SSH tunnel that no system setting points at) — set `mode` to `"manual"` and write URLs directly.

The auto-overlay exists to prevent the "I changed Clash from 7890 to 7897 and nothing notices" bug: without it, `proxy.json` keeps the stale port forever and every spawned shell gets the wrong value. Auto mode re-detects on every spawn.

### `theme.json` — visual theme

```json
{
  "theme": "classic",
  "name": "Classic",
  "color_scheme": "Classic Dark"
}
```

| Field | Type | Default | Valid values | Description |
|---|---|---|---|---|
| `theme` | string | `"standard"` | `"agent-inbox"` / `"standard"` / `"midnight"` / `"daylight"` / `"classic"` / `"notion-dark"` / `"notion-light"` | Preset id. The other two fields are derived from this — they're written for human readability but `theme` is the authoritative key. |
| `name` | string | `"Standard"` | Any | Human label of the active preset. Cosmetic. |
| `color_scheme` | string | `"Unterm Dark"` | Any color scheme name known to Unterm | Underlying termwiz color scheme. Cosmetic when present alongside `theme`. |

The built-ins:

| `theme` id | `color_scheme` | Description |
|---|---|---|
| `agent-inbox` | Agent Inbox | Deep neutral with amber status accents |
| `standard` | Unterm Dark | Neutral high-contrast terminal style |
| `midnight` | Unterm Midnight | Low-glare blue-black workspace |
| `daylight` | Unterm Daylight | Readable light mode for bright rooms |
| `classic` | Classic Dark | Plain high-contrast terminal colors |
| `notion-dark` | Notion Dark | Notion-inspired warm dark |
| `notion-light` | Notion Light | Notion-inspired clean light |

**When you'd edit by hand:** rarely. The ▼ menu picker, Web Settings, and `unterm-cli theme set midnight` all write this with the three fields in lockstep. If Unterm is running, the CLI applies through the local HTTP settings API immediately; otherwise it writes this file for the next launch. Hand-editing `theme` to an unknown value silently falls back to `standard`.

### `lang.json` — UI locale

```json
{
  "lang": "zh-CN"
}
```

| Field | Type | Default | Valid values | Description |
|---|---|---|---|---|
| `lang` | string | OS-detected locale, falling back to `"en-US"` | `en-US`, `zh-CN`, `zh-TW`, `ja-JP`, `ko-KR`, `de-DE`, `fr-FR`, `it-IT`, `hi-IN` | Active translation bundle. Unknown values are silently ignored — Unterm canonicalizes via the supported list. |

**When you'd edit by hand:** when your OS locale is exotic and Unterm's autodetect picks the wrong fallback. Otherwise use the ▼ menu's language picker or `unterm-cli lang set zh-CN`. The CLI's `--lang` flag overrides the current process only and does not touch this file.

### `scrollback.json` — per-pane scrollback cap

```json
{
  "lines": 10000
}
```

| Field | Type | Default | Valid values | Description |
|---|---|---|---|---|
| `lines` | usize | `10000` | `100` – `999999999` | Maximum scrollback lines retained per pane. Anything outside the range is ignored and the default is used. |

The 10,000-line default is bigger than WezTerm's 3,500 because Unterm users routinely run log-heavy commands (`cargo build`, `find /`, full CI tails) and hit the cap. Memory worst case is `lines × 80 cols × 96 bytes/cell` per pane — about 75 MiB at 10,000 lines.

**When you'd edit by hand:** when you want a non-Web-Settings value, or want it set before the GUI starts. Existing panes keep their old buffer (capacity is locked at pane creation); only new panes pick up the change.

### `compat.json` — TERM_PROGRAM masquerade

Some third-party tools — Gemini CLI, certain IDE detectors, the occasional shell prompt theme — whitelist a fixed set of `TERM_PROGRAM` values and reject anything else. Unterm advertises `TERM_PROGRAM=Unterm` by default, but you can override per user.

```json
{
  "term_program": "WezTerm"
}
```

| Field | Type | Default | Valid values | Description |
|---|---|---|---|---|
| `term_program` | string | `"Unterm"` | Any non-empty string. UI restricts to: `Unterm`, `WezTerm`, `Apple_Terminal`, `iTerm.app`, `xterm`. | Value advertised in the `TERM_PROGRAM` env var. Empty / blank values fall back to the default. |

**When you'd edit by hand:** when a tool hard-codes a supported-terminal list and Unterm isn't on it. Existing shells keep their old env var until you open a new tab. Think twice before spoofing — some tools hand-craft escape sequences for specific terminals and break if you advertise iTerm but don't speak iTerm's protocol.

### `recording.json` — session recorder + redaction

Controls per-pane session recording (the markdown transcript feature) and the regex-based token redactor that runs over recordings before they hit disk.

```json
{
  "recording": {
    "enabled": false
  },
  "redaction": {
    "enabled": true,
    "custom_patterns": []
  },
  "idle_rotate_minutes": 5
}
```

| Field | Type | Default | Description |
|---|---|---|---|
| `recording.enabled` | bool | `false` | When true, every newly-spawned pane starts recording automatically. Existing panes are unaffected. |
| `redaction.enabled` | bool | `true` | When true, the redactor masks GitHub PATs, AWS-style keys, generic 40+ char base64/hex strings, and bearer tokens before writing markdown. Recommended on. |
| `redaction.custom_patterns` | string array | `[]` | Additional regex patterns to mask. Each pattern is compiled as a Rust regex; match groups are replaced with `<REDACTED>`. Bad patterns are silently dropped on load. |
| `idle_rotate_minutes` | u64 | `5` | After this many minutes of pane idleness the recorder rotates the session file (so a long-lived shell doesn't accumulate a single huge transcript). |

**When you'd edit by hand:** mainly to add custom redaction patterns for company-internal token formats. Example for masking hypothetical "INT-" prefixed internal IDs:

```json
{
  "recording": { "enabled": true },
  "redaction": {
    "enabled": true,
    "custom_patterns": [
      "INT-[A-Z0-9]{16,}",
      "(?i)slack_user_token[=:]\\s*\\S+"
    ]
  }
}
```

The default redactor catches most real secrets — patterns are defined in `unterm-services/src/recording/redact.rs`. Custom patterns are applied on top, never instead.

## Per-project files — `<cwd>/.unterm/sessions/…`

Recordings prefer to live inside the project directory itself, not under `~/`. The reasoning: they describe work done in that project, so they should travel with the project. When recording starts in a pane whose cwd is `/Volumes/Dev/code/myapp/`, transcripts go to:

```
/Volumes/Dev/code/myapp/.unterm/sessions/<YYYY-MM-DD>/tab-<id>-<HHMMSS>.log
/Volumes/Dev/code/myapp/.unterm/sessions/<YYYY-MM-DD>/tab-<id>-<HHMMSS>.md
```

The `.log` is the raw event stream (microsecond-tagged base64-encoded chunks); the `.md` is the human-readable rendering produced on stop or on demand. When the project directory isn't writable, the recorder falls back to `~/.unterm/sessions/<project-slug>/<YYYY-MM-DD>/` so you never silently lose a recording.

Markdown rendering uses OSC 133 prompt markers when the shell emits them — each prompt becomes a heading, each command a code block, each output the body following. When OSC 133 is absent the recorder falls back to a plain stdout dump.

`~/.unterm/sessions/index.json` is a registry populated as recordings start and stop. Useful for tooling that wants a "show me everything I've recorded" listing.

`~/.unterm/screenshots/` holds output from the in-app selection-capture feature and the `capture.screen` / `capture.window` MCP methods. Filenames are timestamped: `region_hidden_20260430_140604_233.png`, `screen_20260502_104521_117.png`, `window_20260502_104525_993.png`.

## Permissions and security

Files written by the multi-instance bookkeeping path (`server.json`, `active.json`, `instances/*.json`, `auth_token`, `proxy.json`, `recording.json`, `last_session.json`, `onboarded.json`, `projects.json`, plus the `screenshots/` and `sessions/` dirs) get `0600` mode — readable only by you. Files written by older code paths (`theme.json`, `lang.json`) currently respect the user's umask, which on macOS defaults to 0644. None of those leaked-readable files contain secrets.

The auth token (`auth_token` and `server.json:auth_token`) is the only really sensitive field. Treat it like an SSH key: anyone with read access can drive your Unterm windows from the same machine. Off-host it's powerless because both servers bind 127.0.0.1 only.

Recordings have token redaction on by default. The default rules catch GitHub PATs, AWS-style keys, generic high-entropy 40+ char base64/hex, and bearer tokens. They're a guard rail, not a guarantee — echo a custom secret format the redactor doesn't recognize and it lands in the transcript. Add a regex to `recording.redaction.custom_patterns` for any format you care about.

The HTTP settings server gates everything behind the same auth token. `/bootstrap.json` is the one exception — it returns the token and ports unauthenticated, but only over 127.0.0.1.

## Migrating between machines

Some files are personal config worth carrying; others are runtime state.

| Portable | Not portable |
|---|---|
| `unterm.conf` | `server.json` (regenerated each launch) |
| `theme.json` | `tasks.db` (fleets point at worktrees on this machine) |
| `lang.json` | `active.json` (regenerated each launch) |
| `scrollback.json` | `instances/*.json` (process-specific) |
| `compat.json` | `auth_token` (regenerated each launch) |
| `recording.json` | `update_check.json` (re-fetched from GitHub) |
| `proxy.json` (manual URLs only) | `last_session.json` (window geometry rarely transfers cleanly) |
| `projects.json` (only useful on the same filesystem layout) | `first_run.json` / `onboarded.json` |

If you sync `~/.unterm/` with rsync or a dotfiles tool, exclude `auth_token`, `server.json`, `active.json`, `instances/`, `update_check.json`, `last_session.json`, `first_run.json`, `sessions/`, `screenshots/`. Including them either won't matter (regenerated on launch) or will cause confusion (the receiving machine thinks it's connected to a PID from the source machine).

## Resetting to defaults

Every file is read on demand with a fallback to the built-in default.

- **Reset one setting:** `rm ~/.unterm/<name>.json`. Most reads are at spawn or feature-toggle time, so the change takes effect within seconds.
- **Reset everything:** quit Unterm, `rm -rf ~/.unterm/`, relaunch.
- **Reset just the auth token:** quit Unterm and relaunch — a new UUID is generated on every launch.

Precedence at runtime: file value > Web Settings (which is a write path to the file) > built-in default in the Rust source. There's no Lua / dotenv / env-var override layer for these specific files. One exception: `scrollback_lines` in `unterm.conf`, when set, wins over `scrollback.json`.

When a file is malformed, Unterm logs at `debug` and uses the default — it doesn't delete the bad file or refuse to launch. Run `RUST_LOG=debug unterm 2>&1 | grep -i config` to see which file got rejected.

---

For each schema above, the load-bearing source files in the repo are:

- `unterm-services/src/server_info.rs` — `InstanceInfo` plus the writers for `server.json`, `active.json`, `instances/`, and the auth token.
- `unterm-settings/src/update_check.rs` — `update_check.json`.
- `unterm-services/src/settings.rs` — shared theme, proxy, profile, and compatibility settings, and where `unterm.conf` is found and loaded.
- `unterm-engine/src/next_core/config.rs`, `config_schema.rs`, `config_migrate.rs` — the `unterm.conf` parser, the list of valid keys, and the `unterm.lua` migration.
- `unterm-services/src/cockpit/fleet_store.rs` — fleets in `tasks.db`.
- `unterm-mcp/src/handler.rs` — MCP proxy, policy, workspace, profile, and recording operations.
- `unterm-services/src/launch_env.rs` — resolves profile and proxy environment immediately before a pane is spawned.
- `unterm-app/src/window.rs` and `unterm-settings/src/server.rs` — native and Web settings surfaces; live theme changes use `unterm-services/src/theme_state.rs`.
- `unterm-services/src/i18n/mod.rs` — locale detection, persistence, and English fallback.
- `unterm-services/src/scrollback_options.rs` — scrollback settings.
- `unterm-services/src/recording/` — recording lifecycle, archive, redaction, rendering, and indexing.

Issues, schema additions, or doc fixes welcome at [github.com/zhitongblog/unterm](https://github.com/zhitongblog/unterm).
