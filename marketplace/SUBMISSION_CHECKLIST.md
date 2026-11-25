# Submission checklist

Where to post Unterm's MCP server (and the Skills bundles), in priority
order. Each row: the marketplace, the submission method, the materials
required from this kit, and a status box you can tick as you go.

Legend: ☐ not yet submitted · ⏳ submitted, awaiting review · ✅ live

## MCP server marketplaces

| Status | Marketplace | URL | Submission method | Required materials |
| :-: | --- | --- | --- | --- |
| n/a | **Anthropic / MCP official registry** | <https://github.com/modelcontextprotocol/servers> | Checked — repo README only lists Anthropic-authored reference servers, no community section. Community discovery happens at the awesome-* lists below. Skip. | — |
| ☐ | **Smithery.ai** | <https://smithery.ai/new> | `smithery.yaml` already at repo root for auto-discovery — see [submissions/01-smithery.md](submissions/01-smithery.md) for the sign-in + claim flow. | `smithery.yaml` (root) + `submissions/01-smithery.md` |
| ☐ | **mcp.so** | <https://mcp.so/submit> | Form submission — paste-ready field values in [submissions/02-mcp-so.md](submissions/02-mcp-so.md) | `submissions/02-mcp-so.md` |
| ✅ | **Glama.ai / mcp** | <https://glama.ai/mcp/servers/zhitongblog/unterm> | Live & driving referrer traffic (`glama.ai/mcp/servers/zhitongblog/unterm`). | GitHub topics |
| ✅ | **PulseMCP** | <https://www.pulsemcp.com> | Auto-indexed from GitHub `mcp-server` topic. | GitHub topics |
| ⏳ | **awesome-opencode-market** | <https://github.com/awesome-market/awesome-opencode-market/issues/3185> | Review issue #3185 opened automatically on 2026-11-14. | (auto) |
| ✅ | **sudosubin/agents.nix** | <https://github.com/sudosubin/agents.nix/pull/132785> | Automatically tracks Unterm releases (updated through `0.71.26`). | (auto) |
| ✅ | **patelflow/scoop-bucket-tools** | <https://github.com/patelflow/scoop-bucket-tools> | Indexed in `bucket/unterm.json` for Windows Scoop users. | (auto) |
| n/a | **wong2/awesome-mcp-servers** | <https://github.com/wong2/awesome-mcp-servers> | PRs disabled on repo (zero PR history of any state; `has_issues: false` too — maintainer accepts direct pushes only). Skip. | — |
| ⏳ | **awesome-mcp-servers** (punkpeye) | <https://github.com/punkpeye/awesome-mcp-servers> | PR #7166 opened — adds Unterm to the Command Line section. <https://github.com/punkpeye/awesome-mcp-servers/pull/7166> | (auto) |
| ☐ | **Claude Desktop catalog** | Built-in: Settings → MCP → Browse | Auto-discovers from `claude_desktop_config.json` snippet users paste in. No separate submission. | `marketplace/mcp/configs/claude-desktop.json` |
| ☐ | **Cursor MCP directory** | <https://cursor.com/mcp> | Editorial — email or Discord. Drafts in [submissions/03-cursor.md](submissions/03-cursor.md) | `submissions/03-cursor.md` |
| ☐ | **OpenAI / ChatGPT MCP catalog** | <https://platform.openai.com/docs/mcp> | Currently invite-only / partner-only. Hold. | TBD when GA. |

### GitHub repo prep (one-time)

Before submitting anywhere, set on `github.com/zhitongblog/unterm`:

- **Topics**: `mcp`, `mcp-server`, `model-context-protocol`, `terminal`,
  `tty`, `claude`, `cursor`, `agent-tools`, `wezterm` (auto-picked by
  Glama and several other indexers).
- **Repo description** (right sidebar): the one-line tagline from
  `marketplace/README.md`.
- **Repo website link**: <https://unterm.app>.
- **Pinned releases**: keep the latest minor tagged release pinned.

## Skills directories

The Anthropic Skills format (`SKILL.md` + YAML frontmatter) is shared
via GitHub repos rather than a single registry. Two paths:

| Status | Where | How |
| :-: | --- | --- |
| ✅ | **In-repo `skills/` directory** (this kit) | Users `git clone zhitongblog/unterm` and copy `marketplace/skills/<name>/` into `~/.claude/skills/<name>/`. | files are at `marketplace/skills/` |
| ⏳ | **ComposioHQ/awesome-claude-skills** | PR #960 — adds the 3 skills to "Development & Code Tools" section. <https://github.com/ComposioHQ/awesome-claude-skills/pull/960> | (auto) |
| ☐ | **Claude.ai shared skills** (when sharing opens GA) | Upload via Claude Settings → Skills → Share | each `SKILL.md` text |

The three Skills authored:

1. **`terminal-control`** — base recipe for any agent that wants to drive a
   terminal via Unterm MCP. The "Hello, World" skill.
2. **`release-engineer`** — opinionated end-to-end release workflow
   (version bump → tests → tag → push → notarize → upload → verify).
   Replicates what `release-mac.sh` codifies, in agent-driven form.
3. **`parallel-agents`** — fan a task out to 3+ inner agents in side-by-side
   panes (one Claude, one Codex, one Gemini), then reconcile.

## After-submission housekeeping

Each marketplace listing should:

- Link **Download** to <https://unterm.app/api/download> (UA-aware redirect
  to the right OS artifact — single canonical URL).
- Link **Docs** to <https://unterm.app/docs/mcp-reference> (live MCP method
  reference).
- Quote the **headline metric**: "67 methods across 11 namespaces".
- Mention the **no-config-needed launch path**: if the user installs
  Unterm and uses *Shell → AI Agents* to launch an inner agent, the MCP
  is auto-wired with no copy-paste.

## Tracking submissions

When you submit, edit this file: flip ☐ → ⏳ and add the PR / submission URL
in a footnote. When it lands, flip ⏳ → ✅. This file becomes the audit
trail.
