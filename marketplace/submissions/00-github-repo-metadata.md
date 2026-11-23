# GitHub repo metadata (manual — admin-only)

I tried to set these via `gh api PATCH` but my collaborator token returns 404.
**You need to do this once** at <https://github.com/zhitongblog/unterm/settings>
(takes 30 s and is the highest-leverage step — it auto-feeds Glama, drives
GitHub Topics discovery, and improves every marketplace listing).

## 1. Description (the right-sidebar one-liner)

```
The terminal AI agents can drive — a pure-Rust terminal & multi-agent cockpit with 155 MCP methods across 16 namespaces. macOS / Linux / Windows, local-first, $0, MIT.
```

## 2. Website

```
https://unterm.app
```

## 3. Topics (Glama and several others auto-index from these)

Paste these into the "Topics" field of the repo settings, comma-separated:

```
mcp, mcp-server, model-context-protocol, terminal, tty, pty, claude, claude-code, cursor, codex, gemini-cli, opencode, antigravity, ai-agents, agent-tools, rust, macos, linux, windows
```

## 4. Releases — confirm latest is pinned

The Releases page (<https://github.com/zhitongblog/unterm/releases>) should show
**v0.71.26+** at top with all platform artifacts. The smart-download function
at `unterm.app/api/download` already routes here.

## Why these three matter

| Field | Impact |
| --- | --- |
| **Description** | Shows up under your repo name on GitHub search, in `awesome-mcp-servers` cards, and in the Cursor / Smithery preview cards |
| **Website** | Auto-linked from every repo card; turns `unterm.app` into the discovery anchor |
| **Topics** | **Glama** and several other indexers auto-pull from `mcp-server` topic — no separate submission needed |
