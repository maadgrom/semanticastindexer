# Installation

## Quick install (per OS)

Downloads a prebuilt binary from the latest GitHub Release. No Rust toolchain needed.

```bash
# macOS / Linux
curl -fsSL https://maadgrom.github.io/semanticastindexer/install.sh | bash

# Windows (PowerShell)
powershell -c "irm https://github.com/maadgrom/semanticastindexer/releases/latest/download/semanticastindexer-installer.ps1 | iex"
```

Or pick your OS on the hosted install page:
👉 **[maadgrom.github.io/semanticastindexer](https://maadgrom.github.io/semanticastindexer/)**

## Connect your coding agent

Optional, the binary is a full CLI on its own. On macOS/Linux, add `--platform <id>` and
`install.sh` wires up that client's MCP config (and, for Claude Code, installs the
`semantic-code-search-mcp` skill into `~/.claude/skills/`):

```bash
curl -fsSL https://maadgrom.github.io/semanticastindexer/install.sh | bash -s -- --platform cursor
```

Supported ids: `claude-code`, `claude-desktop`, `cursor`, `windsurf`, `continue`, `codex`,
`hermes`, `ollama`, `generic`. Pass `--write` to merge the config into your client's config
file (best-effort, with a backup); omit it to just print the snippet and the exact target
path. On Windows, install the binary above, then paste the printed block into your client.

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

## Embeddings

The DuckDB backend embeds **locally** via a pluggable embedder (`embedder: ort | ollama`):

- **`ort` (default) — on-device ONNX Runtime.** No server, no API keys. The model is pulled
  from Hugging Face on first run: the code-trained
  [`jina-embeddings-v2-base-code`](https://huggingface.co/jinaai/jina-embeddings-v2-base-code)
  (161M params, 768-dim), or [`intfloat/multilingual-e5-small`](https://huggingface.co/intfloat/multilingual-e5-small)
  (118M, 384-dim) as the zero-config text default. Swap in any
  [ONNX embedding model on Hugging Face](https://huggingface.co/models?pipeline_tag=feature-extraction&library=onnx)
  (set `model` + a matching `vector_dim`).
- **`ollama` — embedding server over HTTP.** Point at a local or remote Ollama server. Handy in
  **CI/CD**, where an embedding service often already runs: `ollama serve`,
  `ollama pull mxbai-embed-large`, set `ollama.url` + `ollama.model`, and index. Browse
  [embedding models on Ollama](https://ollama.com/search?c=embedding).

See [backends & embedders](backends-and-embedders.md) for the full matrix and the recommended
code model for de-duplication.

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

**Requirements:** Rust **stable** toolchain (edition 2024, MSRV 1.88). A `rust-toolchain.toml`
pins `stable` so `rustup` auto-activates the latest stable when you build here.

Then run the [one-command setup script](mcp-server.md#one-command-setup-script):

```bash
./mcp-setup/setup.sh --non-interactive --backend duckdb --embedder ort
```

## Security

- Credentials are read only from `QDRANT_URL` / `QDRANT_API_KEY` — never commit them.
- If an API key is ever exposed, **rotate it** in the cluster's *API Keys* tab.
- Add `target/` to `.gitignore` (build artifact).
- The MCP server is **read-only by default**; the write tool (`sai_refresh`) requires `--allow-write`.

## Uninstall

```bash
curl -fsSL https://maadgrom.github.io/semanticastindexer/uninstall.sh | bash
```

Removes the binary (`~/.cargo/bin` / `~/.local/bin`), the Claude Code skill
(`~/.claude/skills/semantic-code-search-mcp/`), and the `semantic-code-search` entry from
known JSON MCP configs (Claude Desktop, Cursor, Windsurf, and `./.mcp.json`), backing each up
to `<file>.bak`. Pass `--yes` to skip the confirmation prompt.

Left untouched (delete by hand if you want them gone): per-project index files (`.index/`) and
`indexer.yaml`, the Codex (`~/.codex/config.toml`) and Continue (`~/.continue/config.yaml`)
entries, and any PATH line the installer added to your shell rc.
