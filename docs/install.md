# Installation

The fastest path is the hosted **install page**, which gives a copy-paste one-liner per
platform:

👉 **[maadgrom.github.io/semanticastindexer](https://maadgrom.github.io/semanticastindexer/)**

```bash
# Generic example — pick your platform on the page for the exact command:
curl -fsSL https://maadgrom.github.io/semanticastindexer/install.sh | bash -s -- --platform generic
```

`install.sh` downloads a prebuilt binary from the latest GitHub Release (no Rust toolchain
needed), then prints the ready-to-paste MCP config for your platform (and, for Claude Code,
installs the `semantic-code-search-mcp` skill into `~/.claude/skills/`). Supported platform
ids: `claude-code`, `claude-desktop`, `cursor`, `windsurf`, `continue`, `codex`, `hermes`,
`ollama`, `generic`.

Pass `--write` to let the script merge the config into your client's config file
(best-effort, with a backup); omit it to just print the snippet and the exact target path.

## Per-platform config locations

| Platform | Config file | Notes |
| -------- | ----------- | ----- |
| **Claude Code** | project `.mcp.json` + skill in `~/.claude/skills/semantic-code-search-mcp/` | Full skill experience |
| **Claude Desktop** | `~/Library/Application Support/Claude/claude_desktop_config.json` (macOS) | Linux: `~/.config/Claude/…` |
| **Cursor** | `~/.cursor/mcp.json` | Or project `.cursor/mcp.json` |
| **Windsurf / Cascade** | `~/.codeium/windsurf/mcp_config.json` | |
| **Continue.dev** | `~/.continue/config.yaml` | `mcpServers` block |
| **Codex CLI** | `~/.codex/config.toml` | `[mcp_servers.semantic-code-search]` (TOML) |
| **Ollama** | n/a (embedding backend) | Installs the binary configured with `--embedder ollama`; run `ollama serve` + `ollama pull nomic-embed-text` |
| **Generic / manual** | your client's MCP config | Paste the printed `.mcp.json` block |

## Build from source

If you prefer to build the binary yourself (or there's no release for your platform), build
with **all features enabled** so every capability is present:

```bash
# Recommended — full-featured binary (everything included)
cargo build --release --features all
```

```bash
# Also fine (equivalent)
cargo build --release --features "qdrant,ort,ollama,ast,mcp"
```

The binary lands at `./target/release/semanticastindexer`. The first build is slower because
`--features all` pulls in native dependencies (bundled DuckDB + ONNX Runtime via `ort`).
Subsequent builds are fast thanks to cargo's incremental compilation.

**Requirements:** Rust **stable** toolchain (edition 2024, MSRV 1.85). A `rust-toolchain.toml`
pins `stable` so `rustup` auto-activates the latest stable when you build here.

Then run the [one-command setup script](mcp-server.md#one-command-setup-script):

```bash
./mcp-setup/setup.sh --non-interactive --backend duckdb --embedder ort
```

## Security

- Credentials are read only from `QDRANT_URL` / `QDRANT_API_KEY` — never commit them.
- If an API key is ever exposed, **rotate it** in the cluster's *API Keys* tab.
- Add `target/` to `.gitignore` (build artifact).
- The MCP server is **read-only by default**; the write tool (`refresh`) requires `--allow-write`.
