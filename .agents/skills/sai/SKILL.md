---
name: sai
description: Installs and configures semanticastindexer as a near-duplicate detector and semantic code search MCP server for any agentic coding system (Claude, Cursor, Windsurf, Continue, etc.). Provides local, private, high-precision duplication detection with optional AST awareness.
version: "1.0.0"
author: semanticastindexer project
tags: [mcp, near-duplicate, deslop, sai, embeddings, agentic]
---

# Semantic Code Search MCP Setup Skill

This skill turns the `semanticastindexer` binary into a first-class MCP server that any agentic coding tool can use to find near-duplicates and deeply understand a codebase.

## What You Get

- High-precision near-duplicate detection across functions (`sai_find_duplicates`)
- Local-first semantic search (DuckDB + fully offline ONNX via ort, or Ollama)
- Optional symbol-aware chunking with tree-sitter (AST)
- Works great with Claude Code, Cursor, Windsurf/Cascade, Continue.dev, and any stdio MCP client
- Private — nothing leaves your machine unless you choose Qdrant Cloud

## Quick Start (Recommended for Agents)

```bash
cd /path/to/semanticastindexer
./mcp-setup/setup.sh --non-interactive --backend duckdb --embedder ort
```

Then index the current project:

```bash
cd /path/to/your/project
/path/to/semanticastindexer/target/release/semanticastindexer
```

## Full Interactive Setup

```bash
./mcp-setup/setup.sh
```

Options the script supports:

- `--backend duckdb|qdrant`
- `--embedder ort|ollama` (ort = fully offline, default)
- `--non-interactive` (perfect for agent-driven setup)
- `--platform <id>` — also wire up a client: `claude-code`, `claude-desktop`, `cursor`,
  `windsurf`, `continue`, `codex`, `hermes`, `generic` (repeatable)
- `--write` — merge the config into the client's file directly (JSON clients; backs up first)
- `--install-global` (puts binary in `~/.local/bin` + creates `sai` wrapper)
- `--target-dir`, `--collection`, `--features`

This is the **build-from-source** path. To install a **prebuilt** binary instead and wire a
client in one line (no Rust toolchain), use the released installer:

```bash
curl -fsSL https://maadgrom.github.io/semanticastindexer/install.sh | bash -s -- --platform cursor --write
```

Both paths share the same MCP client-wiring module, so the experience is identical.

## Generated Artifacts

The script creates (in the target project):

- `sai-cfg.yml` — copied from `mcp-setup/templates/sai-cfg.yml` (the single source of truth),
  patched for the chosen backend/embedder/collection. The MCP server reads it, so its `args`
  are just `["mcp", "--config", "sai-cfg.yml"]`.
- `.mcp.json.example` / `claude-desktop-config.example.json` — generated from the same shared
  builder used by the installer (so they never drift).

## Wiring any client (not just Claude)

The MCP server is **vendor-agnostic** (stdio, `sai_`-prefixed tools). Pass `--platform`/`--write`
to wire a client automatically, or paste the snippet into the right file yourself:

| Client | Config file |
|--------|-------------|
| Claude Code | `<project>/.mcp.json` (or `claude mcp add sai …`) |
| Claude Desktop | `~/Library/Application Support/Claude/claude_desktop_config.json` (macOS) |
| Cursor | `~/.cursor/mcp.json` |
| Windsurf | `~/.codeium/windsurf/mcp_config.json` |
| Continue | `~/.continue/config.yaml` (YAML form) |
| Codex | `~/.codex/config.toml` (TOML form) |

The portable Agent Skills live in `.agents/skills/` — Claude Code auto-loads copies from
`~/.claude/skills/`; other tools read the same `SKILL.md` from the repo. See
[mcp-clients](../../../book/src/integrations/mcp-clients.md) for the authoritative per-client guide.

## Recommended Feature Sets

We strongly recommend `--features all` (the default used by the setup script). This
gives you every backend, every embedder, the MCP server, and the AST chunker in one
binary.

| Use Case                    | Command                                      |
|----------------------------|----------------------------------------------|
| Recommended (everything)   | `--features all` (default)                   |
| Fully offline + AST        | `--backend duckdb --embedder ort` (uses all) |
| Lightweight (still full)   | `--backend duckdb --embedder ollama` (uses all) |

## After Setup – Typical Agent Workflow

1. The agent calls this skill/setup script.
2. The agent runs the indexer on the user's current project (with `--dry-run` first).
3. The agent (or user) adds the generated MCP server entry to their client config.
4. The agent now has powerful tools (all `sai_`-prefixed so they stand apart from other
   MCP servers' tools in the agent's tool list):
   - `sai_search_code`
   - `sai_find_similar` (by code snippet or by existing function)
   - `sai_find_duplicates`
   - `sai_index_status`
   - `sai_prepare_mcp_setup` (can be called later to help set up semantic search in new projects)
   - `sai_refresh` (re-index specific files; requires `--allow-write`)
   - `sai_sync` (reconcile the index with the working tree, like the CLI `sync`; requires `--allow-write`)

When the semanticastindexer MCP server is running with `--allow-setup`, the `sai_prepare_mcp_setup` tool can actually execute the setup script on demand.

5. To use those tools **well**, follow the companion **`sai-deslop`** skill
   ([.agents/skills/sai-deslop/SKILL.md](../sai-deslop/SKILL.md)) — it covers when to call each
   tool and a triage protocol for judging duplicate/similarity findings before acting. For
   repo-wide audits in Claude Code, delegate to the **`dedup-auditor`** subagent
   ([.claude/agents/dedup-auditor.md](../../../.claude/agents/dedup-auditor.md)).

## Security & Privacy Notes

- Default recommendation is **local DuckDB** (nothing leaves the machine).
- Qdrant Cloud path requires `QDRANT_URL` + `QDRANT_API_KEY` in the environment.
- Search is **read-only by default**. The write tools (`sai_refresh`, `sai_sync`) require the explicit `--allow-write` flag when starting the server.
- **Do not add `--allow-setup` (or `--allow-write`) to a default/committed MCP config.** `--allow-setup` lets `sai_prepare_mcp_setup` run a build script (`bash -c …`); it is high-trust and should only be enabled deliberately, interactively, when you actually want the agent to run setup.

## Files

- `.agents/skills/sai/SKILL.md` — this file (the portable Agent Skill definition; Claude Code installs a copy into `~/.claude/skills/sai/`)
- `.agents/skills/sai-deslop/SKILL.md` — the **usage + triage** skill: when to reach for the `sai_` tools while coding and how to triage every duplicate/similarity finding before acting (portable; installed to `~/.claude/skills/sai-deslop/`)
- `.claude/agents/dedup-auditor.md` — Claude Code subagent that runs the repo-wide near-duplicate sweep with the triage protocol in an isolated context and returns a classified digest (installed to `~/.claude/agents/`)
- `mcp-setup/setup.sh` — the robust, agent-friendly setup script (installs the binary, all skills, and the subagent)
- `mcp-setup/templates/` — example configuration templates

## Related Project Documentation

- [Installation & per-platform setup](../../../book/src/installation.md)
- [MCP server: tools, thresholds, wiring](../../../book/src/reference/mcp-server.md)
- [Chunking strategies](../../../book/src/reference/chunking.md) and [similarity tuning](../../../book/src/reference/configuration.md)
- Hosted install page: <https://maadgrom.github.io/semanticastindexer/>

This skill is maintained as part of the semanticastindexer repository.
