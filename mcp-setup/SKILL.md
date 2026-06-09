---
name: semantic-code-search-mcp
description: Installs and configures semanticastindexer as a powerful semantic code search MCP server for any agentic coding system (Claude, Cursor, Windsurf, Continue, etc.). Provides local, private, high-recall code search with optional AST awareness.
version: "1.0.0"
author: semanticastindexer project
tags: [mcp, code-search, embeddings, semantic-search, setup, agentic]
---

# Semantic Code Search MCP Setup Skill

This skill turns the `semanticastindexer` binary into a first-class MCP server that any agentic coding tool can use for deep semantic understanding of a codebase.

## What You Get

- Local-first semantic search (DuckDB + Ollama or fully offline ONNX via ort)
- Optional symbol-aware chunking with tree-sitter (AST)
- Works great with Claude Code, Cursor, Windsurf/Cascade, Continue.dev, and any stdio MCP client
- Private — nothing leaves your machine unless you choose Qdrant Cloud

## Quick Start (Recommended for Agents)

```bash
cd /path/to/semanticastindexer
./mcp-setup/setup.sh --non-interactive --backend duckdb --embedder ollama
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
- `--install-global` (puts binary in `~/.local/bin` + creates `code-search-mcp` wrapper)
- `--target-dir`, `--collection`, `--features`

## Generated Artifacts

The script creates (in the target project):

- `indexer.yaml` — tuned for agentic code search (smart excludes, good similarity thresholds)
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
4. The agent now has powerful tools:
   - `search_code`
   - `find_similar` (by code snippet or by existing function)
   - `find_duplicates`
   - `index_status`
   - `prepare_mcp_setup` (can be called later to help set up semantic search in new projects)
   - `refresh` (when started with `--allow-write`)

When the semanticastindexer MCP server is running with `--allow-setup`, the `prepare_mcp_setup` tool can actually execute the setup script on demand.

## Security & Privacy Notes

- Default recommendation is **local DuckDB** (nothing leaves the machine).
- Qdrant Cloud path requires `QDRANT_URL` + `QDRANT_API_KEY` in the environment.
- The MCP server is **read-only by default**. Write access (`refresh` tool) requires explicit `--allow-write` flag when starting the server.

## Files in This Skill

- `setup.sh` — The main robust, agent-friendly setup script
- `templates/` — Example configuration templates
- `SKILL.md` — This file (self-documenting skill)

## Related Project Documentation

- [Installation & per-platform setup](../docs/install.md)
- [MCP server: tools, thresholds, wiring](../docs/mcp-server.md)
- [Chunking strategies](../docs/chunking.md) and [similarity tuning](../docs/configuration.md)
- Hosted install page: <https://maadgrom.github.io/semanticastindexer/>

This skill is maintained as part of the semanticastindexer repository.
