# semanticastindexer MCP Setup

This directory contains everything needed to turn `semanticastindexer` into a first-class near-duplicate detector and semantic code search MCP server for any agentic coding system.

## Quick Start

```bash
# From the root of this repository
./mcp-setup/setup.sh
```

See [`.agents/skills/sai/SKILL.md`](../.agents/skills/sai/SKILL.md) for the full skill
description and agent usage patterns (the portable Agent Skill definition; installers copy it
into `~/.claude/skills/sai/`).

## Contents

- `setup.sh` — Main setup script (supports interactive + `--non-interactive` for agents)
- `templates/` — Example configuration files

The skill definition itself lives at the repo-root `.agents/skills/sai/SKILL.md` (the
agentskills.io standard location, portable across Claude Code and other agent runtimes).

## Philosophy

- **Local by default** — DuckDB + Ollama or ort is the recommended path for agentic use.
- **Agent-friendly** — The script is designed to be driven by LLMs via `--non-interactive` mode.
- **Zero magic** — Everything is explicit, logged, and reproducible.

## Contributing

Improvements to the setup experience (better defaults, more client templates, smarter feature detection, etc.) are very welcome.
