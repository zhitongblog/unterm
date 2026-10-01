---
layout: ../../layouts/Doc.astro
title: Identity profiles — one window per identity
subtitle: Bind a window to GitHub, AWS, npm, OpenAI, and SSH credentials at once. Open new windows from CLI. Let agents see (but not write) which identity they're running under.
kicker: Docs / Profiles
date: 2026-05-11
---

## What problem this solves

You have a personal GitHub account and a work GitHub account. You have a personal AWS profile and a work AWS profile. You have an OpenAI key paid for by your startup and one paid for personally. You have an `~/.ssh/work_ed25519` and an `~/.ssh/personal_ed25519`. Every few hours you do something in the wrong identity — push to the wrong fork, `npm publish` to the wrong registry, run `aws s3 sync` against a bucket that belongs to the other client.

The conventional answer is some combination of `direnv` plus shell hooks plus `gh auth switch` plus `aws sso login --profile work` plus `ssh-add -D && ssh-add ~/.ssh/work_ed25519`. That works, sort of, if you're the kind of person who reads `man direnv`.

Unterm bundles all of those concerns into one concept: a **profile**. One window in Unterm is bound to one profile. Every shell in that window inherits the profile's credentials. New window for a different identity. The chip in the tab bar tells you which one you're in. You never lose track.

If `direnv` is the power-user answer, profiles are the answer for everyone else — including the power user when they're tired.

## The mental model

**One window = one identity.** You don't switch profiles inside a window. You open a new window. This mirrors how browsers do profile isolation, and it sidesteps a class of bugs where an existing shell keeps the old env after a switch.

A profile binds together:

- **Static env vars** like `NODE_ENV=production`, `AWS_REGION=us-east-1`.
- **Secrets** — tokens and keys stored in the OS-native vault (Keychain on macOS, Credential Manager on Windows, libsecret over D-Bus on Linux). The TOML files under `~/.unterm/profiles/` only contain *references* to keychain entries (`keychain://unterm/work-acme/GITHUB_TOKEN`). The raw token bytes never touch your dotfile directory, so the TOML is safe to commit.
- **Git identity** — `GIT_AUTHOR_NAME`, `GIT_AUTHOR_EMAIL`, `GIT_COMMITTER_*`. Injected as env rather than rewriting `~/.gitconfig`, so your global git config stays untouched.
- **SSH key routing** — an opt-in fragment at `~/.unterm/ssh/config.unterm` that adds `Match host` blocks. When the profile is active, `ssh github.com` uses that profile's private key. When it's not, your normal SSH config applies.
- **An accent color** that tints the chip in the tab bar so you can tell identities apart visually.

Unterm windows are also independent instances of the app — `alpha`, `bravo`, `charlie`, … (see [multi-instance](/docs/multi-instance/)). Profiles are *orthogonal* to that: two windows can carry the same profile, one window has exactly one profile, and an agent can ask "which profile is the alpha window bound to?" via MCP.

## Quick start in four commands

```bash
# 1. Create a profile. Type whatever you want — display name, CJK, emoji, em-dash.
#    Unterm derives a safe internal ID automatically (you never see it).
unterm-cli profile create "Work — Acme"

# 2. Store a token in the OS keychain. Hidden /dev/tty prompt by default.
unterm-cli profile set-secret "Work" GITHUB_TOKEN
#  → Enter value for GITHUB_TOKEN (input hidden): ········

# 3. Open a new Unterm window bound to this profile. Every pane inside
#    inherits UNTERM_PROFILE=work-acme, GITHUB_TOKEN=<the secret>,
#    GIT_AUTHOR_NAME=... etc.
unterm-cli profile spawn "Work"

# 4. Make this profile the default for new windows so `unterm` (no flag)
#    auto-binds. Optional — without a default, plain `unterm` starts un-bound.
unterm-cli profile set-default "Work"
```

That's the whole loop. No `~/.gitconfig` edits, no `.bashrc` exports, no `chmod 600` on a config file.

## Importing existing credentials

Most users already have credentials sitting in tool-specific config files. Unterm scans seven sources and lists what it finds — entirely read-only, never modifying source files:

```bash
unterm-cli profile import
```

The seven sources:

| Source   | What we look at                                  |
| -------- | ------------------------------------------------ |
| `gh`     | `~/.config/gh/hosts.yml` — accounts (token or not) |
| `aws`    | `~/.aws/credentials` — every `[profile]` section |
| `npm`    | `~/.npmrc` — `_authToken` lines, scope mappings  |
| `ssh`    | `~/.ssh/{config, id_*, *_ed25519, *_rsa, ...}`   |
| `docker` | `~/.docker/config.json` `auths` map              |
| `gcloud` | `gcloud config configurations list --format=json` |
| `netrc`  | `~/.netrc` machine/login/password tuples         |

Each candidate shows whether we could read the value or whether it sits in another keychain we can't access (modern `gh` stores tokens in macOS Keychain itself, for example). Where the value is available, the wizard (CLI for now; GUI wizard in v0.14) will offer to copy it into Unterm's own `unterm` service in your OS vault. Where it isn't, you paste the token once and Unterm stores it.

Either way, **the source file is never modified**. Your existing `gh auth` continues to work as before; Unterm's keychain entry is in addition to, not instead of.

## How env injection works

When an Unterm window is bound to a profile, every pane spawned in it goes through `apply_unterm_profile_env` in `unterm-services/src/launch_env.rs`:

1. Read `~/.unterm/instances/<id>.json` for this instance.
2. If `profile` field is set, load the profile registry from `~/.unterm/profiles/`.
3. For each entry in the profile's `[secrets]` table, parse the `keychain://unterm/<id>/<env>` URL and fetch the value from the OS vault.
4. Build an env map containing `UNTERM_PROFILE` + static `[env]` + decoded secrets + `GIT_AUTHOR_*` / `GIT_COMMITTER_*` derived from `[git]`.
5. Overlay onto the `CommandBuilder` env. Profile values override anything the caller set, because the explicit profile choice is more authoritative than process-inherited env.

If any step fails (keychain locked, individual secret missing, profile TOML unparseable), Unterm logs a warning and continues with whatever did resolve. **A shell with partial profile env is more useful than no shell at all.** The `profile.audit` MCP method surfaces unresolved secrets so an agent (or future GUI chip) can flag the problem visibly.

## How SSH routing works

Profiles can map host patterns to private key paths:

```toml
# ~/.unterm/profiles/work-acme.toml
[ssh]
"github.com" = "~/.ssh/keys/work_acme_ed25519"
"gitlab.acme.example" = "~/.ssh/keys/work_acme_ed25519"
```

Whenever a profile is created, edited, or deleted, Unterm regenerates `~/.unterm/ssh/config.unterm` with the corresponding `Match host` blocks:

```text
Match host github.com exec "test \"$UNTERM_PROFILE\" = \"work-acme\""
    IdentityFile /Users/alex/.ssh/keys/work_acme_ed25519
    IdentitiesOnly yes
```

The `exec` directive runs the test command in your shell environment when `ssh` consults its config. Inside a pane bound to `work-acme`, `UNTERM_PROFILE` is set to `work-acme`, the test passes, and `ssh github.com` is restricted to that one key (`IdentitiesOnly yes` prevents `ssh-agent` from offering other keys). Outside that profile, the Match misses and your normal SSH config applies. **Unterm's routing is additive, never destructive.**

For these blocks to take effect, your main `~/.ssh/config` must include the Unterm fragment:

```text
Include ~/.unterm/ssh/config.unterm
```

That one line is the only manual SSH setup Unterm asks of you. The future onboarding wizard will offer a one-click add; for now, paste it in yourself.

## The TOML schema

Each profile is one TOML file at `~/.unterm/profiles/<id>.toml`. You normally never edit these by hand — `unterm-cli` and the GUI manage them — but the format is human-readable so you can audit, version-control, or fix things via `unterm-cli profile edit` (which opens `$EDITOR`).

```toml
display_name = "Work — Acme"
accent_color = "#3b82f6"
description = "Acme Corp work account"

[git]
user_name = "Alex Lee"
user_email = "alex@acme.example"
signing_key = "keychain://unterm/work-acme/gpg-signing"   # optional

[env]                                       # plaintext, non-secret
NODE_ENV = "production"
AWS_REGION = "us-east-1"

[secrets]                                   # references only, never raw tokens
GITHUB_TOKEN          = "keychain://unterm/work-acme/GITHUB_TOKEN"
AWS_ACCESS_KEY_ID     = "keychain://unterm/work-acme/AWS_ACCESS_KEY_ID"
AWS_SECRET_ACCESS_KEY = "keychain://unterm/work-acme/AWS_SECRET_ACCESS_KEY"
ANTHROPIC_API_KEY     = "keychain://unterm/work-acme/ANTHROPIC_API_KEY"

[ssh]
"github.com" = "~/.ssh/keys/work_acme_ed25519"

[gh_host]                                   # pin gh CLI to a specific account per host
"github.com" = "alex-acme"

[npm]
registry = "https://npm.pkg.github.com/@acme"   # scoped private registry

[expiration]                                # optional; chip shows red dot ≤ 7 days
GITHUB_TOKEN = "2026-09-15"
```

Field ordering matters for human-readability — when you open the file in `$EDITOR`, `display_name` and `accent_color` are at the top so you immediately know what profile you're looking at. Empty sections are omitted from the on-disk file.

There's a sibling file at `~/.unterm/profiles/index.toml`:

```toml
order = ["personal", "work-acme", "side-project"]
default = "work-acme"
```

`order` is the display order for the chip dropdown. `default` is the profile new Unterm windows bind to when launched without a `--profile` flag. Both are optional — Unterm falls back to alphabetical ordering and "no default" respectively.

## CLI reference

```text
unterm-cli profile list                        # all profiles, * marks default
unterm-cli profile current                     # which profile this window is on
                                               #   (proxies to MCP profile.current)
unterm-cli profile create <display_name>       # creates profile, auto-derives ID
                                               #   --accent "#RRGGBB"
unterm-cli profile show <name>                 # full details, never shows secret values
unterm-cli profile set-secret <profile> <ENV>  # hidden prompt; --from-stdin to pipe
unterm-cli profile delete <name>               # removes TOML + keychain entries; -y to skip prompt
unterm-cli profile audit                       # secrets expiring ≤ 7 days
unterm-cli profile edit <name>                 # opens $EDITOR on the TOML file
unterm-cli profile export <name>               # shell-eval'able script with raw values
                                               #   ⚠ writes secrets to stdout
unterm-cli profile spawn <name> [--cwd <dir>]  # opens a new Unterm window bound to profile
unterm-cli profile import [--source X]         # scans 7 sources for existing credentials
unterm-cli profile set-default <name>          # makes <name> the default for new windows
                                               #   pass `-` to clear
```

Every name argument accepts the display name, the slugified ID, or a unique case-insensitive prefix. So all of these resolve to the same profile:

```
unterm-cli profile spawn "Work — Acme"
unterm-cli profile spawn "work-acme"
unterm-cli profile spawn work               # prefix
unterm-cli profile spawn Work               # case-insensitive prefix
```

If a prefix is ambiguous (two profiles both starting with "work"), the CLI errors and asks you to be more specific.

## MCP namespace

Agents driving Unterm from outside see three read-only methods:

```jsonc
profile.list      → { profiles: [{id, display_name, accent_color, description,
                                   secret_count, expiration_count, is_default}, ...],
                      default: "work-acme" }
profile.current   → { instance: "alpha", profile: "work-acme" }
profile.audit     → { warnings: [{profile, display_name, env_name, expires_on,
                                  days_remaining}, ...],
                      healthy_count: 12 }
```

Write methods (`create`, `set-secret`, `delete`) and `profile.spawn` are intentionally **not** exposed via MCP. Profile management is CLI-only on purpose:

- It keeps the agent attack surface narrow — no plausible MCP call can write to keychain.
- Spawning a new GUI window from within the MCP server process is awkward; the CLI's existing `unterm-cli profile spawn` already does the right thing by exec'ing a fresh `unterm` process.
- Agents that want to "switch identity" can simply run `unterm-cli profile spawn X` via their existing shell tooling.

This is the same write-narrow / read-wide pattern Unterm uses for `instance.*` and recording APIs.

## Keychain integration per OS

| Platform | Backend                       | Where to inspect                              |
| -------- | ----------------------------- | --------------------------------------------- |
| macOS    | Keychain (SecItem generic-password) | Keychain Access app, filter on **Service** = `unterm` |
| Windows  | Credential Manager (CredRead/CredWrite) | Control Panel → Credential Manager → Windows Credentials, search for `unterm/...` |
| Linux    | Secret Service via D-Bus (libsecret) | `seahorse` (GNOME Keyring) or `kwalletmanager` (KDE) — look under the default collection for `unterm/<id>/<env>` |

All three backends are exposed through the same `SecretStore` trait in the `unterm-profile` crate, backed by the [`keyring`](https://crates.io/crates/keyring) crate. Switching from one OS to another requires no profile changes — copy the TOML files to the new machine and re-enter the secrets via `set-secret` (we can't transport keychain entries between OSes directly).

**Headless Linux** without a desktop session has no Secret Service to talk to. The store backend returns `BackendUnavailable` in that case. Unterm logs the limitation and the window starts without profile env injection — your shells run with your normal env instead. You can still run `unterm-cli profile list / show / edit` because the TOML files don't depend on the vault; only `set-secret` / `get` need a working backend.

## What an agent sees inside the shell

Pop a shell inside a profile-bound Unterm window and inspect the environment:

```bash
$ env | grep -E "^(UNTERM_PROFILE|GITHUB_TOKEN|GIT_AUTHOR|AWS_)"
UNTERM_PROFILE=work-acme
GITHUB_TOKEN=ghp_redactedRealValue
GIT_AUTHOR_NAME=Alex Lee
GIT_AUTHOR_EMAIL=alex@acme.example
GIT_COMMITTER_NAME=Alex Lee
GIT_COMMITTER_EMAIL=alex@acme.example
AWS_ACCESS_KEY_ID=AKIAREDACTED
AWS_SECRET_ACCESS_KEY=secretvalueredacted
```

`UNTERM_PROFILE` is the canonical signal — every other env var follows from it. Shell hooks, agent tooling, and CI scripts can branch on its value:

```bash
# In ~/.zshrc
case "$UNTERM_PROFILE" in
  work-acme) export NODE_ENV=production ;;
  personal)  export NODE_ENV=development ;;
esac
```

Agents reading `screen.text` (see [MCP reference](/docs/mcp-reference/)) will see the `UNTERM_PROFILE` in the shell prompt if your prompt theme includes it (most don't by default). A future "destructive command guard" (v0.14) will pop a confirmation when a profile-bound shell tries to run `gh repo delete`, `aws s3 rb`, `npm unpublish`, or `git push --force` to the wrong remote.

## Destructive-command guard

A profile-aware safety net for the handful of operations that genuinely can't be undone — `gh repo delete`, `aws s3 rb`, `npm unpublish`, `git push --force`, `vercel rm`, and friends. Run any of these inside a profile-bound shell and Unterm intercepts the call to ask for confirmation:

```text
[Unterm guard] profile=work-acme
[Unterm guard] About to run: gh repo delete unzooai/disposable
[Unterm guard] gh repo delete — proceed? [y/N]
```

Setup is one line in your `.bashrc` / `.zshrc`:

```sh
[[ -n "$UNTERM_PROFILE" ]] && eval "$(unterm-cli profile shell-integration zsh)"
```

(Use `bash` or `fish` in place of `zsh` to match your shell.) The `[[ -n "$UNTERM_PROFILE" ]]` guard means the integration only loads inside Unterm windows bound to a profile — your other shells (cron, SSH-to-server, plain Terminal.app) are unaffected.

**Bypass for known-safe scripts:** prefix the binary with `command` to skip the wrapper:

```sh
# Skip the prompt — useful in disposal scripts where you know the repo is safe to delete
command gh repo delete unzooai/disposable --yes
```

**What's wrapped vs not:**

| Wrapper | Triggers a prompt for | Passes through |
| ------- | --------------------- | -------------- |
| `gh`    | `repo delete`, `repo archive` | everything else |
| `aws`   | `s3 rb`, `s3api delete-bucket`, `s3 rm --recursive` | everything else |
| `npm`   | `unpublish` | everything else |
| `git`   | `push --force`, `push -f` | `push --force-with-lease` (safe variant), all non-push |
| `vercel` | `rm`, `remove` | everything else |

The list is curated rather than exhaustive on purpose — the goal is to catch the few commands that have ruined people's afternoons, not to wrap every potentially-meaningful operation. If you have a tool that should be on this list, open an issue.

## FAQ

**Will switching profile change my git identity in existing shells?**
No — profile env is injected at shell-spawn time. An already-running shell keeps the env it was born with. New panes in the same window get the current profile's env. New windows get whichever profile they're bound to. To pick up new git identity in an existing shell, exit it and open a fresh pane.

**Can I have two windows with the same profile?**
Yes. Each window is its own *instance* (alpha, bravo, …) with its own NATO name. Multiple instances can bind to the same profile. They share keychain entries (read-only) and don't conflict.

**Where do the actual token bytes live?**
In your OS's native vault, never in `~/.unterm/`. Treat the `unterm-profile` TOML files like `.gitconfig`: safe to commit, safe to share, contain no secrets. The OS vault is responsible for permission gating — Unterm asks the keychain for the value at spawn time, and the OS may prompt you to authorize the first read on a new build.

**Can I sync profiles across machines via dotfiles?**
Profile *configuration* (the TOML files), yes — push them through `chezmoi` or whatever you use. Profile *secrets*, no — those have to be re-entered on each machine because the keychain is OS-specific. The `unterm-cli profile export <name>` command emits a shell-eval'able script that you can transfer over a secure channel and re-import, but this is a one-shot migration aid, not a sync mechanism.

**Can I edit the TOML by hand?**
Yes — `unterm-cli profile edit <name>` is just `$EDITOR <path>`. Unterm reloads the file on the next CLI run / window spawn. The CLI also re-validates the TOML parses correctly after `$EDITOR` exits, so a syntax error gets caught before it can break your next launch.

**What about non-token credentials, like AWS short-lived SSO tokens or kerberos tickets?**
We deliberately skip those. AWS SSO tokens refresh every few hours; copying them into Unterm's keychain would race the SDK's own refresh and produce broken sessions. Kerberos tickets work at a different layer than env vars. For these, run the SDK's own login command inside a profile-bound shell — the profile environment (e.g. `AWS_PROFILE=work-acme`) tells the SDK which credentials to use; the actual token management stays in the SDK.

**What goes in `[env]` vs `[secrets]`?**
Anything you'd be comfortable putting in a dotfile commit goes in `[env]`. Anything you'd rotate quarterly, anything that grants write access to a production system, anything you'd panic about leaking to a screen recording goes in `[secrets]`. When in doubt, `set-secret` it.

**What if I'm on Linux SSH'd into a server with no desktop session?**
Profiles still work for the TOML side (list / show / edit), but `set-secret` and env injection both need a Secret Service backend you don't have. The chip will eventually hide itself in this case (v0.14). For now you'll see a warning in the log and the window starts without env injection — exactly as if no profile were bound.

## See also

- [CLI reference](/docs/cli-reference/) — every other `unterm-cli` subcommand
- [MCP reference](/docs/mcp-reference/) — what agents see from outside
- [Multi-instance](/docs/multi-instance/) — NATO names, `instance.list`, and how profiles interact with the instance system
- [Configuration](/docs/configuration/) — `~/.unterm/` layout, env vars, file paths
- [Config file reference](/docs/config-reference/) — every key in `unterm.conf`
