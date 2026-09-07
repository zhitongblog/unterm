# Changelog

## v0.71.4 — 2026-09-07

### Fixed

- **Every glyph only a colour font carries was drawn as its own negative.**
  Claude Code opens each message with `⏺`, and it rendered as a filled block
  with a round hole punched out of it — the photographic negative of a filled
  disc. The character exists in no text font: not JetBrains Mono, FiraCode,
  Menlo, SF Mono or Monaco, only in Noto Color Emoji and Apple Color Emoji.
  So it always came from a colour face, and the rasterizer asked every face
  for monochrome first, retrying in colour only when monochrome drew *nothing*.
  A colour strike loaded without `FT_LOAD_COLOR` neither fails nor comes back
  empty: FreeType flattens it to 8-bit grey, and the grey of a black disc on a
  transparent ground is that disc inverted. Non-empty, so the colour retry was
  never reached. Colour faces are now asked for colour first.

- **A window that could not reach the Core said so only to stderr.** The
  fallback is right — a user whose Core will not come up still gets a terminal
  — but it costs the one thing the Core is for: sessions no longer outlive the
  window, and the MCP surface it serves goes with them. That was announced on
  stderr, which for a window opened from the Dock or the Start menu is nowhere
  at all, so a front end could run for days in an arrangement its user never
  chose. The status bar now leads with a `core:local` chip while it lasts, and
  a press on it gives the handshake's own words, which name the Core and its
  version.

### Changed

- **Risk has seven bands where it had three.** `read`, `local_mutation`,
  `exec`, `external_side_effect`, `credential_access`, `financial`,
  `destructive` — the vocabulary `contracts/v0` defines. The missing one that
  mattered most was `exec`: running a command had to be filed either as a
  local mutation, which is far too loose, or as destruction, which is far too
  tight, and typing into a shell was classified as the same kind of act as
  renaming a tab. Reaching a secret and sending bytes off the machine were
  likewise unsayable, so `profile.*`, `upload.file`, `brain.fetch` and the
  browser capability all travelled under whichever of the three fitted worst.

- **The gate moved with the bands.** Only `destructive` used to be asked
  about; everything else went through in silence, which meant handing an agent
  a token was as quiet as resizing a pane. Anything from
  `external_side_effect` upward now needs a person to say yes. `exec` and
  below stay silent: a terminal runs commands all day, and a prompt on every
  keystroke is a prompt nobody reads.

- **Secrets, money and destruction are answered one call at a time.** They no
  longer accept a permission that outlives the call — only a single-use grant
  naming that very method. Narrowed where grants are *written*, not only where
  they are matched: a grant that is stored, listed, and can never answer is
  worse than no grant, because the user was told they gave one. "Allow for
  this task" on a deletion becomes "allow once", and "always allow" on an
  agent banner is capped at the highest band a standing permission may carry.

- **Brain events carry the contract's names.** `tool_call.requested`,
  `tool_call.result`, `usage.updated`, `turn.completed`, `session.closed`,
  each with its own `schema_version` and the `cursor` it sits at, so an event
  passed on by itself is still locatable. `output.delta` deliberately keeps
  its `assistant`/`reasoning` discriminator rather than splitting reasoning
  into an event of its own: rendering a transcript in order needs the two
  interleaved, and two names make that the reader's problem.

- **Failures come back as `{code, message, retryable}`.** A caller may branch
  on `code` and must never parse `message`. Fifteen codes, each under one of
  the ten categories the contract names, with the list closed and a test that
  says so.

## v0.71.3 — 2026-09-02

### Fixed

- **Quitting stopped the Core, then started another one.** The fix in v0.71.2
  worked; what it could not survive was the window's own reconnection. A front
  end that loses its connection to the Core cannot tell a crash from a
  deliberate shutdown, and it heals a crash the only way it knows — by
  starting a replacement. So a quit read, in the log, as two lines in a row:
  *stopped the unterm-core this process started*, and then *unterm-core
  replaced (now pid N)*. The window went away and left a Core behind anyway.

  Found on a Linux desktop by closing the last window and looking at what was
  still running. The orphan is why an install on Windows could still find
  `unterm-core.exe` in use: it was started *during* the quit, after the
  installer's own close pass had already run.

  Quitting now says so before it stops anything, and both recovery paths —
  the request retry and the frame worker's event feed — leave a Core that is
  gone during a shutdown gone.

## v0.71.2 — 2026-09-01

### Fixed

- **Quitting left the Core running.** Closing a window is not meant to end
  the shells behind it, so the Core is started detached — but nothing ever
  stopped it when the application itself went away, so every quit left one
  behind. On macOS they piled up unseen; on Windows one holds
  `unterm-core.exe` open, which is why the installer asked you to close a
  program that has no window to close.

  Quitting now stops the Core it started. Only the one it started: a Core
  that was already running belongs to whoever started it, including a
  `--headless` one you launched on purpose, and one still serving another
  window is left alone.

- **Opening Unterm twice made two of everything.** `unterm start --cwd`
  became a whole second process — a GPU adapter each time, and a second Core
  to be left behind later — because the call that hands a window to the
  running front end took no arguments, so handing over would have lost the
  directory. The guard was right about that; what was wrong was treating it
  as a trade rather than fixing the call. The request now carries the
  directory, profile and command, so every `start` hands over.

  One consequence worth knowing: with a front end already running, `start`
  no longer creates a second process. Windows share one, as the
  single-process design has intended since v0.68.2 — faster, and no stray
  Core, at the cost of one window's crash taking the others.

- **`unterm-core.exe` reported version 0.1.0.** It carried the crate's
  version rather than the product's. Worse than the blank it replaced: a
  blank is honest, and 0.1.0 looks like an answer.

## v0.71.1 — 2026-08-30

### Fixed

- **Installing over a running Unterm asked you to close something you could
  not see.** An install is interactive, so a file still held open makes
  Restart Manager stop and say "the following applications should be closed".
  The installer declared nothing about its own processes, so clearing that was
  left to the user — and could not be done: `unterm-core.exe` is started
  detached, on purpose, so that closing a window does not end the shells
  behind it, and `unterm-cli.exe` is the MCP bridge an editor holds open for
  an agent. Neither has a window. Quitting Unterm left both running, the
  installer still saw a locked file, and the install failed again.

  The installer now closes all three itself before it copies anything.
  Verified on a Windows 11 ARM machine: with the Core and the bridge running,
  the install log shows the close action completing before the first file
  copy, both processes gone afterwards, exit code 0.

  This is the installer half. The reason those processes are still there when
  you quit is a separate fix, still to come: quitting should take the Core
  with it.

- **`unterm-core.exe` and `unterm-cli.exe` shipped with no version number.**
  Only the front end embedded a version resource, so Explorer's properties
  page and any support tooling reported nothing for the other two. Not the
  cause of the install failure above — `REINSTALLMODE=amus` already forces
  every file onto disk regardless of version — but it is why nobody could
  tell which build was installed by looking.

### Added

- **The settings app can host the Unzoo One console**, reachable from the
  menu, and the installer can bundle it. Merged from `feat/console-hosting`.

## v0.71 — 2026-08-28

### Fixed

- **Anything that shifted a row sideways could tear a wide character in
  half.** The v0.70 fix covered writes and erases; it did not cover the
  operations that *move* cells. `ESC[@` and `ESC[P` — which zsh's line editor
  uses constantly when you type or delete inside a line — plus insert mode and
  `SL`/`SR` all shift, and a shift leaves the orphaned half in the *middle* of
  the row rather than at its edge, where the earlier point-checks were looking.

  Every one of those now repairs the rewritten span afterwards, reaching one
  column past each end because a pair can straddle a boundary, and working
  left to right because blanking a wide cell shrinks it and the continuation
  behind it has to be judged against what the cell has become. The surviving
  half keeps its own colours: nothing was written over it, its neighbour was
  merely carried away.

  This also closed a hole in the v0.70 fix itself — a continuation cell at
  column one owns nothing, and the backward search for its owner used to
  bottom out on the cell itself and quietly do nothing.

- **`SL` and `SR` moved one row instead of the whole scroll region.** What
  shipped as "scroll left/right" was `ESC[P`/`ESC[@` applied to the cursor's
  row, with the left edge taken from the cursor rather than the margin.
  ECMA-48 gives the cursor no part in either, and both act on every row
  between the top and bottom margins. On any full-screen application that
  drew a box, scrolling tore the frame open along one line.

  Two existing tests had pinned the old behaviour and have been rewritten
  against the specification; a multi-row test now covers the case that would
  have caught this. `DECIC`/`DECDC` — the genuinely cursor-relative column
  insert and delete, and the likely source of the original confusion — remain
  unimplemented and are silently dropped.

- **A cycle in the process tree could hang or crash a directory lookup.**
  Windows reuses process ids and does not clear a dead parent's id from its
  children, so the parent/child graph can contain a loop; the walk that built
  the tree had no visited set and would follow it until the stack ran out. The
  same unguarded walk had been written three times, once per platform. It is
  now one walk with a visited set — an edge into a process already in the tree
  is dropped — plus a depth ceiling, since a long chain costs stack even
  without a cycle.

### Changed

- **Writing an `a` no longer consults the Unicode width tables, and a cell no
  longer costs 80 bytes.** Profiling the output path after v0.70 put
  `unicode_column_width` at the top: it ran for every character written,
  almost always to rediscover that a plain ASCII letter occupies one column.
  Printable ASCII now short-circuits, cross-checked against the real tables by
  a test that walks every code point it claims.

  `ScreenCell` went from 80 bytes to 48 — the combining marks it almost never
  carries moved behind a pointer, and two fields were sized to the values they
  actually hold. A pane with a full 10,000-line scrollback now peaks at 24.9 MB
  where it used to reach 35.9, a 31% reduction, and every copy of a cell got
  narrower.

  Parser throughput measured 1.52× on an in-process flood. End-to-end wall
  clock is unchanged, and that is worth stating plainly: since v0.70 removed
  the scrollback's copying, that benchmark spends more than half its time
  blocked reading the pty, so it no longer measures the parser. The gains here
  are processor time and memory, not seconds on that number.

## v0.70.1 — 2026-08-27

### Fixed

- **Windows: reading a pane's directory walked the whole process tree, and
  could exhaust the stack doing it.** v0.70's directory fix re-read the cwd on
  every snapshot, and the call it used built a `LocalProcessInfo` for every
  process on the machine and then recursed over it three times. On Windows,
  where the main thread gets a 1 MiB stack — and where a recycled parent PID
  can make the parent/child graph contain a cycle, which no amount of stack
  survives — that overflowed. It ran on every sidebar refresh.

  The directory now comes from `current_working_dir(pid)`, which asks the one
  process it cares about and does not build or walk a tree at all. It is also
  the more honest answer: a pane's directory is its shell's, and a
  long-running foreground program does not move the shell.

  The recursive tree walk still backs the activity/agent-detection views,
  where it runs far less often; its missing cycle guard is a known gap.

## v0.70 — 2026-08-26

### Fixed

- **A pane opened in a folder showed that folder for the rest of its life.**
  `cd` somewhere else and the sidebar kept naming the directory the pane was
  opened in — jump from `mv` into `story` and the strip still said `mv`.

  Two causes on top of each other. A shell reports where it is with OSC 7,
  but macOS only installs zsh's hook for that by sourcing
  `/etc/zshrc_$TERM_PROGRAM`, and a GUI launched from Finder inherits no
  `TERM_PROGRAM` — the same empty-environment trap that left `TERM` unset and
  turned colour off in v0.69. So that escape never arrives in Unterm, and the
  process-tree walk behind it is not a rare fallback but the only source
  there is. It was guarded by `if cwd.is_none()`: it ran only while nothing
  had been recorded yet. A pane opened *in* a directory starts with one
  recorded, so for those panes it never ran at all.

  The directory is now re-read on every query. A failed read keeps the last
  known answer rather than blanking the label.

- **A TUI that repaints one character at a time could scramble a line of
  Chinese.** Claude Code and Codex redraw by jumping to an absolute column
  and rewriting a single cell — `ESC[3G` then one glyph — which in a
  CJK-heavy interface lands on half of a wide character constantly. Unterm
  wrote the new character and left the other half standing, so the row kept
  either a continuation cell that owned no character, or a two-column glyph
  still painting over the character that had replaced it. Both read on
  screen as characters in the wrong place, and only while an agent was
  redrawing — which is why it looked intermittent.

  Overwriting either half of a wide cell now releases the other, and the
  same rule covers the two erase primitives these interfaces use alongside
  it, `ESC[K` and `ESC[nX`. The half that survives keeps its own colours:
  nothing was written to it. `ESC[@` and `ESC[P`, which shift cells rather
  than overwrite them, can still separate a pair and are not covered here.

- **Every throughput benchmark had been passing without running.** The
  benchmark workloads were written in cmd.exe syntax (`for /L %i in …`), so
  away from Windows the shell rejected the command, the completion marker
  landed in the same millisecond, and the report recorded a pass for work
  that had never happened; three further benchmarks failed outright. macOS
  and Linux therefore had no throughput measurement at all. The workloads
  now come in both dialects — the Windows text byte-for-byte unchanged, so
  its recorded numbers stay comparable — and `bench-next-core.ps1` runs on
  all three platforms from the one definition.

### Changed

- **The default theme's text now stands apart from an agent's grey.** Claude
  Code and Codex mark their hints, token counts and status lines with
  `#999999`. Against Agent Inbox's old `#d6d3cc` foreground that is a
  contrast ratio of 1.91 — under the 3:1 floor where a difference reads at a
  glance, which is why the two were easy to confuse even after colour
  started working in v0.69. It was also the dimmest foreground of any theme
  shipped here, its siblings sitting between 2.28 and 2.46. The same warm
  white now sits at `#f2efe7`, a ratio of 2.48, without going to a clinical
  pure white.

  Unterm reproduces `#999999` accurately — that was measured off the
  rendered pixels, not assumed — so this widens the gap from our side rather
  than rewriting anybody's colours. Worth knowing for anyone tuning
  `colors.foreground` themselves: the macOS coverage curve that gives text
  its native weight also lays down about 19% more ink than the raw glyph
  outline, which makes dim text read bolder than it does elsewhere.

- **A terminal used to get slower the longer it ran.** The scrollback was a
  list that dropped its oldest line by shifting every remaining line up one
  place. At the default 10,000-line limit that is roughly a quarter of a
  megabyte of memory moved for every single line of output — paid forever
  once the buffer fills, which for a build log takes seconds. It accounted
  for 83% of the time the PTY reader thread spent doing anything.

  The scrollback is now a deque, where both ends are free. The row evicted
  from the top is handed straight back as the new blank row at the bottom,
  so a scrolling terminal in steady state stops allocating rows altogether.
  And the raw-output buffer no longer copies itself on every chunk to stay
  under its limit: at the chunk sizes an interactive session produces that
  alone went from 36 µs to 0.04 µs per chunk.

  Half a million lines of output went from 5.5 seconds to 1.0 — 89,000
  lines per second to 497,000, measured on macOS.

## v0.69.1 — 2026-08-24

### Fixed

- **A pane could be resized down to one column, and one column is not a
  narrow terminal — it is a destroyed one.** A codex pane was found
  showing three characters, `=`, `[`, `[`: the first character of each of
  its three lines. Resizing a screen narrower truncates its lines and its
  scrollback in place rather than reflowing them, which is the right
  answer for a window the user is dragging narrower and a catastrophe for
  a width nobody asked for — and one column can only ever come from the
  latter. Every fallback along the way said `.max(1)`, so a window
  reporting no size at all, chrome measured wider than its window, or an
  agent passing `cols: 1` all arrived at the pane as a single column.

  Four paths closed at once: a `Resized(0,0)` event is now ignored rather
  than clamped to one pixel; a window still reporting 0×0 at startup is
  not used to compute a grid, and a session being adopted is not resized
  at all; the grid floor rises from one cell to the engine's minimum, so
  when the sidebar, file tree and git panel together outgrow the window
  it is the chrome that loses room rather than the pane's contents; and
  the engine now refuses any resize below `MIN_SESSION_GRID`, a floor the
  front end and the engine finally share — the same gate covers the GUI,
  Core IPC, and MCP `session.resize`.

## v0.69 — 2026-08-23

Colour. Unterm never had any.

### Fixed

- **The terminal never told programs what it was, so they turned colour
  off.** A GUI launched from Finder or launchd inherits no environment,
  and `TERM` is the one variable no parent will ever supply — announcing
  it is the terminal's own job. We never did. Every pane therefore told
  the programs inside it "I am a blank terminal", and they believed it:
  Node reported a colour depth of 1, so Claude Code and Codex switched
  colour off entirely. Their dim hints, their status lines and the text
  you were typing all arrived in one undifferentiated foreground —
  measurably so, at RGB (235,235,233) for every one of them, with the
  highlighted half and the dimmed half of the same line identical and
  even a `failed` rendered in no red at all. Since dimming its own
  chatter is the only way an agent distinguishes its notices from what
  you just typed, that distinction was gone. `tput` failed outright and
  ncurses programs fell back to their dumbest mode. The palette a theme
  defines had never once been asked for. Panes now announce
  `TERM=xterm-256color` and `COLORTERM=truecolor` — anything already
  present still wins.

- **A pane an agent opened announced itself differently from one you
  opened.** There are two paths to a new pane, and only one of them had
  learned to answer: the MCP surface built its own command and hand-set
  `TERM`, having never heard of `COLORTERM`. On macOS the value is
  usually inherited from the parent and the split stayed invisible; a
  Linux VM, where nothing supplies it, showed the two kinds of pane
  disagreeing outright. Both paths now share one definition.

### Verified

macOS and Ubuntu 24.04 (aarch64), by measuring rendered pixels rather
than trusting that the escape codes were sent: red, green and blue each
render as themselves, and dim sits 26–29 luminance below white on both.
**Windows was not verified** — the test VM needs more memory than the
machine could spare. The code carries no platform branch, so the same
logic Linux proves is what Windows runs; the open question there is not
whether colour works but whether announcing `TERM` changes the
behaviour of MSYS/Git-bash-flavoured tools.

### Internal

- Two CI gates had stopped guarding anything since 0.68. The MCP host
  gate ran `--bin unterm`, which held zero tests after `main.rs` was
  split into `lib.rs` — it was the strict "exactly 4 passed" assertion
  that caught this, where a laxer "at least one" would have gone on
  passing while testing nothing. The GUI pane gate's counts had not
  followed the single-process multi-window work, leaving it red since
  0.68.2.

## v0.68.3 — 2026-08-23

### Fixed

- **A headless Core could not say who it was.** `unterm-core --headless`
  runs, listens, and creates sessions with no window in sight — but
  `instance.list` answered with an empty list and `instance.info` with an
  empty record, `pid_alive: false` and all. An agent was told the Unterm it
  was talking to did not exist, and `unterm-cli instance list` said "No live
  Unterm instances" while the same CLI's `session list` was plainly connected
  to it. Registration is a front end's job — it writes `~/.unterm/instances`
  — and since 0.68 this surface lives in the Core, which publishes itself
  elsewhere. The two records are now joined: with a window open the Core is
  already on the list under that window's name, and with no window it is
  listed as `core`. `unterm-cli --instance core` reaches it.

- **The MCP surface looked for the Core's record in the wrong directory.**
  `state_path("core.json")` is `~/.unterm`; the Core writes to its platform
  data directory. It had never found the record once — invisibly, because the
  caller falls back to this process's own identity, which is right for a Core
  and wrong for anything asking about a different one.

## v0.68.2 — 2026-08-23

Windows are no longer processes. Opening a second one used to launch a
second Unterm — a second GPU adapter, a second Core connection, 587 ms of
waiting. It now opens in the process that is already running, in 31 ms, and
an agent can tell one window from another.

### Added

- **Windows have ids.** `instance.new_window` answers with the id of the
  window it opened, `instance.focus` takes one, and `instance.windows` lists
  every window this front end is showing — with its title, whether it has
  the focus, and which sessions are in it. Naming a window that is not there
  is an error rather than a silent redirect to whichever window was most
  recent. `instance.list` carries the same list for the current instance.

- **`session.focus` raises the window holding the session.** It used to set
  the active session and stop, and the front end only follows the active
  session in the window that is in front — so an agent saying "look at this"
  about a session in the other window changed a flag and the user saw
  nothing. When the platform refuses the foreground, the taskbar is asked
  for attention instead of the request vanishing.

### Changed

- **A second launch hands over to the running front end** instead of
  becoming a second process, and the whole multi-window exit story follows
  the platform: the last window closing ends the process on Windows and
  Linux, while on macOS the application outlives its windows.

- **The default scrollback is 50,000 lines**, down from 100,000.

### Fixed

- **A session belongs to one window.** Every window mirrored the Core's
  whole session list into its own tab bar, so a second window showed the
  first one's shells as well as its own — its title bar said `[1/2]` before
  anything had been run in it — and the two drifted towards the same set.
  A pane now belongs to the window that adopted it; a split belongs beside
  its source, in that source's window, even when the user is looking at a
  different one; and a session no window holds is taken by the window the
  user is in, which is where a session created over MCP lands and where the
  shells of a closed window resurface.

- **A background window keeps up.** Only the window being drawn mirrored the
  Core, so a pane an agent split or closed in a background window did not
  appear or disappear until the user happened to go back to it.

- **Closing one window ends only its own panes.**

- **A dead instance record no longer locks up the CLI.**

- **Audit appends re-read the whole day's log.** The 0.68 hash chain needed
  the previous entry's hash and got it by parsing every entry written that
  day. 0.09 ms and flat, rather than climbing past 3.75 ms.

## v0.68.1 — 2026-08-19

Two fixes for the same complaint: several agent tabs open, content stops
appearing, then the terminal stops responding. Both are 0.68 regressions.

### Fixed

- **Every audit line re-read the whole day's log.** The hash chain added in
  0.68 needs the previous entry's hash, and it got it by reading and parsing
  every entry written that day — to use one of them. Linear per write,
  quadratic over a session: 0.36 ms per append at five hundred entries,
  3.75 ms at four thousand, still climbing. An agent session writes an entry
  per event, so a couple of busy panes walk the terminal into a stall it does
  not come out of. It now reads back one line: **0.09 ms, flat**.

- **The Agent Cockpit asked every pane for its whole screen four hundred
  milliseconds apart** — 4800 cells with colours and attributes, of which it
  kept the characters and discarded the rest. At forty panes that poll cost
  **8.9 seconds** against a 400 ms budget, which is not a slow window but a
  window that never finishes a frame. It now asks for the eight lines it
  actually reads: **67 ms**, and a pane nobody typed in costs an empty
  envelope. 133× faster, and the worst case — every pane writing at once —
  costs the same as idle.

If you saw blank panes rather than a hang, that was the same starvation from
the front: the main thread had no time left to paint.


## v0.68.0 — 2026-08-18

### Added

- **A terminal an agent can be hosted in, rather than shouted at.**
  `agent_session.*` runs a CLI agent — Codex, Claude — and turns what it
  does into events: what it said, what it was thinking, which tool it
  asked for, how it ended. Before this the only way to know what an
  agent was doing was to parse the terminal, which meant screen-scraping
  a program that was already printing structured JSON. The identifiers
  you pass in come back on every event untouched; nothing here invents
  one, because an id we made up would correlate with nothing on your
  side and look like it did.

- **Capabilities that live in another process, used under a key you can
  take back.** Unterm can now find, bind and drive a browser — Unzoo —
  through leases with an expiry, an epoch and a sequence number. A
  recorded exchange cannot be replayed: a repeated number is refused
  before anything is performed, because a replay noticed afterwards has
  already done the thing it was replaying. Every call leaves evidence:
  a hash of what was asked and what came back, not the payloads, which
  can be a page of your mail.

- **Approvals somebody can actually answer.** The gateway has been able
  to ask since 0.62; nothing could answer, so a destructive request from
  an agent sat there until it expired — a refusal with a five-minute
  delay wearing an approval's clothes. Settings now shows what is
  waiting, with "allow once", "allow for this task" and "always". Agents
  can see the queue and cannot empty it.

- **Workspaces that cannot see each other.** A named root, and every
  other root explicitly denied — including archived ones, because
  "nobody works there any more" is not a reason to let a different
  workspace in. Symlinks, `..`, case on a case-folding volume, Windows verbatim
  and UNC prefixes all resolve before anything is compared. A shell that
  `cd`s out stops being inside: the judgement is remade on every call,
  not remembered from when the session started.

- **An audit trail that shows edits.** Every entry carries its own hash
  and the previous one's, so changing a line makes the next line
  disagree. This does not make the trail unalterable — it is a file —
  it makes alteration visible, which is the honest property a local log
  can offer.

- **Evidence bundles.** One task's whole story as plain files with a
  manifest of hashes, for somebody who was not there and has no reason
  to take your word for it. `unterm-cli evidence export` and `verify`.

- **Diagnostics you can send without sending your life.** Versions,
  process health, counts. No tokens, no prompts, no commands, no paths —
  the bundle is built by naming what goes in, so a field nobody thought
  about is absent rather than leaked.

- **`unterm-cli system`** — what is running, whether it can work without
  a window, data snapshots, and an uninstall that describes what it
  would remove before removing anything.

### Changed

- **Waking from sleep re-checks the world.** No platform API involved: a
  monotonic clock does not advance while suspended and a wall clock
  does, so a large gap between them is how long the machine was away.
  Providers are re-found and leases that lapsed overnight are reported —
  "your agent's permission ran out at 3am" is a different sentence from
  "your agent stopped working".

- **The Core hears the machine ask it to stop.** SIGTERM on macOS and
  Linux, a console control event on Windows. It writes down that it is
  going and why, drops its discovery record so nothing connects to a
  corpse, and leaves. Trying to finish work in those seconds is how a
  process gets killed halfway through finishing it.

- **Upgrades can be taken back.** Snapshot the data, stage beside the
  old binary, swap, run the new one, and put *both* back if it does not
  answer. Verified with real binaries on Linux: a broken upgrade leaves
  the working program and the untouched data.

### Fixed

- **Interrupting an agent now reaches what the agent started.** The
  grace period was measured against the agent itself, and a shell dies
  on SIGINT while the background job it launched ignores it — POSIX
  requires exactly that — so the interrupt reported success at the
  moment the survivors were orphaned. It is measured against the whole
  process group now.

- **A model can no longer drive a browser around the front door.**
  Raw CDP, Playwright, Puppeteer, Selenium and browsers started with
  automation flags are refused inside a managed agent session, with the
  supported path named in the refusal. A development workspace can be
  given an exception; it is a grant, so it has a clock on it and shows
  up in the trail when used.

- **149 methods, and every one of them classified.** A contract test now
  fails the build if a published method has no entry in the risk
  registry — previously only the 103 frozen at 0.66.0 were checked, so
  anything added after that was outside the net.


## v0.67.0 — 2026-08-15

### Fixed

- **A TUI opened at startup is no longer drawn wider than the window.**
  Launching Claude Code — or anything that trusts `$COLUMNS` — in a
  freshly opened Unterm cut the right-hand side off until the window
  was maximised. Maximising was not really a workaround: it was the
  first time anything measured the terminal correctly. The startup path
  sized the first shell from the whole window while every later
  measurement subtracted the tab strip and the padding, so the shell was
  told it owned the sidebar's columns as well as its own. Both paths now
  measure the same thing.

### Changed

- **Fleets are stored durably instead of in a JSON file.** The Agent
  Cockpit's record of what ran now lives in a transactional store with a
  state machine and crash recovery, rather than `~/.unterm/fleets.json`
  rewritten whole on every change — where a crash between the write and
  the rename lost the lot. Existing fleets are imported automatically
  the first time this version touches them, and the old file is kept
  (renamed, not deleted) rather than consumed. Nothing about the Review
  page, `fleet.*` or `review.*` changes: same commands, same shapes,
  same behaviour.
- A worker that dies mid-flight now leaves a verdict rather than a row
  stuck at "running" forever, so the Cockpit stops showing work in
  progress that nobody is doing.

### Internal

- The release pipeline now refuses to build a tag that does not match
  the version in the tree, and unpacks each finished artifact to ask the
  binaries inside what version they are — a correctly-named archive full
  of the previous release used to be undetectable.
- Milestones M0, M1 and M2 of the One Core plan are closed; the durable
  task engine (`unterm-tasks`) and the action gateway's vocabulary
  (`unterm-gateway`) are in place.

## v0.66.0 — 2026-08-14

### Added

- **"Keep running in the background" leaves something on screen.**
  Choosing it at the close prompt used to end with the process gone
  and no evidence anywhere that the Core still held your shells — the
  promise was real, but only the dialog you had just dismissed ever
  said so. The window now parks instead of exiting: a menu-bar item on
  macOS, a notification-area icon on Windows and Linux, reporting how
  many sessions are running and how many agents are waiting, with
  "Open Window" and "End everything and quit" behind it. Reopening
  rebuilds the window onto the sessions the Core kept, scrollback and
  split arrangement included. On macOS the Dock tile steps aside while
  parked (the menu bar is where the app lives then) and clicking
  Unterm in Finder or Spotlight brings the window back.
- **An agent can bring a parked window back.** `instance.focus` and
  any confirmation prompt now reopen the window instead of failing for
  as long as Unterm sits in the background — which is exactly when an
  agent has something worth showing you.

  Known limitation on Windows 11: it files every new notification-area
  icon into the overflow flyout behind the `^` chevron, and there is no
  supported way for an application to promote itself out of it. The
  indicator is there and its menu works, but it is one click further
  away than on macOS or GNOME until you drag it onto the taskbar. On
  Linux the indicator needs a StatusNotifier host — Ubuntu ships and
  enables one, bare upstream GNOME does not.

### Fixed

- **The two close buttons no longer disagree about your shells.** The
  title bar's own cross went through one path and the system frame's
  (along with Cmd-W and Alt-F4) through another: one left every Core
  session running with nothing attached, the other destroyed them all,
  for the very same click on the very same window. Both now run one
  function with one argument, so whether your shells survive is
  decided by what you chose, not by which pixel you hit.

## v0.65.0 — 2026-08-13

### Fixed

- **The redraw storm is over.** `needs_redraw` compared the frame
  cache's generation; `draw` recorded a sum of per-pane snapshot
  revisions — a different counter, never equal in Core mode, so every
  idle tick redrew and reshaped the whole window forever. That double
  bookkeeping was the chronic 90%-CPU window, and under heavy agent
  output it grew into multi-second freezes: the content area sat black
  while the GUI thread ground through full-screen HarfBuzz passes and
  queued behind the compositor for drawables. Draw now records the very
  number `needs_redraw` compares, and a flood of output draws at most
  one frame per refresh interval. Idle CPU dropped from ~94% to under
  10%; a 20,000-line CJK flood renders in under a second with zero
  black frames and zero watchdog stalls.

### Added

- **Settings can choose the default shell**, with a per-platform
  install plan for shells that are not there yet (winget on Windows,
  Homebrew on macOS; Linux states plainly that there is no one-line
  official install).
- **`session.create` takes `argv`** — a program argv array launched
  directly, no shell wrapping — alongside the existing `command`
  string that still runs through the platform shell.
- **Split layouts survive a GUI restart.** The arrangement, not just
  the panes, comes back.
- **`fleet retry` and `review verify`** join the Agent Cockpit
  lifecycle, on the CLI and over MCP.
- The startup session draws as soon as it is ready instead of waiting
  out the first housekeeping tick.

### Changed

- **`cli`, `connect`, `record` and `replay` are legacy stubs now**:
  each answers with a machine-readable error naming its replacement
  (`session` / `instance` / `server` commands, `session record` and
  `session export`) instead of dragging half-working mux-era code
  along. MCP stdio startup is hardened for clients that race the
  handshake, and Core-first discovery is documented and covered by
  compatibility tests.

## v0.64.0 — 2026-08-10

Three days of live debugging with CJK as the crash-test dummy. Four
bugs that looked unrelated — garbled typing, spaces appearing in
pasted text, double-click selecting one character, the cursor wedged
between characters — all turned out to be corners of the same debt: a
wide character owns two grid columns, and every consumer had its own
opinion about the second one.

### Fixed

- **Typing CJK no longer shatters into `<009d>` byte junk.** A GUI
  launched from Finder inherits no locale, so shells ran under
  `LC_CTYPE=C` and zle treated every 0x80–0x9F UTF-8 continuation
  byte as a C1 control. Unterm now synthesizes `LANG` from the
  system region (validated against the locales the OS ships), the
  way Terminal.app, iTerm2 and Ghostty do. Anything already set wins.
- **Copied text is the text, not the grid.** The spacer cell behind
  every wide glyph went onto the clipboard as a real space — 你好
  pasted as 你 好. All copy paths now strip it.
- **Double-click selects the CJK word, and keeps it.** The spacer
  posed as a word boundary (one hanzi per double-click), and the
  micro-move inside a real double-click re-extended the drag to the
  pointer cell, shrinking the selection. A held drag now grows by the
  granularity its click streak established: word by word after a
  double-click, row by row after a triple-click.
- **The cursor owns both columns of a wide character.** The block was
  one cell wide, covering half the glyph — cursor movement through
  CJK text looked broken while the position was right all along.
  Block, underline and the unfocused outline now span the character;
  a cursor reported on the continuation cell snaps to the lead.
- **A live IME composition owns its keys.** Keystrokes inside an
  active pinyin composition also arrived as raw letters — 房间里
  landed as "fangjianli房间里". Composition keys no longer reach the
  shell; switching input sources mid-composition no longer strands a
  phantom candidate window.
- **"Open in Unterm" works from every entry point.** The Finder
  extension now deep-links around the sandbox that silently ate its
  open requests, watches every mounted volume (watching /Volumes
  itself watches nothing), refuses App Nap, and leads with the deep
  link instead of a doomed document-open that raised an alarm dialog.
  The two Services menu items, dead buttons since v0.40, are wired.
- **A window opened at a directory starts there.** It used to adopt
  the focused old session and ignore the request.
- **The tab strip holds still.** Rows no longer teleport between a
  pinned section, reorder under the pointer, or start a drag from a
  mere click; a drag ends the instant the button lifts.
- **Quit and close never hang.** Session drain moved off the UI
  thread with a three-second fuse, and an occluded window no longer
  spins 70% CPU waiting for a drawable nobody will show.
- **Right-click follows one rule everywhere**: selection copies,
  no selection pastes — every pane, every platform. A screen capture
  leaves both the image and its file path on the clipboard, and
  pasting an image pastes a shell-quoted path to it.
- **MCP confirmations appear when asked.** The banner was painted by
  the frame loop, and a resting window schedules no frames — every
  agent write died unanswered with nothing on screen. It repaints on
  registration now. `unterm-cli agent trust/untrust` accepts the
  documented parameter name it always printed in its own help.
- **A Windows upgrade leaves a terminal behind**, and a failed one
  leaves a trace; the default font is staged where the MSI actually
  looks.

## v0.63.2 — 2026-08-07

### Fixed

- **"Open in Unterm" reaches a window that is already open.** v0.63.1
  fixed the cold-launch half: right-clicking while Unterm was closed
  opened it at the folder, but doing the same with Unterm already running
  did nothing — AppKit records what a delegate answers at the moment the
  delegate is set, before our handler existed. The delegate is now re-set
  once after the handler is added, and deliveries to the running window
  land as focused tabs. Same cure for folders dropped on the Dock icon.

## v0.63.1 — 2026-08-07

A same-day patch: the fixes a day of hard dogfooding earned.

### Fixed

- **Backspace deletes again.** Switching input sources mid-composition
  stranded the IME's marked text, and macOS fed every Backspace to the
  ghost composition instead of the shell. Two locks now: any key arriving
  while a stale composition lingers clears it on the spot, and an
  input-source-change listener clears it before anyone even types.
- **The title-bar chevron answers corner clicks.** Insetting it from the
  window's rounded corner left ten dead pixels exactly where years of
  muscle memory throw the pointer. The icon stays inset; the target
  reaches the edge again.
- **The folder picker steps in front.** Its dialog belongs to a helper
  process nobody activated, which modern macOS may park behind the
  terminal — reading as "nothing happened". The dialog now activates
  itself, and a start directory the dialog cannot open no longer fails
  the whole picker.
- The Agent Inbox theme shows its name in the theme picker instead of a
  raw localization key, in all nine languages.
- CI's Windows gate woke from a two-day runner outage owing three stale
  entries (a renamed theme test, tests deleted with the dead statsbar
  ladder, and a section pointing at tests that moved into the engine with
  layout plan B); the required lists are reconciled against the codebase
  in full, and the kernel size budget stands at 12800/12800 lines and
  10/10 external dependencies — paid for by deduplicating the raster
  entry points rather than by raising the ceiling.

## v0.63.0 — 2026-08-06

The release the install deserved. 0.62.0's Core architecture was real, but
what a fresh install actually got was quieter: no `unterm-core` in any
package, a settings page whose every request bounced, and an emoji face that
had never drawn a single glyph. A day of dogfooding a clean install found
them all.

### An install now contains the product

- **`unterm-core` ships.** All six release pipelines — macOS DMG and zip,
  Windows MSI and zip, deb, AppImage — packaged the window and the CLI but
  not the engine daemon they both look for beside themselves. Every install
  silently fell back to the in-process engine; none of the Core architecture
  ever reached a user.
- **The settings page works in Core mode.** The window skipped registering
  itself when the Core hosts the agent API, so the page bootstrapped with no
  credentials and every call — including the one that fetches its own title —
  came back 401. The window now registers with the Core's port and token,
  which also revives the stale-registration takeover: a dead instance's
  record no longer squats on `server.json`.
- **macOS finally wears Logo A.** The brand refresh regenerated Windows and
  Linux assets and left the mac `.icns` to "the release process", which had
  no such step. The icns is now generated from the same assets, into the
  bundle template.

### Emoji, for the first time

- **The bundled emoji face never worked.** It was a COLRv1 font, which
  FreeType cannot rasterize — so the face opened, claimed the code points,
  and drew nothing, from the day it was bundled. It is now the CBDT bitmap
  build, the rasterizer picks the nearest strike and scales it, and a colour
  glyph renders as a silhouette the chrome can tint. The sidebar's ✋
  waiting badge exists on macOS for the first time.

### Typing keeps up

- **The pause-then-type lag is gone.** After two quiet seconds the render
  loop relaxed its engine polling to 96ms — and a keystroke did not wake it
  back up, so the first character after a pause could wait most of a tenth
  of a second to appear. Keyboard and IME input now snap the loop back to
  its 8ms cadence before the shell sees the byte.

### The window, straightened

- **The title bar sits on one line.** On macOS the quiet bar was four pixels
  shorter than the height AppKit centres the traffic lights for, and the
  bar's own text was nudged the other way: lights low, title high, chevron
  in the window's rounded corner. The bar now uses the platform's 28 logical
  pixels, the text centres on the lights' optical line, and the chevron has
  the margin the lights get on their side.
- **The shell picker appears when you reach for it.** At rest the sidebar
  footer is two things — new session and settings. Point at the row and the
  dropdown pill shows itself, centred, beside the label it belongs to.
- **A GUI stall leaves a trace again.** The stall watchdog lost in the
  rewrite is back: gaps over two seconds land in `stall.log` with their
  duration, so a frozen window is a diagnosis rather than an argument.

### Windows: open in a tab

- **Explorer's right-click grows "Open in Unterm tab".** macOS has handed
  directories to the running window via Finder all along; Windows always
  opened a new one. `unterm.exe --tab` forwards the directory to the live
  window as a new session and focuses it, and with nobody to take it,
  degrades to a plain open. No console flashes: the forwarding lives in the
  GUI-subsystem binary.

### Fixed

- The macOS folder picker no longer fails outright over a start directory
  it cannot open (an unmounted volume, a cloud placeholder): it asks again
  without the default location, and its error notice now names the cause.
- `assert!` messages in an edition-2018 crate printed their placeholders
  instead of the values; newer compilers rightly refuse.
- The `version_exit` test measures the product's startup rather than
  Gatekeeper's scan of a freshly linked binary.
- 110MB of accidentally committed Windows packaging output removed from the
  repository tip, with ignore rules so it stays out.

## v0.62.0 — 2026-08-05

Your shells stop belonging to the window. A per-user `unterm-core` process
holds the sessions and serves the agent API, so closing the window no longer
ends what was running in it, and an agent connected to Unterm stays connected
across a restart of the window it was watching.

### Sessions outlive the window

- **`unterm-core` owns the terminals.** Sessions, scrollback and split
  arrangements live in a process of their own. Close the window and reopen it
  and they are still there, contents and layout intact. Set
  `UNTERM_CORE_CLIENT=0` to keep the old single-process arrangement.
- **The agent API moved with them.** One MCP server, in the Core, serving the
  sessions it owns. Previously the window hosted it, so quitting the window
  disconnected every agent — even though the shells they were driving had
  nothing to do with that window.
- **A crashed Core no longer means restarting Unterm.** The window notices,
  says so, finds the replacement Core and rebuilds itself onto it. The shells
  the dead Core held are gone — nothing can bring those back — but the window
  recovers by itself and opens a fresh one.

### The confirmation before an agent types into your shell

- **It now shows you the command.** It used to lead with a byte count, so the
  one thing you needed in order to decide was also the first thing cut when
  the row ran out of room: `exec.run: len=22…`.
- **A newline hidden mid-command is flagged.** Approving `ls` and getting
  `rm -rf /` along with it is the shape this catches. An ordinary trailing
  newline is not flagged, because a warning that is always on is not a warning.
- **It is translated, and it fits.** Measured in display columns rather than
  characters — the Chinese labels are twice as wide as they look, and the key
  that refuses used to be the first thing pushed off the edge.
- **Being blocked tells you which kind.** "No window is open to approve this",
  "the user refused" and "nobody answered in time" are three different things
  to do next; all three used to arrive as the word *denied*.

### Fixed

- The CLI no longer reports "Unterm is not running" at an Unterm that is
  running and waiting for you to approve something. Its read timeout now
  outlasts the confirmation it is waiting on, so the real verdict arrives.
- A `unterm.conf` saved by Notepad or `Set-Content -Encoding utf8` is read
  instead of silently ignored — the UTF-8 byte-order mark used to swallow the
  whole file.
- Recordings, exports, trust lists and registries all honour
  `UNTERM_STATE_DIR`. 49 places had each worked out where state lives on their
  own, and most of them ignored it.

## v0.61.1 — 2026-08-02

A hotfix for Windows installs. If v0.61.0 would not open after upgrading,
this is the release that fixes it — install it directly over the broken one.

### Windows

- **Upgrading from 0.57.4 left an install with no terminal in it.** The old
  WezTerm-era `unterm.exe` was stamped file version 1.0.0.0; the next-core one
  is stamped 0.61.x, which Windows Installer reads as *older*, so the upgrade
  skipped copying the new exe and then deleted the old one with the old
  product. The installer now forces every packaged file onto disk
  (`REINSTALLMODE=amus`), which also repairs installs already in that state.
- **Startup failures are no longer silent.** unterm.exe is a GUI-subsystem
  binary; a fatal error used to vanish with the process. Fatal startup errors
  and panics now show a message box and are appended to `~/.unterm/panic.log`.
- **A backend only wins after proving it can present.** Graphics bring-up now
  configures the real surface and takes a first frame before committing to
  DX12; on failure it falls back to GL (ANGLE), and a wgpu error at runtime is
  a logged frame skip — plus a one-shot swapchain reconfigure — instead of a
  process death.

### Packaging, all platforms

- JetBrains Mono Regular — the default terminal font, opened by file name —
  now actually ships in the MSI, the Windows zip, and the macOS DMG. The
  macOS DMG also gains the Symbols Nerd Font and emoji faces it had been
  missing entirely: an installed 0.61.0 drew every chrome icon as an empty box.

## v0.61.0 — 2026-08-02

The first release in which the kernel replacement is *finished*: feature
parity with the WezTerm-based 0.57.4 closed item by item against a
159-requirement ledger and a 29-item interaction audit, on the platform
people actually run it on.

### Performance

- **22ms from launch to a live MCP surface** on macOS, against 7.1s for the
  old kernel on the same machine. Windows installs start in 761ms vs 1349ms.
- **Throughput holds**: 200k lines through the PTY in 0.45s, identical to the
  old kernel. Idle at a prompt stays at 6.6% of a core (was 80% pre-0.60).

### macOS, natively

- The window keeps its real frame: traffic lights, system corners and shadow
  over the custom chrome (`titlebar_transparent` + fullsize content view).
  No brand mark crowding the corner — the Dock already says whose window it is.
- A point is a point: `font_size 13` is 13px, the same number Terminal, iTerm
  and Warp mean. The default face is the bundled JetBrains Mono again, whose
  generous line gap is what made 0.57.4's rows breathe.
- CJK text is finally right, four fixes deep: the spacer cell after a wide
  character no longer counts as a phantom column (TUI boxes aligned, links and
  copy-mode columns correct past any hanzi); font collections are enumerated
  face-by-face so PingFang Regular wins over Hiragino W0 hairline; the
  on-demand font assets directory joins the scan; and glyphs sit centred in
  their double cell at natural size. A macOS coverage curve gives strokes the
  weight CoreText would.
- Interactive screenshots (`screencapture -i`, hidden-window variant included)
  and the system folder picker are real on macOS and Linux, not Windows-only
  stubs. `capture.window` works headlessly via CGWindowList.

### The agent surface

- The MCP write-confirmation banner asks in the status row, wakes an idle
  window to show itself, and answers to Enter — verified end-to-end:
  park → banner → approve → write → audit.
- `allowed_patterns` in the command policy is enforced (blocks always win;
  an empty allowlist means no allowlist). `policy.check` previews exactly
  what execution will decide. (#16)
- The audit trail persists: redacted JSON lines in per-day user-only files,
  thirty days kept, backfilled into `session.audit_log` on restart. (#18)

### Fixed

- Go-to-directory jumps again from every entry point, with the in-app palette
  as the feature and the OS picker as its Ctrl+O row.
- The command palette, tooltips and the quick menu render on their own top
  tier: nothing bleeds through a modal card, file tree open or not.
- Search reads through the same history path as `screen.text`, so a live PTY
  can no longer report zero matches while text is visibly on screen.
- The Git dock only swallows mouse presses; releasing a drag over it no
  longer wedges the selection. A tree directory opens on one click and only
  cds on a double, through POSIX-quoted paths.
- Per-pane scrollbars and the pane close button are back to 0.57.4's exact
  geometry; palette selection clamps instead of wrapping.

## v0.60.0 — 2026-07-30

The version jumps from 0.57 because what is underneath it changed completely.
Unterm was a fork of WezTerm. It is not one any more: WezTerm is gone from the
build and everything it used to do is done by a kernel written for this product.

### Changed

- **The terminal kernel is Unterm's own.** Parsing, the screen model, scrollback,
  Unicode width, selection, panes and sessions are all `next-core` now, held to
  a hard budget of 12,000 source lines and 10 direct dependencies so it stays
  something a person can read. The front end is winit + wgpu.
- **The window is drawn from a design scale rather than the terminal grid.** The
  chrome has its own proportional face at 13pt; it stopped being laid out in
  terminal cells, which is what used to spell the wordmark `u n t e r m`.
- **The left tab strip is the primary navigation again**, with project groups,
  shell and agent icons, count badges, and a jade rail on the active row. The top
  bar is a single row of labelled actions and carries no tabs.

### Fixed

- **An idle window cost most of a CPU core.** The event loop polled as fast as
  the CPU allowed, and every pass re-listed sessions, re-derived the title and
  resized every pane -- a PTY resize syscall per pane, four times a second,
  forever. It now waits between frames, drops to 48ms once nothing has happened
  for two seconds, and only resizes a pane when the pane actually moved.
  Measured idle at a prompt, release build: **80% of a core before, 6.6% after**.
- **Focus reporting never worked.** Two `WindowEvent::Focused` arms, the second
  unreachable, and the unreachable one was the half that sent DEC mode 1004. vim
  and tmux have been showing the wrong focus state the whole time.
- **The chrome had no icons.** The bundled Nerd Font was requested by family
  name, which only works for installed fonts, so every icon resolved to nothing.
  Bundled faces are now loaded by file.
- **The play mark `▶` drew nothing while still advancing the pen.** The bundled
  colour-emoji face sat ahead of the installed families and claims text symbols
  it cannot render under a monochrome pass. It now sorts last, and rasterisation
  skips any face that returns an empty bitmap.
- **`unterm-services` and `unterm-cli` did not build on Linux or macOS**, having
  called `libc::kill` without declaring libc. Nothing caught it because CI was
  still checking crates that the WezTerm removal had already deleted.

## v0.57.4 — 2026-07-25

### Fixed

- **`server.health` now reports the instance's real MCP port.** The response hardcoded 19876, so on multi-instance setups (alpha/bravo/…) every window claimed the same port — an agent probing health on bravo would be told to connect to alpha's port. The port now comes from the instance's own server info.
- CI: the cockpit verification timeout test no longer flakes on busy Windows hosts — process-tree teardown via `taskkill` gets a realistic cleanup deadline, and the timing assertion reports the measured duration on failure.

## v0.57.3 — 2026-07-24

### Fixed

- **The bottom-bar screenshot chips (`capture:exclude` / `capture:include`) are visible again.** The v0.57.2 responsive status bar gated them behind a ≥208-column window — wider than a 13–14" laptop can reach even fullscreen (~150–180 columns), so the chips effectively vanished. They now appear from 128 columns, the same tier as the proxy/mcp chips, with a unit test locking the laptop-fullscreen case.
- **Theme switching no longer transiently corrupts the left tab bar.** Every theme switch reloads the config, which rebuilds the glyph-texture atlas — but the left tab bar's cached element (which stores atlas coordinates) was not invalidated on that path, so it painted stale sprites (wrong colors, or garbled glyphs once the atlas layout shifted) until its 5-second cache TTL expired. Atlas recreation now invalidates the left tab bar in `recreate_texture_atlas()` itself, covering every current and future caller; the local theme-apply path invalidates it too. A/B frame captures confirm the sidebar now switches in the same frame as the pane.

## v0.57.2 — 2026-07-23

### Fixed

- **Right-click copy now works inside mouse-aware TUIs (Claude Code, vim, htop).** Selecting text with the bypass modifier (Shift+drag) and then plain right-clicking used to forward the click to the application — the copy silently never happened, which read as "right-click copy isn't implemented" in exactly the panes agents live in. An Unterm-side selection can only exist in such panes via the bypass drag, so it now proves the gesture: right-click completes the copy. Without a selection the click still belongs to the TUI (no paste hijack). Live-verified on macOS against a mouse-reporting pane.

## v0.57.1 — 2026-07-22

### Fixed

- **macOS: Ctrl+Left-click is treated as a secondary click only when CTRL is the sole modifier.** The v0.57.0 pointing-device workaround matched any CTRL-containing chord, which hijacked the default Ctrl+Shift window-drag gesture and silently shadowed user CTRL mouse bindings — each of them pasting the clipboard instead. A consumed Ctrl+Left press now also swallows the rest of that physical gesture, so a single click can no longer both paste and trigger the default drag/release bindings (extend selection, open link).
- **An empty clipboard no longer shows a "couldn't paste" error.** Right-clicking with nothing on the clipboard is a normal no-op now, on every platform; the red failure notice is reserved for real read failures. (Live-verified on macOS along with copy, paste, and Ctrl+click secondary paste.)
- **Sidebar: the active tab is revealed again when row churn pushes it out of view.** Reordering the active tab or closing tabs above it scrolls it back into the viewport; scrolling away yourself still isn't overridden.
- **Sidebar: duplicate project names always get a parent hint.** When one project path's tail is contained in another's (`/acme/app` vs `/work/acme/app`), the shorter one now falls back to its immediate parent instead of rendering an ambiguous bare name; narrow headers no longer overflow into the count badge by one column.
- **Paste to a closed pane no longer leaks per-pane state**, and the duplicate-name disambiguation search no longer reruns its O(projects²) scan on every paint tick — it's memoized on the actual project set.

### Added

- Unit tests locking the secondary-click modifier matching, the mouse-reporting guard, and the suffix-contained duplicate-name fallback.

## v0.57.0 — 2026-07-21

### Added — Fleet verification loop

- **Automatic verification per Fleet member.** Review can infer and run conventional validation commands for Cargo, Go, npm/pnpm/yarn, Python/uv, Maven, Gradle and .NET projects, or run an explicit command. Results persist locally with status, exit code, duration and a bounded log; timeouts terminate the full process tree.
- **Candidate scoring and ranking.** Review ranks Fleet members using verification status and change size, while showing changed files, line churn, untracked files, commits ahead, elapsed time and worktree health. Git sampling runs off-thread behind a short cache and never blocks the page.
- **Safe member retry.** Failed members can restart in their existing isolated worktree without losing committed, staged, unstaged or untracked changes. Attempts and launch errors are persisted with backward-compatible Fleet data.
- **Verification-gated merge.** A normal squash merge requires the member's latest verification to pass. MCP/CLI can explicitly use `force` to override the gate; the override and verification record are included in the audit response.
- **Web, MCP and CLI parity.** Added `review.verify`, `fleet.retry`, `unterm-cli review verify`, and `unterm-cli fleet retry`, plus Review controls for validation, logs, retry, rank and score.
- **Fast project and window navigation.** The left sidebar now groups tabs by a stable repository root, disambiguates equal project names with their parent directory, and exposes an always-visible fuzzy search. Results include tab number, Agent or foreground command, full project path and split count.

### Fixed

- **Windows checkpoint diffs include untracked files correctly.** The platform now passes exactly one null device when generating synthetic patches. Rollback verifies the restored Git tree, and merge responses include commit and staged-file audit data.
- **Right-click copy and paste target the clicked pane again.** Split-pane clicks no longer re-resolve to a stale active pane, and Windows clipboard reads once again start on the event-loop thread instead of silently failing on a worker.

### Changed

- **New command-loop brand mark.** The app icon and every logo surface (macOS .icns, Windows .ico, Linux hicolor ladder, sidebar / titlebar marks, website favicons and social card) move to the command-loop terminal mark — a metal U-loop with a jade prompt chevron on the dark tile.

## v0.56.0 — 2026-07-21

### Changed

- **The documentation caught up with the product.** README and unterm.app were rebuilt around Agent Cockpit as the headline: the MCP reference now documents the full surface — 97 methods across 21 namespaces (the `agent`, `cockpit`, `fleet`, `review`, `profile`, `meta`, `upload`, and `ghost` namespaces were previously undocumented) — and the CLI reference covers all 32 subcommand families, including the `fleet` and `review` families and the cockpit's `agent status / signal / inbox / enable-hooks`, each verified against the shipping binary. The site's capability map and per-namespace counts were corrected from the stale numbers (65 methods / 11 namespaces / 22 CLI commands).
- **No engine changes since v0.55.3.** This tag exists so the in-app version string, the update poller, and the website all point at the same milestone as the refreshed documentation.

## v0.55.3 — 2026-07-13

### Fixed

- **The "jump to bottom" button now actually scrolls to the bottom when clicked.** The button (added in v0.55) rendered correctly but its click handler was shadowed by an earlier catch-all match arm in the mouse dispatcher, so clicking it did nothing — only the keyboard `ScrollToBottom` worked. The dedicated handler is now reachable. (This also un-breaks the `-D warnings` CI check, which had been failing on the unreachable pattern since v0.55.1.)

## v0.55.2 — 2026-07-13

### Fixed

- **Windows: the minimize / maximize / close buttons no longer disappear when the window is narrowed.** In the top tab bar, the tab cluster carried a higher paint order (`zindex 1`) than the floated window-control cluster (`zindex 0`). A single tab is sized up to `window_width / tab_count`, so on a narrow window it extends across the bar and painted over the window controls, hiding them. The window-control cluster now paints on top (`zindex 2`), and click hit-testing already favors it, so the buttons stay both visible and clickable at any width.
- **Long login URLs (e.g. Claude Code's OAuth link) are now clickable and copyable in full.** Logical-line reconstruction capped a wrapped line at 1024 characters, which split URLs longer than that: clicking opened only the fragment under the cursor, and copying a selection that crossed the split inserted a newline mid-URL, breaking the paste. The cap is raised to 8192 — enough for any real login URL, still guarding the megabyte-JSON pathological case, and only computed on hover / selection.

## v0.55.1 — 2026-07-13

### Fixed

- **Windows: Claude Code no longer errors after every turn.** Claude Code executes its hooks through bash (Git Bash) even on Windows, and the cockpit hook command v0.55.0 wrote — `C:\Program Files\Unterm\unterm-cli.exe …` — was unquoted, so bash split it at the space and ate the backslashes, failing with `/usr/bin/bash: line 1: C:Program: command not found` at the end of every turn. The hook command is now quoted and uses forward slashes (valid in bash, PowerShell, and cmd alike). Re-running `setup-ai` (which the GUI does automatically on a version bump) or `unterm-cli agent enable-hooks` self-heals an existing broken entry in place rather than appending a duplicate. Only Claude Code and Aider were affected; Codex's `notify` is an argv array executed directly and was always fine.

### Added

- **"Jump to bottom" button on scrolled-away panes.** When a pane's viewport is scrolled off the live tail — reading history while an agent keeps streaming output — a small chevron button appears in the pane's bottom-right corner. Click it to return to the bottom and resume following; it disappears once the pane is pinned to the tail again. Keyboard `ScrollToBottom` is unchanged; this is the mouse-flow counterpart.

### Changed

- **The Web Settings page shows the release version, not just the build stamp.** It now reads `v0.55.1 (20260713-…)` — the semantic version you recognize first, the git build stamp (for bug reports) in parentheses — instead of only the raw stamp, which read as "not updated" right after installing a new release.

## v0.55.0 — 2026-07-11

### Added — Agent Cockpit

Unterm is the terminal AI agents can drive. v0.55 adds the other half: the terminal now sees, aggregates, and orchestrates the agents running inside it.

- **Agent state, live, everywhere.** Unterm watches every pane for AI agents (Claude Code / Codex / Gemini CLI / Aider / …) and folds four signal layers — official lifecycle hooks, OSC title/progress/notification parsing, foreground-process detection, and screen-text heuristics — into one state per pane: working, needs-you, done, idle. Zero configuration for Claude Code and Gemini; `unterm-cli agent enable-hooks` (run automatically by setup-ai) wires exact hook reporting for Claude Code, Codex, and Aider, merge-only with `.unterm-bak` backups.
- **You can see it without looking for it.** Sidebar tabs carry a state dot (breathing blue = working, amber = needs you, green = done); the top bar shows a cross-window tally chip that turns amber the moment any agent blocks on you. Click it — or press `Ctrl+Shift+A` — for the **Agent Inbox**: every agent sorted waiting-first, Enter jumps straight to its pane, wherever it lives.
- **Fleet: one task, N agents, N isolated worktrees.** Launch from the Inbox (or `unterm-cli fleet launch --agents claude,claude,codex -- <task>`): each member gets its own git worktree beside the repo (`../<repo>.fleet/…`), its own branch, and its own tab whose badge tracks that member's state. Crew presets are built from the agents actually installed; `claude+codex+gemini` runs the same task through three different models.
- **Review: nothing an agent does is untracked.** The moment an agent starts working in a repo, Unterm takes a non-invasive checkpoint (a dangling commit — HEAD, index, and files untouched). The new **Review** page in Web Settings shows every fleet member and checkpoint with line-level diffs (untracked files included), squash-merges a member's work into your repo as staged changes (the commit stays yours), discards what you don't want, rolls a repo back to any checkpoint, and compares two members' takes side by side.
- **Everything above is also MCP + CLI.** Twelve new methods (`agent.status/signal`, `cockpit.inbox`, `fleet.*`, `review.*`) and matching `unterm-cli agent status/signal/inbox`, `fleet`, and `review` subcommands — external agents can drive the cockpit itself.

### Fixed

- **CJK input methods no longer make palette inputs look dead.** With an IME active, keystrokes enter composition and never arrive as key events — palettes received nothing while the marked text painted at the pane cursor *behind* the card. Composition now previews inline in the palette's input line (tinted until committed) and the IME candidate window anchors to it. Applies to the fleet palette and the directory-jump palette.
- **OSC 9 / OSC 777 notifications now actually reach the window.** They were dropped by the mux-subscription pre-filter before any window-side consumer could see them.
- **MCP methods accept the documented `pane_id` parameter** (previously only `id` / `session_id` worked).
- **`unterm-cli agent signal` routes to the instance that owns the calling pane** (via the instance-unique `gui-sock-<pid>` in the pane's environment) instead of whichever instance registered last — with several windows open, hook events could tag the wrong pane.
- **Sidebar no longer rebuilds on every agent title-spinner frame.** Agents animate a braille spinner in their pane title several times a second, and the raw title sat in the sidebar cache key — every frame forced a full (~44ms) sidebar rebuild for as long as an agent worked. State glyphs are now stripped before the title enters the cache key.

## v0.54.5 — 2026-07-10

### Fixed

- **Ctrl+Shift+O now actually opens the directory-jump palette.** The shortcut was declared on the command definition but the action was never listed in the default-actions table that the key map is generated from, so the binding never existed and the keystroke fell through to the shell as `^O`. The palette was reachable only from the toolbar folder button and the context menu; the documented shortcut now works.
- **macOS/Linux: the bottom bar no longer mislabels the shell as `zsh (wsl)`.** The WSL heuristic was "the foreground process path starts with `/`" — which is true for every shell on unix platforms (`/bin/zsh`), so the `(wsl)` suffix appeared everywhere. The heuristic now applies only on Windows, where it correctly marks shells running inside WSL.

## v0.54.4 — 2026-07-10

### Fixed

- **The top-bar stats (cpu / mem / uptime / git) now update in real time.** They were frozen at whatever the first paint sampled, for two stacked reasons: the periodic status timer only re-arms inside the title-update path, which nothing reaches without a Lua status handler — so the timer died after one tick; and the stats text isn't part of `TabBarState`, so even a live timer never noticed it changing. The status tick now drives the title update directly, and the stats text participates in the chrome invalidation check. The bar repaints only when a value actually changes (~every 2s while values move, never when idle).
- **Windows: CPU% in the top bar shows a real value instead of a permanent 0.0.** The old sampler was a PowerShell `Get-Process` shim (a hidden ~100ms process spawn per refresh) that couldn't compute a percentage at all. It's now native Win32 (`GetProcessTimes` deltas between refreshes + `GetProcessMemoryInfo`), so the value is real and the refresh is effectively free.
- **Thin lines and small chrome text no longer smear or vanish.** Vertically centering an even-height child in an odd-height container produced a half-pixel offset, and the GPU's linear sampling spread every 1px stroke across two rows at ~50% alpha — the maximize button lost its top edge entirely and the stats text read as clipped. Middle-alignment now rounds to whole pixels.
- **Windows: the directory-jump palette's deep-scan rows indent correctly.** Relative paths were built with `\` separators on Windows while the depth indent counts `/`, so every nested row rendered at depth 0. Paths are now normalized to `/` everywhere.
- **The directory-jump palette no longer paints past its right edge on long paths.** Deep-scan labels and the dim path hints aren't clipped by the layout, so an over-long path pushed the text — and the selected-row highlight with it — outside the card. Labels and hints are now ellipsized from the left (`…window/render`) to the card's width.
- **The directory-jump palette shows one cursor, not two.** The mouse-hover highlight and the keyboard selection were independent, so hovering one row while arrowing to another showed both at once. Hover now moves the selection itself (launcher-style), so mouse and keyboard share a single cursor.

### Changed

- **The sidebar↔bottom-bar corner uses the same dark seam as the top chrome.** The bottom bar's top hairline was a light foreground-alpha line that disappeared against the sidebar's grey; it now uses the pane-background tone, matching how the top bar's seam is drawn, so the chrome frame reads consistently.

## v0.54.3 — 2026-07-08

### Fixed

- **No longer crashes on launch on GPU-less hosts (VM / RDP / cloud Windows / old GPUs).** When the OpenGL front end is rejected and Unterm falls back to WebGpu, the initial surface configuration used the window's dimensions verbatim — but configuring a wgpu surface with a 0 width or height panics ("Invalid surface"). On a GPU-less host the window can report 0 dimensions at that moment (the WARP software adapter, an unrealized window), so Unterm aborted before the window appeared. The initial surface size is now clamped to at least 1×1 (matching the existing guard on the resize path); the real size is applied as soon as the window is measured.

## v0.54.2 — 2026-07-06

### Fixed

- **Fullscreen no longer lets the macOS menu bar cover the terminal's top chrome.** In non-native (simple) fullscreen the window covers the whole screen, and the menu bar was set to auto-hide — so moving the mouse to the top revealed it as an overlay that covered Unterm's own top row (stats + toolbar buttons: settings, search, split, …), making them unreadable and unclickable. The menu bar and dock are now fully hidden in fullscreen for a true immersive mode, so they can't reveal on hover. macOS only.

## v0.54.1 — 2026-07-06

### Fixed

- **Pasting no longer freezes the window for up to half a second.** macOS pasteboards are lazy: reading the clipboard makes a synchronous cross-process request to whichever app owns it (a browser, editor, or IM), and that read ran on the GUI thread — so pasting content copied from a busy or slow source app stalled the whole window until that app answered. The clipboard is now read on a background thread. Text copied inside the terminal was already fast and stays fast.
- **Double-clicking a word no longer copies an extra character.** The mouse pixel-to-cell mapping rounded the click position to the nearest cell boundary — a forgiveness meant for drag-selection endpoints that also applied to a fresh click, so double-clicking the right half of a character rounded into the next cell and the word selection grabbed one extra letter (`hello` → `hellow` / `ohello`). A click now resolves to the cell the pointer is actually inside; dragging still rounds so the selection endpoint stays forgiving.

## v0.54.0 — 2026-07-05

### Performance

- **Cold start is 2.8× faster: ~780ms → ~280ms to first frame.** Five startup-path optimizations: config keys are validated against a precomputed key set instead of materializing the whole config per key (Lua eval 297ms → 8ms); the second config load is skipped unless the GUI actually queried screens/appearance before startup (128ms → 0); WSL distros are enumerated from the registry instead of spawning `wsl.exe` (43ms → 4ms); the 1119 built-in color schemes parse in parallel and warm in the background (195ms → 43ms); and the OpenGL context is prewarmed on a helper thread right after argument parsing, then adopted by the real window (GPU setup 222ms → ~110ms, fully overlapped).
- **Sustained output no longer burns a CPU core on the UI thread.** During output floods (hundreds of repaint requests per second), the throttled `WM_PAINT` handler returned without validating the update region, so Windows re-queued `WM_PAINT` continuously and the message loop spun at ~91% of a core. It now validates the region and lets the frame-rate timer re-invalidate; the UI thread drops to ~4% during the same flood.
- **The MCP server stays responsive during output floods.** Same root cause as above: MCP operations that need the main thread were starved by the paint storm, so `unterm-cli` calls timed out after 30s mid-flood. They now answer in tens of milliseconds.

### Fixed

- **The first terminal row is no longer clipped under the top bar.** The reserved chrome height was still the old 1.6× title-cell formula while the quick-action buttons had grown the painted chrome to ~2× + divider, hiding the top ~16px of row one. The reserved height is now derived from the same button-geometry constants the layout uses, so they can't drift apart again.
- **The settings-menu prewarm no longer fails on small glyph atlases.** The menu's codicons/CJK labels could outgrow the startup atlas; the prewarm now grows the atlas in place (like the paint pass does) instead of giving up, which also spares the first real menu open the atlas-growth hitch.
- **CLI commands without `--id` now target the pane you're looking at.** `session.list` exposed an unordered HashMap walk and the CLI picked its first entry, so with several panes open a bare `unterm-cli exec run` could type into a random pane. The list is now sorted with an `is_active` flag and the CLI prefers the active pane.
- **`session.resize` refuses panes that are laid out by the GUI window** instead of silently desyncing the PTY grid from the visible grid (content clipped / wrapped wrong until the next window resize).
- **A mistyped CLI flag can no longer end up typed into your terminal.** Trailing command/text arguments no longer accept unknown flag-like tokens (`--sesion 0 ...` previously flowed into the command string and was sent to the live pane); clap now rejects them with a hint to use `--`. Flags after the first command token (`ping -n 1 ...`) and the documented `-- --flag` escape are unaffected.

## v0.53.2 — 2026-07-04

### Fixed

- **The left sidebar no longer covers the bottom status bar.** The sidebar's trailing `+` / shell-picker row overflowed its surface and dragged the whole panel down over the bottom info bar, hiding the shell name and the start of the working directory. The footer is now reserved inside the sidebar's height so it meets the status bar flush.
- **Double-clicking a directory in the tree opens the right one.** The first click of a double-click expands the directory and repaints, shifting every row below; the second click then hit-tested a shifted row and `cd`'d to the wrong directory (intermittently, as a repaint race). The double-click now acts on the path captured by the first click and undoes that click's expand.
- **The hover/selection highlight no longer lands on two rows at once.** The hover hit test used an inclusive interval, so two vertically-adjacent rows both matched on the pixel they share. It now uses a half-open interval, so each row owns its top edge but not its bottom edge.
- **macOS traffic-light dots are the right size on Retina.** The custom-drawn window buttons were sized in raw device pixels, rendering at half the native cap diameter on 2× displays. They now scale with DPI.
- **The ↔ resize cursor no longer appears over the tab rows on HiDPI.** The sidebar resize hit test compared the physical mouse position against a logical edge coordinate as a fallback, which matched in the middle of a tab row on 2× displays. Only the (correct) physical test remains.

### Changed

- **Clearer chrome across all themes.** The sidebar↔content and bottom-bar↔content edges use a clearly-visible hairline instead of tone-only contrast, and the top-bar action icons are brighter, so divisions and buttons read cleanly on the dark themes.

## v0.53.1 — 2026-07-03

### Fixed

- **Windows no longer crashes on startup on GPU-less / old-GPU machines.** When the initial software OpenGL front end is rejected and Unterm falls back to WebGpu, the render backend is briefly torn down and rebuilt. The background settings-menu prewarm could fire during that window and dereference the absent render state, aborting the process before the window ever appeared (seen on Windows-on-ARM and in VM / RDP sessions). The prewarm now backs off cleanly and the menu builds once the WebGpu backend is live. macOS and Linux were unaffected.

## v0.53.0 — 2026-07-03

### Added

- **Composer — a prompt queue for driving agents (`Ctrl+Shift+J`).** Queue up several prompts and run them into the active pane one after another. In auto-approve mode the Composer smart-advances through an agent's confirmation prompts, so a batch of instructions runs to completion without babysitting each step.
- **Read-only Git panel (`Ctrl+Shift+G`).** A right-docked panel shows the active pane's repository at a glance — branch, upstream tracking, and working-tree status — without shelling out. The terminal reflows around it, and it toggles away just as fast.
- **Distinct left-sidebar tab titles.** Each sidebar row now derives its label from the pane's foreground command, so a window full of shells reads as its actual work (`zhitong@host`, an editor, a build) instead of a column of identical `zsh` entries.
- **`USAGE.md` agent-terminal guide** documenting the Composer / prompt queue, the Git panel, and the tab-title behavior.

### Changed

- **Unified chrome.** The top bar, left sidebar, and bottom status bar now share one continuous frame and tone (top-left Unterm wordmark, frameless top-bar buttons, no redundant dividers), so the window reads as a single surface rather than three stacked strips.
- **Otty-style left tab bar.** The active row gets a rounded accent-colored selection, rows carry quiet activity indicators, and group headers are cleaner.

### Fixed

- **The left sidebar no longer covers the bottom status bar.** The sidebar's trailing `+` / shell-picker row was overflowing its surface and dragging the whole panel down over the bottom info bar, hiding the shell name and the start of the working directory. The footer row is now reserved inside the sidebar's height, so it meets the status bar flush and the info bar shows in full from the left edge.
- **CJK locales no longer garble menu, overlay, and shell-selector cards.** Wide-character width is measured in cells, so cards laid out under a Chinese locale stop showing mojibake.
- **Windows: no console-window flashes at startup**, a **process-lock self-deadlock** that could hang the window from ever showing is resolved, and the status-bar working directory no longer carries a stray remote-host prefix.
- **`Ctrl+Shift+J` reliably opens the Composer** — the toggle is now registered in the command list.
- **Less tofu flash on first paint.** The top-bar wordmark, tree-sidebar glyphs, title font, and directory-jump glyphs are prewarmed so they render immediately instead of flashing missing-glyph boxes.

### Performance

- **Smoother sidebar resize.** Dragging the sidebar edge now throttles the PTY reflow to ~25 fps instead of reflowing on every pixel of the drag.

## v0.52.0 — 2026-06-29

### Added

- **More AI agents recognized out of the box.** The baked agent manifest now ships definitions for **Kimi Code CLI** (Moonshot login or BYO key) and **Trae Agent** (provider / model / config-driven), so they are auto-discovered and registerable like the existing agents without a manifest refresh.

### Fixed

- **Windows windows can always be grown again.** The window minimum size is now clamped so a window can no longer be shrunk to a tiny size that left it impossible to drag back larger.
- **Windows clipboard survives transient contention.** Copy/paste now retries when another process briefly holds the Windows clipboard open, instead of failing the operation outright.

### Performance

- **Fewer GUI stalls.** The left tab bar, status bar, top stats bar, ghost-text, and pane paint paths were reworked to cut per-frame work, keeping the UI responsive under load.

## v0.51.1 — 2026-06-20

### Fixed

- **Directory-jump finder no longer leaves a big empty gap above the results.** When the finder had more than one page of matches, the scrollbar — a tall floated block placed before the rows — pushed every result and the footer down by its full height, so the matches sat at the bottom under a large blank band. The rows now sit directly under the search field with the scrollbar floated alongside them.

## v0.51.0 — 2026-06-20

### Added

- **Native macOS glyph weight (`font_smoothing`).** The CoreText rasterizer now applies the system's grayscale font smoothing by default, so terminal and chrome text pick up the same stem-darkening as Terminal.app instead of rendering thin. New `font_smoothing` option (default on); set it to `false` for the lighter Warp-style HiDPI look. macOS only.

### Changed

- **Sidebar typography breathes.** The chrome UI font is now 13.5pt with looser row leading, so left-tab labels read larger and less cramped. The bottom footer is a single quiet "DO AI PM" caption, with a generous gap above the add-tab `+` row so it is no longer mis-clicked.

### Fixed

- **Split divider draws all the way to the edge; pane close button sits in the corner.** The vertical-split divider now spans the full pane width to the window border instead of stopping a few pixels short, and the per-pane close `×` moved out of the chrome gutter onto the pane's first content row, tucked against the top edge and the divider.

### Performance

- **Left sidebar scales to many tabs.** Per-tab metadata (title / agent / working directory) is gathered only for the visible rows instead of for every tab on every repaint, keeping windows with dozens of sessions responsive.

## v0.50.2 — 2026-06-18

### Fixed

- **Theme switching now repaints the active input row immediately.** Runtime theme changes update the window config, terminal palette, and line render caches together, so the prompt no longer stays in the previous theme until Enter is pressed.
- **Bottom status text is vertically centered.** The status bar now accounts for its own vertical padding in layout and paint, avoiding the old low-sitting text.
- **Terminal appearance polish.** Refined built-in theme palettes, sidebar spacing, active-tab contrast, and the live settings theme switch path so the UI reads more consistently across light and dark skins.

## v0.50.1 — 2026-06-17

### Fixed

- **macOS Chinese / IME input no longer flashes garbled text before settling.** The Cocoa string bridge now reads the actual UTF-8 string instead of truncating by UTF-16 code-unit length, so composed CJK text is rendered with complete bytes from the first frame.

## v0.50.0 — 2026-06-17

### Changed

- **Official site now tracks the current release.** The homepage hero badge, footer version chip, latest download URLs, and release fallback all point at `v0.50.0`, so visitors and build-time fallbacks stay aligned with the published tag.

## v0.44.4 — 2026-06-16

### Fixed

- **Left sidebar selection now reads as one surface instead of stacked overlays.** The active row uses a deeper neutral fill, the cyan marker is inline with the row content, and the outline was removed so the selection no longer looks like layered gray boxes.
- **Scrollbar interaction is reliable again.** The left sidebar scrollbar now sits flush against the edge, the thumb track matches the visible rows, and wheel / drag input stays responsive without fighting the row layout.
- **New tabs activate cleanly.** Spawning a tab no longer does a redundant second activation pass, which keeps focus and scroll position stable.

## v0.44.3 — 2026-06-15

### Fixed

- **Visible seam between the top bar and the left sidebar.** The
  v0.44.1 / v0.44.2 attempts (1.3× / 1.0× chrome height) both broke
  worse than they fixed: shortening the chrome clipped the macOS
  traffic lights and the terminal's first row, and the user's
  recurring complaint ("边栏压住顶栏") wasn't a geometry overflow at
  all — the chrome's background and the sidebar's background both
  resolved to near-identical greys (sidebar bg lifted only 5 % over
  chrome bg), so the boundary between them was invisible and the
  chrome's empty lower region read as a continuation of the sidebar.
  Restored chrome to 1.6× cell_height (the size everything was
  designed around) and added a 1 px divider line at the chrome's
  bottom edge in `bar_fg * 0.12` alpha — matches the divider already
  drawn at the sidebar's right edge, so the chrome / sidebar /
  terminal panels each read as their own surface.

## v0.44.2 — 2026-06-15

### Fixed

- **macOS chrome dead band below the traffic lights.** v0.44.1's chrome
  was still 1.6× cell height (~50 px) — the AppKit native traffic
  lights anchor to a fixed ~14 px y-offset from the window's top edge
  regardless of chrome height, so the lights sat at the top with ~22 px
  of empty bar below that nothing filled. Dropped the multiplier to
  1.0× cell height (~31 px) so the chrome matches the lights' natural
  row; codicons + stats text now share the same row as the lights and
  the sidebar's first tab sits flush against the chrome's bottom edge
  with no visible gap.

### Changed

- **Default `integrated_title_button_style` back to `MacOsNative` on
  macOS.** v0.44.1 had flipped the default to `MacOsCustom` (the new
  custom-drawn dots) to chase pixel-exact centering, but that lost the
  AppKit hover glyphs (X / − / +) and the rendered dots looked flatter
  than the OS lights. Reverted to `MacOsNative`; `MacOsCustom` stays
  available for users who explicitly want it via `unterm.lua`.

## v0.44.1 — 2026-06-15

### Added

- **`IntegratedTitleButtonStyle::MacOsCustom`** — three filled circles
  (Apple-palette red / yellow / green) drawn through our own box-model,
  so `VerticalAlign::Middle` lands them at pixel-exact chrome center.
  AppKit's native NSWindowButton widgets are anchored to a fixed
  y-offset from the window's top edge, not the chrome center, so the
  unified Warp-style top bar always left the lights off-axis (with a
  dead band either above or below depending on chrome height). The
  custom dots route clicks through the existing `TabBarItem::WindowButton`
  handler — close / minimize / zoom work identically. Default on macOS
  flipped from `MacOsNative` → `MacOsCustom`; set
  `integrated_title_button_style = "MacOsNative"` in `unterm.lua` to
  go back to OS-drawn lights (which include the X / − / + hover
  glyphs that the custom dots don't reimplement).

### Fixed

- **Stats segment overlapped the traffic lights on narrow windows.**
  The right-aligned stats text + codicon cluster floats from the right
  edge; as the window shrank past ~900 px the codicons reached the
  traffic-light reservation and the stats text started drawing under
  the lights. The stats composer now early-returns empty when
  `pixel_width < 900` — codicons stay (they're controls, not status)
  and the stats segment re-appears as soon as the window widens past
  the threshold.

## v0.44.0 — 2026-06-15

### Added

- **Per-pane top stats bar.** The tab row now carries an inline status
  strip for the active pane: branch + ahead/behind from `git status`,
  the foreground process name, its CPU / RSS / uptime, and the active
  MCP agent if one is driving the pane. Git and process info refresh
  off the render thread on a short cache so typing isn't slowed; both
  `ps` parsing (Unix) and a PowerShell `Get-Process` shim (Windows)
  feed the same pipeline. Agent detection now walks the pane's full
  process tree, so `claude` / `codex` / `gemini` get picked up even
  when they're spawned underneath a tmux or shell wrapper.
- **Sidebar footer linking to doaipm.com.** A pinned link row at the
  absolute bottom of the left tab bar — `DO AI PM · BY ZHITONG ↗` in
  the platform UI font (SF Pro on macOS) — opens the project's
  marketing site via the system browser. Caption is localized across
  all 9 shipped locales (`sidebar.author_caption`) and rendered as a
  separate, z-pinned element so it doesn't follow the tab scroll.
- **`▾` shell selector in the left tab bar.** Next to the trailing
  `+` row, a discoverable arrow opens the shell picker (login shells,
  registered profiles, AI agents) so new tabs no longer require the
  context menu. Both hit targets sized for comfortable click.
- **`tree '↑ ..' row` for parent navigation.** In tree-sidebar mode
  the first row is now a real "up one directory" entry — click to
  re-anchor the tree at the parent. Previously the only way out was
  to type the parent path manually.

### Changed

- **Chrome height + cluster geometry tightened.** Multiple passes over
  the unified top bar: traffic-light placement reuses `ui_tokens`
  instead of guessing macOS widths, the bar is centered vertically,
  Codicon spacing is even, the duplicate tab-title in sidebar mode is
  dropped, and the overall chrome height comes down ~15% without
  losing finger-target area.
- **Stats / status / tree-sidebar accents derive from the theme's
  palette.** The chrome's teal accent (clickable chips, git ahead/
  behind, process CPU/MEM) now reads from `palette.brights[6]` /
  `palette[14]` so it tracks the active scheme instead of being a
  hard-coded `#6fccb8`. Top-bar background lightens 5% over the
  scheme's bg so the chrome lifts off the pane.
- **`session.create` saved state is per-binary.** Workspace restore
  now keys session JSON on a sha1 of the running binary path, so
  the installed `Unterm.app` and the dev binary at `/tmp/Unterm-dev.app`
  don't trample each other's tabs on simultaneous use.
- **Left vertical tab bar** (`tab_bar_position = "Left"`). Tabs become
  rows in a resizable sidebar (drag the right edge, 200pt–50% window):
  each row shows the tab title over an `agent · directory` subtitle —
  which MCP agent is currently driving that pane (15-minute freshness)
  and the active pane's directory. Click activates, dragging reorders
  live, double-click renames inline (empty resets to auto-title),
  right-click opens the tab context menu, ✕ closes, a trailing `+` row
  spawns, and the wheel scrolls. The top bar stays for window buttons,
  the active title, the menu and quick actions. Toggleable from the
  View / window menus; defaults to the classic top strip.

- **Search button in the tab bar.** A magnifying-glass quick-action button
  next to tree / split / dir-jump / settings opens the scrollback search
  for the active pane — same as `Ctrl+Shift+F` / `Cmd+F`, but
  discoverable. Search was previously reachable only via keybinding or
  the Edit menu.
- **`screen.search` can now jump to its results.** New `goto: true` /
  `goto_match: N` params scroll the user-visible viewport so the match is
  on screen (with a quarter-viewport of context above it) — search-and-
  locate instead of search-and-report. Matches now carry the stable row
  index (same coordinate space as `screen.scrollback_text`) plus the
  match column, so agents can address results even as new output streams
  in. Previously the returned `row` was a bare enumeration index that
  drifted once the scrollback was trimmed, and there was no way to bring
  a match into view. The in-terminal search UI (`Ctrl+Shift+F` /
  `Cmd+F`) already jumped to matches; this brings the MCP surface to
  parity.
- **`unterm-cli screenshot --self`.** Capture only Unterm's own window
  via its CGWindowID — independent of foreground state, never depends
  on what's behind the window. Goes through the existing `capture.window`
  MCP method (defaults pid to the running server) so it works without
  any osascript / activate dance, which lets visual self-tests run
  headlessly. Pairs with the agent-side self-test workflow the global
  CLAUDE.md asks every client project to ship.
- **Workspace restore reopens your tabs, not just the window.**
  Closing Unterm with three tabs no longer comes up with a single
  fresh shell at the default cwd. Tabs beyond the first are spawned
  back into the restored window, each pointing at the cwd they were
  at on shutdown, and any user-set tab titles ride along. Falls back
  to the default cwd if the saved directory has since been deleted
  (no "no such file" bounce). Surfaces per-tab spawn failures via
  log only — one bad tab doesn't abort the rest.

### Changed

- **Unified Warp-style top bar by default.** The split chrome (gray
  native title strip above + dark tab row below) collapses into a
  single panel that adopts the active color scheme — traffic lights
  drop into the tab row, the bar tracks `palette.background` so the
  chrome no longer fights the theme, and height bumps to 2.4× the
  title cell (~60 px @ 144 dpi) to match Warp's chrome rhythm. On the
  right of the bar a six-icon action cluster + `▾` menu lands: command
  palette, tree sidebar, split, dir-jump, search, settings. All icons
  are Codicons rendered through the bundled SymbolsNerdFontMono so
  CoreText/FreeType handles the anti-aliasing — vector-grade crispness
  instead of the previous geometric polylines. Tabs now read
  `[icon] {agent or shell} · {cwd-basename}` (claude / shell / ssh
  glyph, AI agent name preferred over raw process title), and the left
  vertical tab bar's status dot becomes a 16 px circular chip with the
  agent's initial in its accent color. Users who had explicitly
  overridden `window_decorations` or `window_frame.titlebar_*` colors
  keep their overrides — the new defaults only kick in when the field
  is at its hard-coded value.
- **Chrome typography uses the platform system font.** Tab bar, sidebar
  and menu text now renders in SF Pro on macOS (Segoe UI Variable on
  Windows, Noto Sans on Linux) instead of the bundled Roboto, with the
  scale collected into `config::ui_tokens`. macOS text rendering also
  switches from subpixel-LCD to grayscale anti-aliasing — macOS removed
  subpixel AA system-wide in Mojave, so the old default produced visible
  red/blue fringing on Retina panels next to natively rendered text.
- **Search bar UX revamp.** The in-pane search bar (toolbar 🔍 /
  `Ctrl+Shift+F`) is no longer a cryptic English-only one-liner:
  - Localized in all 9 languages, with a dim "type to search…"
    placeholder and the match count + match mode right-aligned so they
    don't jitter while you type.
  - Key hints are shown in the bar itself (`Enter/↑↓ jump · Ctrl+R
    mode · Esc close`) — previously none of the search keys were
    discoverable. Hints degrade gracefully on narrow panes.
  - `Shift+Enter` now jumps in the opposite direction of `Enter`.
  - Interactive search now defaults to **ignore-case** (Ctrl+R still
    cycles exact / regex). Agent-driven `screen.search` is unchanged.
  - Typing latency: the per-keystroke search debounce dropped from
    350 ms to 100 ms, so first highlights appear ~3× sooner.
  - The pattern restored from your last search (or seeded from the
    selection) is replaced wholesale by the first character you type,
    instead of being appended to — backspace/arrows still edit it.
  - The bar now opens instantly: the seeded-pattern re-search is
    deferred off the open path, live pane output re-triggers the scan
    at most twice a second instead of every frame, and the chunked
    scan no longer queues a full window repaint per 1000 rows.
  - Switching matches centers the hit in the viewport (it used to hug
    the screen edge), and the active match is bold + underlined on top
    of its highlight color so it stands apart from the other matches.
  - The search-box cursor blinks, so the typing position is findable
    on the reverse-video bar.
  - **Search opens in one frame, every time.** Two more delays found by
    instrumenting the open path: (1) the copy/search overlay never
    reported its own dirty rows through `get_changed_since`, so the
    renderer's line cache treated the bar, the highlights, and the
    active-match change as "nothing changed" — the search UI literally
    waited for an unrelated repaint; keystrokes now also dirty the bar
    row directly, so typing echoes instantly instead of after the
    search debounce. (2) The localized bar's CJK + ↑ ↓ · glyphs paid
    a few hundred ms of first-time font-fallback resolution exactly on
    first open; those glyphs are now pre-shaped at idle right after the
    window opens. Measured open-to-painted: 33 ms cold (was 230 ms+,
    occasionally seconds).
  - **Overlays paint immediately.** Opening search (or quick select /
    any pane overlay) never requested a repaint, so the bar waited for
    the next incidental one — a cursor blink or pane output — which is
    why it felt slow no matter how fast the search itself got.
  - **Click a match to jump to it.** Left-clicking any highlighted
    match makes it the active one and centers it; previously a click
    silently wiped the match selection. Clicks elsewhere still do
    normal text selection. Inactive matches render dimmed so the
    active one stands apart.

### Fixed

- **`unterm-cli screenshot --self` no longer falls back to a
  full-screen capture right after launch.** Immediately after
  `unterm start`, the NSWindow exists but CGWindowList hasn't yet
  flagged it onScreen, so the lookup returned None and the MCP
  handler silently captured the screen instead — agents doing visual
  self-tests would diff against whatever was behind Unterm. For
  self-targets the lookup now retries 5× with 120 ms gaps (~600 ms
  ceiling). External-pid captures still single-shot so a typo'd pid
  doesn't block.
- **Windows: multi-second freeze at launch (and on every new tab) when the
  proxy toggle is on but the proxy app isn't running.** System-proxy
  auto-detection ran twice on the GUI thread before the first prompt, and
  each pass swept 8 well-known local proxy ports serially — on Windows a
  TCP connect to a closed loopback port only fails after winsock's internal
  retry, so every closed port ate its full 120 ms timeout (~1 s per sweep,
  ~2.5 s total). Detection stages now run concurrently, the port sweep is
  parallel, and results are cached for 5 s and shared by the startup,
  spawn, ▼ menu, and Web Settings paths. Worst-case cold detection is now
  ~150 ms; spawns within the cache window are free.
- **Proxy auto-detection no longer scans ports 8080 / 8888.** These are
  far more often dev servers than proxies, and the scan's only signal is
  a successful TCP connect — a dev server on 8080 would be "detected" as
  a proxy and injected as `HTTP_PROXY` into every spawned shell, cutting
  that shell off from the network. Proxies genuinely listening there can
  still be configured explicitly in `proxy.json` (manual mode).
- **Top stats bar: invisible-render bugs.** Two issues at once kept the
  inline strip blank on first paint: the cell width was computed as
  `pixel_cell = 0` (the strip collapsed to zero-width before text was
  measured), and the strip's z-index sat under the pane so even a sized
  strip was painted over by terminal content. Width now uses the real
  cell metrics; the strip is composed into the chrome row itself
  (zindex 20) so it draws above the pane and behind the click targets.
- **`ps` output parsing in the stats bar handled runs of whitespace
  incorrectly.** `splitn(4, char::is_whitespace)` emits empty fragments
  for runs of spaces, so the RSS column landed in the comm slot and
  the process name showed up as a number. Switched to
  `split_whitespace()`, which collapses runs the way macOS / Linux
  `ps` formats actually want.

## v0.16.0 — 2026-05-18

Two-stage AI-friendly terminal release: locks down the MCP write
surface, adds proposal-style AI integration, and ships fish-style
inline ghost text plus a read-only insights panel.

### MCP security & audit (P0)

- **Every `session.input` / `exec.send` is audited**. The audit log
  records timestamp, method, pane id, content preview (truncated +
  escape-aware), and the calling agent's identity. Cap 1 000 entries
  (configurable via `mcp_audit_log_capacity`).
- **`agent.identify` / `agent.whoami`** — agents self-tag a
  connection so audit entries group by name instead of by socket.
- **First-time-per-agent confirmation banner**. When a new agent
  first writes to the PTY a blocking banner appears above the
  status bar:
  `⚠ <agent> wants to write to pane #N: <preview>` ·
  `[Enter] allow [Esc] block [Alt+A] always allow`.
  Worker thread parks until the user decides or the policy timeout
  (default 30 s) expires (auto-block). Policy configurable via
  `mcp_input_confirmation = Always | FirstTimePerAgent (default) | Never`.
- **Status bar chip `mcp:N`** shows cumulative MCP PTY writes. Click
  copies the recent audit log as JSON to the clipboard.
- New config: `mcp_input_confirmation`, `mcp_confirmation_timeout_ms`,
  `mcp_audit_log_capacity`, `mcp_suggest_queue_capacity`,
  `mcp_suggest_default_ttl_ms`, `mcp_trusted_agents`.

### MCP suggest API + UI (P1)

- **`session.suggest{,_status,_cancel,_list}`** — agents propose
  text that **never reaches the PTY directly**. Lifecycle:
  Pending → Accepted / Dismissed / Expired / Cancelled. Default
  TTL 60 s. Queue capacity 256.
- **Suggest bar UI** — single half-transparent row above the
  status bar showing `✨ <agent>: <text>` plus the keybind hints.
- **Default keybindings**:
  - `Tab` — accept suggestion (passes through to shell completion
    when no suggestion is pending).
  - `Esc` — dismiss suggestion / block pending confirmation
    (passes through to vim, less, etc. when neither is pending).
  - `Alt+Enter` — accept suggestion and append `\n`.
  - `Enter` — allow pending confirmation (passes through when no
    banner is up).
  - `Alt+A` — always-allow the calling agent for this session.
- Acceptance / dismissal is audited under
  `session.suggest.accept` / `session.suggest.dismiss`
  with `agent="user"`.

### Ghost Text (fish-style inline completion)

- Per-pane keystroke observer tracks the current input buffer.
  Inline grey italic continuation rendered to the right of the
  cursor when the buffer prefix-matches a previously-committed
  command.
- **Right Arrow / End** — accept the prediction (writes the
  continuation to the PTY). Returns `Unhandled` when no prediction
  is showing so the keys still move the cursor.
- **Cross-pane history pool** (512 commits) so freshly opened
  panes have something to predict from day one.
- **↑/↓ history navigation, ←/→, Tab, Home** all clear the
  ghost buffer to keep it in sync with what the shell rewrites.
- **OSC 133 prompt detection** — when shell integration is on,
  the overlay only renders inside `Input` semantic zones; TUI
  applications and command output are left alone.
- **`ghost.debug` MCP method** for inspecting per-pane buffer,
  prediction, and cross-pane commit pool from outside the GUI.

### Insights panel

- Read-only dashboard bound to **`Ctrl+Shift+I`** (or
  `KeyAssignment::ShowInsights`). Shows:
  - Shell, cwd, terminal size for the active pane.
  - Most recent 10 commands across all panes.
  - Top-5 most-typed commands (cross-pane frequency).
  - MCP write counters, time since last write, agents seen,
    pending suggestions / confirmations.
  - Last 8 audit log entries.
- Pure local data, no AI, no network round-trip.
- `q` / `Esc` / `Ctrl+C` to dismiss.

### Misc

- Status bar `mcp:N` chip click now copies up to 200 audit entries
  as JSON to the clipboard. The full overlay (Ctrl+Shift+A) is a
  follow-up.
- `agent_first_input` flag added to audit log entries when a new
  agent first hits a PTY-writing method, even when the policy
  doesn't block.
- Lock order documented: `registry()` (per-pane ghost state)
  before `global_commits()` (cross-pane pool). Future contributors
  please honor this — concurrent observe calls would deadlock if
  the order flips.

### Known limitations

- Ghost Text only learns from Enter-committed commands; commands
  recalled via shell history (↑/↓) don't enter the pool, so users
  who lean heavily on history navigation will see fewer
  predictions until they type a command at least once.
- The `Ctrl+Shift+A` audit log overlay is still placeholder — the
  status bar chip copies JSON to clipboard as a stand-in.
- AI Ghost Text (Claude API-driven completion) is intentionally
  not shipped — slated for v0.17.
