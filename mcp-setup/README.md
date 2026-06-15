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

- `setup.sh` — Main setup script (build-from-source; interactive + `--non-interactive` for
  agents, plus `--platform <id>`/`--write` to wire any MCP client)
- `lib/mcp-config.sh` — **shared** MCP client-wiring module (snippet generators +
  `configure_platform`). Canonical source consumed by `setup.sh`; the curl-piped
  `docs/install.sh` keeps a byte-identical inline copy, kept honest by `tests/test_setup.sh`.
- `templates/` — the **single source of truth** for generated config. `setup.sh` copies
  `templates/sai-cfg.yml` and patches the backend/embedder/collection lines, rather than
  emitting its own divergent inline YAML.
- `tests/test_setup.sh` — asserts artifact paths, generated config fields, command strings,
  and `install.sh`↔`lib` snippet parity.

The skill definitions live at the repo-root `.agents/skills/` (the agentskills.io standard
location, portable across Claude Code and other agent runtimes): `sai` (setup) and `sai-deslop`
(usage + duplicate triage). The Claude Code `dedup-auditor` subagent lives in `.claude/agents/`.
`setup.sh` installs all of them.

## Philosophy

- **Local by default** — DuckDB + Ollama or ort is the recommended path for agentic use.
- **Agent-friendly** — The script is designed to be driven by LLMs via `--non-interactive` mode.
- **Zero magic** — Everything is explicit, logged, and reproducible.

## Contributing

Improvements to the setup experience (better defaults, more client templates, smarter feature detection, etc.) are very welcome.
