# Unterm — marketplace submission kit

Everything needed to list Unterm's MCP server and accompanying Skills on the
major AI-agent marketplaces. Use this as the canonical source — when a
marketplace asks for a manifest, a config snippet, or an install URL, point
them here (or copy the relevant file in).

## What's in this kit

```
marketplace/
├── README.md                          ← you are here
├── SUBMISSION_CHECKLIST.md            ← per-marketplace status + how to submit
├── mcp/
│   ├── manifest.json                  ← canonical MCP server descriptor (Smithery-style)
│   ├── configs/                       ← drop-in config for each MCP client
│   │   ├── claude-desktop.json
│   │   ├── claude-code.json
│   │   ├── cursor.json
│   │   ├── codex.toml
│   │   ├── gemini.json
│   │   ├── opencode.json
│   │   └── README.md                  ← which file goes where, troubleshooting
│   └── examples/                      ← (reserved for runnable snippets)
└── skills/                            ← Anthropic Skills bundles using Unterm MCP
    ├── terminal-control/SKILL.md
    ├── release-engineer/SKILL.md
    └── parallel-agents/SKILL.md
```

## Quick pitch for any listing

> **Unterm** — *the terminal AI agents can drive.* A cross-platform
> (macOS / Linux / Windows) pure-Rust GPU terminal and multi-agent cockpit
> that exposes itself as an MCP-controllable surface: **155 methods across
> 16 namespaces** let any external agent spawn shells, split panes, run
> commands with structured JSON results, delegate sub-tasks across CLI
> agents (`agent_session.delegate`), create zero-pollution Git checkpoints
> (`refs/unterm/checkpoints`), read scrollback, capture headless scrolling
> screenshots, and approve actions from your phone over LAN/Tailscale
> (`Unterm Pocket`). Local-first (127.0.0.1 + auth-token + per-call audit),
> MIT, $0, signed + notarized.

## Headline numbers (for marketplace cards)

| | |
| --- | --- |
| Methods | **155 across 16 namespaces** (`v0.71.26`) |
| Transports | `unterm-cli mcp-stdio` (recommended) · raw TCP JSON-RPC on `127.0.0.1` |
| Platforms | macOS (universal, signed + notarized DMG) · Linux x86_64 (`.deb` + AppImage) · Windows 10/11 (`.msi` + portable `.zip`) |
| License | MIT |
| Price | $0 forever — no paid tier, no subscription, no in-app purchase |
| Telemetry | None |
| Supported AI clients | Claude Code · Claude Desktop · Codex CLI · Gemini CLI · Cursor · Windsurf · Antigravity (`agy`) · OpenCode · Aider (1-click MCP auto-wiring in Web Settings) |
| Repo | <https://github.com/zhitongblog/unterm> |
| Homepage | <https://unterm.app> |
| Docs | <https://unterm.app/docs/mcp-reference> |

## The download-first user flow

When a user discovers Unterm MCP on a marketplace, the install path is:

1. **They see the listing** — manifest in this kit gives them name, tagline,
   capabilities, screenshots.
2. **They click "Install" / "Get"** → opens <https://unterm.app/api/download>.
   This is a Cloudflare Pages Function that detects User-Agent at the edge
   and 302s to the right artifact for the visitor's OS (`.dmg` / `.deb` /
   `.AppImage` / `.msi` / `.zip`). One link, every platform.
3. **They install + open Unterm once** so the MCP server boots and writes
   `~/.unterm/active.json` (port + auth token, chmod 600).
4. **They paste the config snippet** for their MCP client (Claude Desktop,
   Cursor, etc.) — see `mcp/configs/`. The `unterm-cli mcp-stdio` bridge
   reads the auth file transparently; no manual credential wiring.
5. **They restart their client.** It now has 67 Unterm tools. The first
   `meta.surface` call returns the full live API.

Skip step 4 entirely if they launch their agent **from Unterm**
(`unterm-cli agent launch <id>` or Shell → AI Agents → ...) — Unterm
auto-wires the right config block to the agent's native location with
`preserve_unknown_keys` semantics. This is the lowest-friction path; lean
on it in the listing copy.

## What "MCP + corresponding Skills" means

| Layer | What it is | For whom |
| --- | --- | --- |
| **MCP server manifest** (`mcp/manifest.json`) | Machine-readable descriptor of the 67 methods, 5 install artifacts, security model, links | Marketplaces that auto-index (Smithery, mcp.so, …) |
| **Client config snippets** (`mcp/configs/`) | Drop-in JSON/TOML for each major MCP-speaking client | End users + marketplaces' "copy config" buttons |
| **Skills bundles** (`skills/*/SKILL.md`) | Anthropic-Skills-format procedural knowledge that *uses* the MCP — recipes for the common workflows | Users of Claude.ai / Claude Code / any agent that supports the Skills format. Other vendors can fork them. |

The Skills are written to be **agent-agnostic** — they use only the MCP
surface (no Claude-specific syntax in the recipes). Vendors can adapt the
`SKILL.md` format to their own (Cursor rules, Gemini prompts, OpenCode
recipes) by lifting the recipe blocks verbatim.

## Submitting — see `SUBMISSION_CHECKLIST.md`

The checklist lists every marketplace we target, the submission method
(PR to a GitHub list / form / API), the required materials (which files
from this kit), and the current submission status.

## Versioning

The `manifest.json` carries `"version"` mirroring the current Unterm
release tag. Bump it in the same commit that bumps the rest of the
project (see the `release-engineer` skill for the full sweep). Any
marketplace that ingests the manifest re-reads the version on next fetch.

## License of this kit

MIT (same as Unterm itself). Other apps are welcome to fork the structure,
the SKILL.md drafting style, and the manifest schema. Attribution
appreciated but not required.
