---
layout: ../../layouts/Doc.astro
title: Shell integration
subtitle: How Unterm learns where prompts start, which command is running and how it ended — in the shells it starts, without touching your own startup files.
kicker: Docs / Shell integration
date: 2026-10-01
---

A terminal sees bytes. Shell integration is the shell telling it what those bytes are: here a prompt begins, here the command line ends, here a command starts running, here it finished with exit status 2. Those are the `OSC 133` marks (`A`, `B`, `C`, `D;<status>`), plus `OSC 7` for the working directory.

Unterm uses them to:

- mark a tab whose last command **failed** (non-zero exit) with the sidebar's ▲, and *not* mark one whose output merely contains the word "error";
- jump between prompts in the scrollback;
- split a [recording](/docs/configuration) into commands;
- clear a program's progress bar when its prompt returns.

## What Unterm does

When Unterm starts zsh, bash, fish or PowerShell, it loads a small script into it. Your own files are read exactly as they would be anywhere else.

| Shell | How the script is loaded |
|---|---|
| zsh | `ZDOTDIR` points at Unterm's `.zshenv`, which puts your own `ZDOTDIR` back first, sources your `.zshenv`, and adds `precmd`/`preexec` hooks. zsh then reads your `.zprofile` and `.zshrc` as usual. |
| bash | Started with `--rcfile`, a file that reads the login files (`/etc/profile`, then the first of `~/.bash_profile`, `~/.bash_login`, `~/.profile`) for a login shell, or `~/.bashrc` otherwise, and then adds the marks through `PROMPT_COMMAND`, `PS1` and `PS0`. No `DEBUG` trap, so yours or bash-preexec's is left alone. |
| fish | A `vendor_conf.d` script found through `XDG_DATA_DIRS`. |
| PowerShell | `-NoExit -Command ". unterm.ps1"`, which wraps the prompt your profile set. The command-start mark is added to Enter only while Enter still does what PSReadLine ships with. |

A shell given arguments of its own — `bash -c "…"`, `pwsh -File build.ps1` — is running something specific and is left alone. So is every other program.

The scripts are built into Unterm and written to `~/.unterm/shell-integration/` the first time they are needed.

## Turning it off

```ini
shell_integration = false
```

in `~/.unterm/unterm.conf`, and new shells start exactly as configured.

## Remote shells

Integration is loaded into shells Unterm starts. A shell on another machine reached through `ssh` does not get it; to have the marks there, source the matching script from `~/.unterm/shell-integration/` in that machine's startup file.
