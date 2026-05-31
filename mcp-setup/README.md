# semanticastindexer MCP Setup

This directory contains everything needed to turn `semanticastindexer` into a first-class semantic code search MCP server for any agentic coding system.

## Quick Start

```bash
# From the root of this repository
./mcp-setup/setup.sh
```

See `SKILL.md` for the full skill description and agent usage patterns.

## Contents

- `setup.sh` — Main setup script (supports interactive + `--non-interactive` for agents)
- `SKILL.md` — Skill definition for Grok / Claude-style skill systems
- `templates/` — Example configuration files

## Philosophy

- **Local by default** — DuckDB + Ollama or ort is the recommended path for agentic use.
- **Agent-friendly** — The script is designed to be driven by LLMs via `--non-interactive` mode.
- **Zero magic** — Everything is explicit, logged, and reproducible.

## Contributing

Improvements to the setup experience (better defaults, more client templates, smarter feature detection, etc.) are very welcome.
