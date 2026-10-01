---
layout: ../../layouts/Doc.astro
title: unterm.conf reference
subtitle: Every key the terminal config file accepts, its type, its built-in default, and what it changes. Also covers the file syntax, the [env] and [keys] sections, and the one-time conversion from unterm.lua.
kicker: Docs / Config reference
date: 2026-10-01
---

`unterm.conf` is Unterm's terminal config: fonts, colours, window chrome, the shell, scrollback, the MCP confirmation policy, environment variables and key bindings. The other files in `~/.unterm/` (proxy, theme, language, recording) are covered in [Configuration files](/docs/configuration).

## Where the file lives

- **Path:** `~/.unterm/unterm.conf`. On Windows that is `%USERPROFILE%\.unterm\unterm.conf`. If `UNTERM_STATE_DIR` is set and not empty, the file is `$UNTERM_STATE_DIR/unterm.conf` instead.
- **Another file:** the GUI accepts `--config <file>`, `-c <file>` or `--config=<file>`. A bare file path on the command line (for example, a file dropped on the app icon) is also read as the config. `unterm-core` takes no such flag and always reads the default path.
- **No file:** no error. Every key takes its built-in default.
- **Encoding:** UTF-8. A leading byte-order mark, which Notepad and `Set-Content -Encoding utf8` add, is removed before parsing.
- **Packaged copy:** the installers put a copy of the project's default config (`assets/unterm.conf`) inside the app bundle. The current code reads only the path above, so with no `~/.unterm/unterm.conf`, the **built-in** defaults in the tables below apply, not the values in that copy. Where the two differ, the tables give both.

## When changes take effect

The config is read once, when the process starts. Nothing watches the file, so edits apply on the next start.

- The GUI (`unterm`) reads it at startup for everything in this document.
- `unterm-core`, the background process that owns the terminal sessions and serves MCP, reads it at its own startup. It uses `scrollback_lines`, `shell`, `quick_select_alphabet`, `[window] initial_cols`/`initial_rows`, `color_scheme`, `[mcp]` and `[cockpit]`. The Core keeps running when you close the window, so those keys change only after the Core restarts. A window that reconnects to a running Core does not restart it.
- `[env]` and `path_append` are set on the GUI process before it starts or connects to the Core. A Core started by that window inherits them. A Core that was already running keeps the environment it started with.

## Errors and warnings

- **Parse errors** (a missing quote, an unquoted string, an unclosed list, a key set twice) make the **whole file** ignored. Unterm starts on built-in defaults and logs each error with its line number. A key set twice is an error, not "last one wins".
- **Unknown keys** do not stop the rest of the file from applying. Each one is logged as a warning with its line number, plus the closest real key when one is near enough (`unknown setting 'font_sze' -- did you mean 'font_size'?`). Names under `[env]` and `[keys]` are never treated as unknown.
- **Checked values:** `line_height` must be between 0.1 and 10, `window.background_opacity` between 0 and 1, `tab_bar.title_format` may only use the `{title}` and `{index}` placeholders, and every `hyperlink_rules` entry must have the right shape. A failed check is a warning. It does not stop the rest of the file from loading.
- **Wrong types:** outside those checks, a value of the wrong type (for example, `font_size = "big"`) is usually ignored without a warning, and the key keeps its default.

The GUI writes these messages to its log as `config line N: …`. `unterm-core` prints them to stderr as `unterm-core: config line N: …`.

## A small example

```ini
# ~/.unterm/unterm.conf
font_family = "JetBrains Mono"
font_fallback = ["PingFang SC", "Symbols Nerd Font Mono"]
font_size = 13
line_height = 1.15
scrollback_lines = 50000

[window]
initial_cols = 120
initial_rows = 30
padding_left = 12

[visual_bell]
fade_in_duration_ms = 75
fade_out_duration_ms = 150

[mcp]
input_confirmation = "first_time_per_agent"
trusted_agents = ["claude"]

[env]
EDITOR = "vim"

[keys]
CTRL|SHIFT+T = "NewTab"
F11 = "None"
```

## Syntax

The format is line-based `key = value`, with `[section]` headers. Parser: `unterm-engine/src/next_core/config.rs`.

**Sections.** `[name]` prefixes every key after it with `name.` until the next header. A header can contain dots, and so can a key, so these three spell the same key:

```ini
[colors.tab_bar.active_tab]
bg_color = "#2b2d31"

[colors]
tab_bar.active_tab.bg_color = "#2b2d31"

# (at the top of the file, before any header)
colors.tab_bar.active_tab.bg_color = "#2b2d31"
```

Only one of those can appear in a file, because setting a key twice is an error. Keys before the first header are top-level.

**Comments.** `#` starts a comment anywhere on a line, except inside a quoted string. `"#1e1e2e"` is a colour, not a comment. Blank lines are ignored.

**Values.**

| Type | Written as | Notes |
|---|---|---|
| Boolean | `true`, `false` | Lowercase only. |
| Integer | `13`, `-1`, `50000` | 64-bit signed. |
| Number | `1.15`, `0.5` | An integer is accepted where a number is expected (`font_size = 12` means 12.0). |
| String | `"text"` | Double quotes only. Escapes: `\n`, `\t`, `\r`, `\\`, `\"`. Any other backslash is kept as written, so `"C:\Users\me\pwsh.exe"` works without doubling. But `\n`, `\t`, `\r` are still escapes inside a path: `"C:\Program Files\nodejs"` contains a newline and `"C:\tools"` a tab. Double the backslash there (`"C:\Program Files\\nodejs"`) or use forward slashes. |
| List | `["a", "b", 3]` | Items can be of any type, including nested lists. A list may span several lines, with comments and a trailing comma. |

An unquoted word such as `font_family = Cascadia` is a parse error. The message suggests the quoted form.

**Platform sections.** The format defines `[platform.windows]`, `[platform.macos]`, `[platform.linux]` and `[platform.other]`. Keys inside them are meant to apply only on that platform: the base value first, then `[platform.other]` (used only when the file has no section for the current platform), then the named platform's section. `assets/unterm.conf` uses them for `title_button.*` and the Windows `path_append`.

**Caveat, current build:** the config loader (`unterm-services/src/settings.rs` `load()`) does not apply this platform logic. No production code calls `Config::resolve_platform`. A key such as `[platform.windows] path_append` is therefore stored as `platform.windows.path_append`, logged as an unknown setting, and has no effect on any platform. Until that changes, put platform-specific values in the main part of the file.

## Top-level keys

| Key | Type | Default | What it does |
|---|---|---|---|
| `font_family` | string | none (bundled JetBrains Mono) | The terminal font family, at its regular weight. If the family is not installed, a warning is logged and the default is used. If the bundled font cannot be opened, the system's default monospace font is used. |
| `font_fallback` | list of strings | `[]` | Families to try, in order, for characters the main font lacks (for example, CJK or Nerd Font icons). They are tried before the built-in fallback list. Non-string entries are skipped. The packaged copy lists PingFang SC, Microsoft YaHei, Noto Sans CJK SC, Noto Sans Mono CJK SC and Symbols Nerd Font Mono. |
| `font_size` | number | `13` | The font size in points. Values below 6 become 6. |
| `line_height` | number | `1.0` (packaged copy: `1.15`) | Multiplies the font's line height. The value used is clamped to 0.5–4.0. The checker warns outside 0.1–10. |
| `color_scheme` | string | none (packaged copy: `"Unterm Dark"`) | Recorded in settings and reported by MCP (`terminal.color_scheme`). It does not change any colours: those come from the theme (`~/.unterm/theme.json`, the theme picker) and `[colors]`. |
| `scrollback_lines` | integer | `scrollback.json`, otherwise `10000` (packaged copy: `50000`) | Scrollback lines kept for each pane created after startup. Accepted range: 0–999,999,999. If the key is missing, negative, too large or not an integer, the `lines` value in `~/.unterm/scrollback.json` is used, and if that is missing too, 10,000. Scrollback stays in memory, so large values on many panes use a lot of RAM. |
| `enable_scroll_bar` | bool | `true` | Shows the scroll bar beside each pane. |
| `text_blink_rate` | number (ms) | `500` | How fast text with the SGR 5 (slow blink) attribute blinks. `0` stops it blinking. Negative values count as 0. |
| `text_blink_rate_rapid` | number (ms) | `250` | The same for SGR 6 (rapid blink). |
| `hyperlink_rules` | list of rules | built-in rules | Rules that turn printed text into clickable links. Each rule is `["regex", "format"]` or `["regex", "format", capture]`. In the format, `$0` is the whole match and `$1` the first group. `capture` is the group drawn as the link (default 0). Setting this key replaces all built-in rules (bare and bracketed URLs, email addresses), and `[]` turns link detection off. A rule whose regex does not compile is skipped with a warning. |
| `quick_select_alphabet` | string | `"asdfqwerzxcvjklmiuopghtybn"` | Letters used for Quick Select labels (Ctrl+Shift+S). Converted to lowercase. It must have at least 8 characters, no whitespace and no repeats. Otherwise a warning is logged and the default is used. |
| `shell` | string or list of strings | platform default | The program new panes start. A string is one program path with no arguments. Use a list for arguments: `["pwsh.exe", "-NoLogo"]`. If unset, Windows uses PowerShell 7 (`pwsh.exe -NoLogo -NoProfile`) when it is found, otherwise `powershell.exe -NoLogo -NoProfile`. macOS and Linux use the login shell from the user database, or `/bin/sh`. The Web Settings "default shell" control writes this key. |
| `path_append` | list of strings | `[]` | Directories added to the end of `PATH` at startup, skipping ones already there (without regard to case on Windows). Shells, agent discovery and the Core started by this window all see the result. On Windows, Unterm also adds `C:\Program Files\nodejs`, `C:\Strawberry\perl\bin`, `%APPDATA%\npm` and `%USERPROFILE%\.bun\bin` when those directories exist, whether or not this key is set. |
| `shell_integration` | bool | `true` | Load Unterm's shell integration into the zsh, bash, fish and PowerShell sessions it starts, without touching your own startup files. It gives Unterm prompt and command marks (OSC 133) with exit codes, and the working directory (OSC 7). `false` starts shells exactly as configured. See [Shell integration](/docs/shell-integration). |
| `audible_bell` | string | on | Whether the bell also beeps. `"Disabled"` (any case) turns the beep off, and any other string leaves it on. The value must be a string: `audible_bell = false` is ignored, so the beep stays on. The beep is the Windows system sound (`MessageBeep`). On macOS and Linux nothing is played. |
| `status_bar` | bool | `false` | Shows a status strip below the terminal. Off by default. It is read once at startup so that the number of terminal rows does not change while running. |
| `window_background_image` | string (file path) | none | A picture drawn behind the terminal. It is scaled to cover the window and cropped from the centre. Its opacity comes from `[window] background_opacity`. A missing or unreadable file is logged and skipped. |
| `use_ime` | bool | — | Accepted but currently has no effect. The input method is always enabled for the window. |

## `[colors]`

Colours are hex strings: `"#rrggbb"`, `"#rgb"`, or the same without `#`. A value that is not valid hex is ignored.

| Key | Type | Default | What it does |
|---|---|---|---|
| `background` | colour | the theme's background (packaged copy: `"#111315"`) | The terminal background. It overrides the active theme's background even after you pick another theme. |
| `foreground` | colour | the theme's foreground (packaged copy: `"#e8eaed"`) | The default text colour. It also overrides the theme. |
| `tab_bar_lift` | number | — (packaged copy: `0.05`) | Accepted but currently has no effect. |
| `inactive_dim` | number | — (packaged copy: `0.35`) | Accepted but currently has no effect. |

## `[colors.tab_bar]` and its subsections

These colour the title bar and tab strip. They are applied on top of the computed chrome colours.

| Key | Type | Default | What it does |
|---|---|---|---|
| `[colors.tab_bar]` `background` | colour | computed | The bar surface (title bar, footer, group background). Used only when `[window_frame] active_titlebar_bg` is not set. |
| `[colors.tab_bar.active_tab]` `bg_color` | colour | computed | Background of the selected row or tab. |
| `[colors.tab_bar.active_tab]` `fg_color` | colour | theme foreground | The bar's text colour in a focused window. Used only when `[window_frame] active_titlebar_fg` is not set. Like all chrome text colours, it applies only while no theme has been picked (see `[window_frame]`). |
| `[colors.tab_bar.inactive_tab]` `bg_color` | colour | — | Accepted but currently has no effect. |
| `[colors.tab_bar.inactive_tab]` `fg_color` | colour | computed | Colour of dimmed (secondary) text in the bar. Falls back to `[window_frame] inactive_titlebar_fg`. |
| `[colors.tab_bar.inactive_tab_hover]` `bg_color` | colour | computed | The hover background in the bar. Falls back to `[window_frame] button_hover_bg`. |
| `[colors.tab_bar.inactive_tab_hover]` `fg_color` | colour | — | Accepted but currently has no effect. |
| `[colors.tab_bar.new_tab]` `bg_color` | colour | — | Accepted but currently has no effect. |
| `[colors.tab_bar.new_tab]` `fg_color` | colour | — | Accepted but currently has no effect. |
| `[colors.tab_bar.new_tab_hover]` `bg_color` | colour | — | Accepted but currently has no effect. |
| `[colors.tab_bar.new_tab_hover]` `fg_color` | colour | — | Accepted but currently has no effect. |

## `[window_frame]`

Title bar and window-button colours, as hex strings. These win over the matching `[colors.tab_bar]` keys. The foreground keys (`active_titlebar_fg`, `inactive_titlebar_fg`) apply only when no theme has been picked. Once a theme is chosen, the bar text uses the theme's foreground, at 70% alpha in an unfocused window.

| Key | Type | Default | What it does |
|---|---|---|---|
| `active_titlebar_bg` | colour | computed | Bar surface in the focused window. |
| `active_titlebar_fg` | colour | theme foreground | Bar text in the focused window. |
| `inactive_titlebar_bg` | colour | `active_titlebar_bg` | Bar surface in an unfocused window. |
| `inactive_titlebar_fg` | colour | `active_titlebar_fg` | Bar text in an unfocused window. It is also the fallback for dimmed text. |
| `active_titlebar_border_bottom` | colour | computed | The window's outer edge line when focused. |
| `inactive_titlebar_border_bottom` | colour | `active_titlebar_border_bottom` | The outer edge line when unfocused. |
| `button_bg` | colour | — | Accepted but currently has no effect. |
| `button_fg` | colour | style default | Glyph colour of the drawn minimise, maximise and close buttons. |
| `button_hover_bg` | colour | style default | Hover fill of the minimise and maximise buttons (not close). It is also the fallback hover background for the bar. |
| `button_hover_fg` | colour | `button_fg` | Button glyph colour on hover (not for the close button). |

## `[visual_bell]`

With both durations at 0 (the default), the screen does not flash when the bell rings.

| Key | Type | Default | What it does |
|---|---|---|---|
| `fade_in_duration_ms` | number (ms) | `0` | How long the flash takes to reach full strength. |
| `fade_out_duration_ms` | number (ms) | `0` | How long it takes to fade back. |
| `fade_in_function` | string | `"Ease"` | Easing curve for the fade-in: `Linear`, `Ease`, `EaseIn`, `EaseInOut`, `EaseOut` or `Constant` (any case). An unknown name means `Ease`. |
| `fade_out_function` | string | `"Ease"` | Easing curve for the fade-out. It takes the same names. |
| `target` | string | `"BackgroundColor"` | What flashes: `BackgroundColor` (the pane background) or `CursorColor` (the cursor cell). Any value other than `CursorColor` (any case) means `BackgroundColor`. |

## `[window]`

| Key | Type | Default | What it does |
|---|---|---|---|
| `background_opacity` | number 0–1 | `0.25` (packaged copy: `1.0`) | How strongly `window_background_image` shows. The value used is capped at 0.5 so text stays readable. It has no effect without a background image and does not make the window transparent. The older top-level spelling `window_background_opacity` is also read. |
| `backdrop` | string | off | `"mica"`, `"on"` or `"true"` (any case, written as a string) asks for the Windows 11 Mica backdrop. It applies only on Windows with DX12, with `decorations` off and no background image. It does nothing on other platforms. |
| `decorations` | bool | `false` | `true` uses the operating system's title bar and frame. `false` lets Unterm draw its own. The packaged copy sets the string `"INTEGRATED_BUTTONS\|RESIZE"`, which is not a boolean, so it is ignored and behaves as `false`. |
| `initial_cols` | integer | `80` (packaged copy: `120`) | Columns for the first window's terminal area. Values below 1 become 1. |
| `initial_rows` | integer | `24` (packaged copy: `30`) | Rows for the first window. Values below 1 become 1. |
| `padding_left` | number (logical px) | `12` | Space between the window edge and the text. Negative values become 0. |
| `padding_right` | number | `12` | Same, right side. |
| `padding_top` | number | `8` | Same, top. |
| `padding_bottom` | number | `8` | Same, bottom. |
| `close_confirmation` | string | — (packaged copy: `"NeverPrompt"`) | Accepted but currently has no effect. The code reads the top-level key `window_close_confirmation` instead (see [Keys read outside the schema](#keys-read-outside-the-schema)). The unterm.lua conversion writes this key, so a converted setting also has no effect. |

## `[inactive_pane]`

Multipliers applied to the colours of panes that do not have focus, in hue, saturation and brightness (HSB). At `1.0` for all three (the default), inactive panes are not changed. Negative values become 0.

| Key | Type | Default | What it does |
|---|---|---|---|
| `hue` | number | `1.0` | Multiplies the hue. The result wraps around the colour wheel. |
| `saturation` | number | `1.0` | Multiplies the saturation. |
| `brightness` | number | `1.0` | Multiplies the brightness. For example, `0.6` dims unfocused panes. |

## `[tab_bar]`

None of these keys is read by the current front end. Tab titles come from fixed rules: a name you gave the tab, otherwise the pane's title or foreground program with `.exe` removed, otherwise `shell`. The keys are kept so that configs converted from unterm.lua do not produce warnings.

| Key | Type | Default | What it does |
|---|---|---|---|
| `position` | string | — (packaged copy: `"Left"`) | Accepted but currently has no effect. |
| `max_width` | integer | — (packaged copy: `32`) | Accepted but currently has no effect. |
| `hide_if_only_one_tab` | bool | — | Accepted but currently has no effect. |
| `show_index` | bool | — | Accepted but currently has no effect. |
| `show_new_tab_button` | bool | — | Accepted but currently has no effect. |
| `title_format` | string | — (packaged copy: `"  {title}  "`) | Accepted but currently has no effect. It is still checked: placeholders other than `{title}` and `{index}` cause a warning. |
| `fallback_title` | string | — | Accepted but currently has no effect. |
| `strip_extension` | bool | — | Accepted but currently has no effect. |
| `capitalize` | bool | — | Accepted but currently has no effect. |

## `[stats]`

| Key | Type | Default | What it does |
|---|---|---|---|
| `refresh_ms` | integer (ms) | `1000` | How often the facts line in the top bar is refreshed: bound agent, git branch, CPU and memory, and the running command. Clamped to 250–60,000. It must be an integer: `1000.0` is ignored. GUI only. |

## `[mcp]`

These control how agents connected over MCP may drive the terminal. They are read by the process that serves MCP, which is normally `unterm-core`.

| Key | Type | Default | What it does |
|---|---|---|---|
| `input_confirmation` | string | `"first_time_per_agent"` | When an agent writing to a pane needs your approval in a banner. `"never"`: no banner. `"always"`: a banner for every write. `"first_time_per_agent"`: ask the first time each agent writes, then remember. Any other value keeps `first_time_per_agent`. `"never"` also skips approval of destructive MCP actions. Writes made by `unterm-cli` itself never raise a banner. |
| `trusted_agents` | string or list of strings | `[]` | Agent names that never need confirmation, for pane writes or for destructive actions. |
| `confirmation_timeout_ms` | integer (ms) | `30000` | How long a confirmation banner waits before it counts as a denial. Values below 1000 are raised to 1000. `unterm-cli` also uses this value to wait for a confirmed call. |
| `suggest_default_ttl_ms` | integer (ms) | `300000` | How long a suggestion an agent posts stays valid when the agent does not give its own `ttl_ms`. |
| `suggest_queue_capacity` | integer | `32` | Most pending suggestions kept. The oldest are dropped first. Values below 8 are raised to 8. |
| `audit_log_capacity` | integer | `1000` | Entries kept in the in-memory MCP audit log, including entries reloaded from disk at startup. Values below 16 are raised to 16. |

## `[cockpit]`

The agent cockpit tracks which agent runs in which pane and what it is doing. Its inbox opens with Ctrl+Shift+A.

| Key | Type | Default | What it does |
|---|---|---|---|
| `enabled` | bool | `true` | Turns agent status tracking on or off. When it is off, the MCP `agent.status` call returns `{"enabled": false, "agents": []}`. |
| `auto_checkpoint` | bool | `true` | Takes a git snapshot of the pane's repository when an agent starts working, so you can review what it changed. |
| `done_hold_secs` | integer (s) | `20` | How long an agent stays "done" before it returns to "idle". |

## `[title_button]`

None of these keys is read by the current front end. The packaged copy sets them in `[platform.*]` sections, which the current build does not apply either (see [Syntax](#syntax)).

| Key | Type | Default | What it does |
|---|---|---|---|
| `style` | string | — | Accepted but currently has no effect. |
| `alignment` | string | — | Accepted but currently has no effect. |
| `buttons` | list of strings | — | Accepted but currently has no effect. |

## `[env]`

Every key in `[env]` is an environment variable name, and its value must be a string.

```ini
[env]
EDITOR = "vim"
LANG = "en_US.UTF-8"
```

At startup the GUI sets each variable on its own process. New panes inherit them, along with agents launched from the terminal and a Core started by this window. A non-string value (`COUNT = 3`) is not exported and is logged with its line number. Values are used literally: `$HOME` and `%USERPROFILE%` are not expanded. Any variable name is accepted, and names under `[env]` never produce unknown-setting warnings.

## `[keys]`

`[keys]` changes key bindings. Each line is `chord = "Action"`:

```ini
[keys]
CTRL|SHIFT+T = "NewTab"          # replace (here: restate) a built-in binding
CTRL|ALT+Left = "FocusPaneLeft"  # add a new chord
ALT+Equal = "IncreaseFontSize"   # `=` cannot be written directly
F11 = "None"                     # unbind: F11 goes to the shell
```

**Chord syntax.** Modifier names, then the key, separated by `|` or `+` (`CTRL|SHIFT+T` and `CTRL+SHIFT+T` are the same). Names are case-insensitive.

- **Modifiers:** `CTRL` (or `CONTROL`), `SHIFT`, `ALT` (or `OPT`, `META`), and `NONE`, which means no modifier. Any other name is an error. Only Ctrl, Shift and Alt are checked when a key is pressed. There is no Cmd, Super or Win modifier, and on macOS `CTRL` means the Control key, not Command.
- **Keys:** any single character (letters match either case, so `CTRL+t` = `CTRL+T`), `Left`, `Right`, `Up`, `Down`, `PageUp`, `PageDown`, `Tab`, `Space`, `F1`–`F12`, and `Equal` (or `Equals`), `Plus`, `Minus` for `=`, `+` and `-`. Those three need names because `=` ends the key and `+` separates modifiers. Other key names (Enter, Escape, Home, F13 and so on) are errors.
- **Matching is exact.** A binding for `CTRL+T` does not fire for Ctrl+Shift+T.

**Values.** The value is an action name (case-insensitive), or `"None"` to unbind.

- A chord that has a built-in binding is replaced by your entry.
- A new chord adds a binding.
- `"None"` removes the binding, so the keystroke goes to the program in the pane.

Bindings from `[keys]` are checked before the built-in ones. The command palette's shortcut hints and the MCP `meta.keybindings` reply show the merged result. A bad chord, an unknown action or a non-string value is logged with its line number and skipped. The other entries still apply. Two spellings of the same chord (`CTRL+T` and `ctrl+t`) are different keys to the parser, so both load. Avoid that.

**Actions.**

| Action | Built-in chord |
|---|---|
| `Copy` | Ctrl+Shift+C |
| `Paste` | Ctrl+Shift+V |
| `SplitRight` | Ctrl+Shift+D |
| `SplitDown` | Ctrl+Shift+E |
| `ScrollPageUp` / `ScrollPageDown` | Shift+PageUp / Shift+PageDown |
| `PreviousPrompt` / `NextPrompt` | Ctrl+Shift+Up / Ctrl+Shift+Down |
| `NewTab` | Ctrl+Shift+T |
| `NextTab` / `PreviousTab` | Ctrl+Tab / Ctrl+Shift+Tab |
| `CloseTab` | Ctrl+Shift+W |
| `Search` | Ctrl+Shift+F |
| `CommandPalette` | Ctrl+Shift+P |
| `Launcher` | Ctrl+Shift+L and Ctrl+Shift+N |
| `CopyMode` | Ctrl+Shift+X |
| `QuickSelect` | Ctrl+Shift+S |
| `Insights` | Ctrl+Shift+I |
| `CockpitInbox` | Ctrl+Shift+A |
| `GitPanel` | Ctrl+Shift+G |
| `Composer` | Ctrl+Shift+J |
| `ThemePicker` | — |
| `LeftTabBar` | — |
| `DirJump` | — |
| `NewWindow` | Ctrl+Shift+Alt+N |
| `NewAdminWindow` | — (Windows only) |
| `ClosePane` | Ctrl+Shift+Q |
| `ZoomPane` | Ctrl+Shift+Z |
| `Settings` | — |
| `CharSelect` | Ctrl+Shift+U |
| `TreeSidebar` | Ctrl+Shift+B |
| `FleetLaunch` | Ctrl+Shift+Alt+A |
| `ClearScrollback` | Ctrl+Shift+K |
| `ClearScreen` | Ctrl+Shift+Alt+K |
| `SelectPane` | Ctrl+Shift+' |
| `SwapPane` | Ctrl+Shift+Alt+' |
| `FocusPaneLeft` / `Right` / `Up` / `Down` | Alt+arrow |
| `ResizePaneLeft` / `Right` / `Up` / `Down` | Shift+Alt+arrow |
| `MoveTabLeft` / `MoveTabRight` | Ctrl+Shift+PageUp / Ctrl+Shift+PageDown |
| `SelectTab1` … `SelectTab9` | Ctrl+1 … Ctrl+9 (`SelectTab9` goes to the last tab) |
| `IncreaseFontSize` | Ctrl+= and Ctrl++ |
| `DecreaseFontSize` | Ctrl+- |
| `ResetFontSize` | Ctrl+0 |
| `ToggleFullScreen` | F11 |

Plain Ctrl+C is not bound. It always reaches the shell as an interrupt.

## Keys read outside the schema

The code also reads the keys below, but the schema does not list them. They work, and each one also logs an "unknown setting" warning.

| Key | Type | Default | What it does |
|---|---|---|---|
| `theme` | string | the theme picked in the picker | Theme id for terminal colours. It takes priority over the theme picker. |
| `font` | string | — | An early spelling of `font_family`, used only when `font_family` is not set. |
| `cell_width` | number | `1.0` | Multiplies the cell width. Clamped to 0.5–4.0. |
| `default_cursor_style` | string | block, steady | `SteadyBlock`, `BlinkingBlock`, `SteadyUnderline`, `BlinkingUnderline`, `SteadyBar` or `BlinkingBar`. Anything else means a steady block. |
| `cursor_blink_rate` | number (ms) | `800` | Blink speed of a blinking cursor. `0` turns blinking off. |
| `default_cwd` | string (path) | none | Directory a new session starts in when no `--cwd` (or restored directory) is given. |
| `window_close_confirmation` | string | asks | `"NeverPrompt"` (any case) closes without asking. Any other value keeps the prompt shown when closing would end tabs or running programs. |
| `window_background_opacity` | number | — | Older spelling of `[window] background_opacity`, used only when that key is not set. |
| `top_bar` | string | compact | `"full"` uses the full-height top bar. Any other value, or no value, keeps the compact one. |

## Migrating from unterm.lua

Releases before the declarative config read a Lua file. On startup, the GUI converts that file once, when **all** of these are true:

1. `~/.unterm/unterm.conf` does not exist. An existing `unterm.conf` is never overwritten.
2. One of these files exists. The first match is used:
   - `~/.config/unterm/unterm.lua`
   - `~/.unterm/unterm.lua` (inside the state directory)
   - `~/.unterm.lua`
   - `~/.wezterm.lua`

The converted text is written to `~/.unterm/unterm.conf`. The Lua file is not changed. Each line that could not be converted is logged as `not converted: …` with its line number and the reason. `unterm-core` does not convert. Only the GUI does.

**What converts** (`unterm-engine/src/next_core/config_migrate.rs`):

- Plain assignments of booleans, numbers, strings and flat lists, written as `config.x = …`, `config['x'] = …`, `M.x = …` or a bare `x = …` inside the returned table.
- Nested tables opened on their own line become sections (`colors = {` → `[colors]`).
- `wezterm.font("Name")` with a single literal name becomes `font_family`.
- One-line `window_padding = { left = …, … }` becomes `[window] padding_left` and the other sides.
- One-line `inactive_pane_hsb = { … }` becomes `[inactive_pane] hue`, `saturation` and `brightness`.
- `if`/`elseif`/`else` branches on `wezterm.target_triple` become `[platform.macos]`, `[platform.windows]`, `[platform.linux]` and `[platform.other]`. As noted under [Syntax](#syntax), the current build does not apply those sections. Move the values you need out of them by hand.
- Old names are renamed: `initial_cols` and `initial_rows` → `[window]`; `window_close_confirmation` → `[window] close_confirmation`; `window_background_opacity` → `[window] background_opacity`; `window_decorations` → `[window] decorations`; `tab_bar_position`, `tab_max_width`, `hide_tab_bar_if_only_one_tab`, `show_tab_index_in_tab_bar` and `show_new_tab_button_in_tab_bar` → `[tab_bar]`; the `integrated_title_button_*` keys → `[title_button]`; `status_update_interval` → `[stats] refresh_ms`.
- `check_for_updates`, `win32_system_backdrop`, `show_unterm_status_bar` and `use_fancy_tab_bar` are dropped without a report, because the new front end has fixed behaviour for them.

**What is reported instead of converted:** functions (including key bindings and event handlers written as Lua callbacks), anything inside other `if`/`for`/`do` blocks, `local` variables, calls into `wezterm.*` or `require`, strings built with `..`, tables with named fields written on one line, and a key that ends up set twice (for example, by two platform branches). Re-create these by hand, using `[keys]` for bindings and `[env]` for `set_environment_variables`.

After the conversion, check the log for unknown-setting warnings. Some converted keys, such as `[window] close_confirmation` and the `[tab_bar]` keys, are accepted but currently have no effect, as the tables above note.
