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
- `--embedder ort|ollama` (ort = fully offline, bigger build)
- `--non-interactive` (perfect for agent-driven setup)
- `--install-global` (puts binary in `~/.local/bin` + creates `sai` wrapper)
- `--target-dir`, `--collection`, `--features`

## Generated Artifacts

The script creates (in the target project):

- `sai-cfg.yml` — tuned for agentic code search (smart excludes, good similarity thresholds). The MCP server reads it for backend/embedder/collection, so its `args` are just `["mcp", "--config", "sai-cfg.yml"]`.
- `.mcp.json.example` — ready for Claude Code / generic MCP clients
- `claude-desktop-config.example.json`

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

## Security & Privacy Notes

- Default recommendation is **local DuckDB** (nothing leaves the machine).
- Qdrant Cloud path requires `QDRANT_URL` + `QDRANT_API_KEY` in the environment.
- Search is **read-only by default**. The write tools (`sai_refresh`, `sai_sync`) require the explicit `--allow-write` flag when starting the server.

## Files

- `.agents/skills/sai/SKILL.md` — this file (the portable Agent Skill definition; Claude Code installs a copy into `~/.claude/skills/sai/`)
- `mcp-setup/setup.sh` — the robust, agent-friendly setup script
- `mcp-setup/templates/` — example configuration templates

## Related Project Documentation

- [Installation & per-platform setup](../../../book/src/installation.md)
- [MCP server: tools, thresholds, wiring](../../../book/src/reference/mcp-server.md)
- [Chunking strategies](../../../book/src/reference/chunking.md) and [similarity tuning](../../../book/src/reference/configuration.md)
- Hosted install page: <https://maadgrom.github.io/semanticastindexer/>

This skill is maintained as part of the semanticastindexer repository.
