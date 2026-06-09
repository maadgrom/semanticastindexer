# Installation

SAI ships as a single self-contained binary. The quickest path is the one-line installer,
which downloads a prebuilt binary from the latest GitHub Release — **no Rust toolchain
required**. You can also wire up a coding agent in the same step, or build from source if
there's no release for your platform.

## Quick install (per OS)

```bash
# macOS / Linux
curl -fsSL https://maadgrom.github.io/semanticastindexer/install.sh | bash
```

```powershell
# Windows (PowerShell)
powershell -c "irm https://github.com/maadgrom/semanticastindexer/releases/latest/download/semanticastindexer-installer.ps1 | iex"
```

Prefer to pick your OS interactively? Use the hosted install page:
**[maadgrom.github.io/semanticastindexer](https://maadgrom.github.io/semanticastindexer/)**.

On macOS/Linux the installer downloads the binary, then **asks which coding agent(s) to
connect** (reading your keypress straight from the terminal, so the prompt works even under
`curl | bash`). Press Enter to skip the prompt and just install the binary — it's a full CLI
on its own. See the [CLI reference](reference/cli.md) to start indexing immediately.

## Connect your coding agent

Connecting an agent is optional. On macOS/Linux, add `--platform <id>` and `install.sh` wires
up that client's MCP config (and, for Claude Code, installs the `semantic-code-search-mcp`
skill into `~/.claude/skills/`):

```bash
curl -fsSL https://maadgrom.github.io/semanticastindexer/install.sh | bash -s -- --platform cursor
```

Supported ids: `claude-code`, `claude-desktop`, `cursor`, `windsurf`, `continue`, `codex`,
`hermes`, `ollama`, `generic`.

| Flag | Effect |
| ---- | ------ |
| `--platform <id>` | Connect one client non-interactively. |
| `--all` | Connect every supported client in one run. |
| `--non-interactive` | Don't prompt — install the binary and print a generic MCP block. |
| `--write` | Merge the config into the client's JSON file (best-effort, with a `.bak` backup). |
| `--collection <name>` | Collection name baked into the snippet (default: `source_code`). |
| `--embedder <id>` | `ort` or `ollama` (default: `ort`; the `ollama` client forces `ollama`). |
| `--skip-binary` | Emit config only; don't install the binary. |

By default — without `--write` — the installer **prints** the config snippet and the exact
target file path so you can paste it yourself. The merge with `--write` only applies to
JSON-based clients; for Continue (YAML) and Codex (TOML) the installer always prints the block
to paste. On Windows, install the binary first, then paste the printed block into your client.

> When no client is selected, no tty is available, or you pass `--non-interactive`, the
> installer prints a `generic` MCP block. The generated snippet runs the server as
> `semanticastindexer mcp --backend duckdb --embedder <id> --collection <name>` with `cwd` set
> to the directory you ran the installer from.

For the full per-client walkthrough, see [MCP clients](integrations/mcp-clients.md).

## Per-platform config locations

| Platform | Config file | Notes |
| -------- | ----------- | ----- |
| **Claude Code** | project `.mcp.json` + skill in `~/.claude/skills/semantic-code-search-mcp/` | Full skill experience |
| **Claude Desktop** | `~/Library/Application Support/Claude/claude_desktop_config.json` (macOS) | Linux: `~/.config/Claude/claude_desktop_config.json` |
| **Cursor** | `~/.cursor/mcp.json` | Or project `.cursor/mcp.json` |
| **Windsurf / Cascade** | `~/.codeium/windsurf/mcp_config.json` | JSON config |
| **Continue.dev** | `~/.continue/config.yaml` | `mcpServers` block (YAML, paste manually) |
| **Codex CLI** | `~/.codex/config.toml` | `[mcp_servers.semantic-code-search]` (TOML, paste manually) |
| **Hermes** | client-specific MCP config | Installer prints a generic block to paste |
| **Ollama** | n/a (embedding backend) | Installs the binary configured with `--embedder ollama`; run `ollama serve` + `ollama pull nomic-embed-text` |
| **Generic / manual** | your client's MCP config | Paste the printed `.mcp.json` block |

The MCP server entry is always registered under the name `semantic-code-search`. For the tool
surface it exposes — `sai_search_code`, `sai_find_similar`, `sai_find_duplicates`,
`sai_index_status`, `sai_prepare_mcp_setup`, and `sai_refresh` — see the
[MCP server and tools](reference/mcp-server.md) reference.

## Embeddings

The DuckDB backend embeds **locally** via a pluggable embedder (`embedder: ort | ollama`):

- **`ort` (default) — on-device ONNX Runtime.** No server, no API keys. The model is pulled
  from Hugging Face on first run: the code-trained
  [`jina-embeddings-v2-base-code`](https://huggingface.co/jinaai/jina-embeddings-v2-base-code)
  (161M params, 768-dim), or
  [`intfloat/multilingual-e5-small`](https://huggingface.co/intfloat/multilingual-e5-small)
  (118M, 384-dim) as the zero-config text default. Swap in any
  [ONNX embedding model on Hugging Face](https://huggingface.co/models?pipeline_tag=feature-extraction&library=onnx)
  by setting `model` plus a matching `vector_dim`.
- **`ollama` — embedding server over HTTP.** Point at a local or remote Ollama server. Handy
  in **CI/CD**, where an embedding service often already runs: `ollama serve`,
  `ollama pull mxbai-embed-large`, set `ollama.url` + `ollama.model`, and index. Browse
  [embedding models on Ollama](https://ollama.com/search?c=embedding).

See [backends and embedders](reference/backends-and-embedders.md) for the full matrix and
[choosing a model](guides/choosing-a-model.md) for the recommended code model for
de-duplication.

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
pins `stable`, so `rustup` auto-activates the latest stable when you build in this repo.

Then run the one-command setup script to register the MCP server:

```bash
./mcp-setup/setup.sh --non-interactive --backend duckdb --embedder ort
```

## Security

- The Qdrant **API key** is read only from the `QDRANT_API_KEY` environment variable (a
  secret — never commit it). The cluster URL can be set in `indexer.yaml` (`qdrant.url`)
  or via `QDRANT_URL`.
- If an API key is ever exposed, **rotate it** in the cluster's *API Keys* tab.
- Add `target/` to `.gitignore` (build artifact).
- The MCP server is **read-only by default**; the write tool (`sai_refresh`) requires
  `--allow-write`.

See [security and privacy](operations/security.md) for the full threat model and the
[environment variables](reference/configuration.md#environment-variables) reference for every credential SAI reads.

## Uninstall

```bash
curl -fsSL https://maadgrom.github.io/semanticastindexer/uninstall.sh | bash
```

The uninstaller reverses what `install.sh` did. Pass `--yes` (or `-y`) to skip the
confirmation prompt; in a non-interactive shell (CI) it proceeds without asking.

**Removed:**

- The `semanticastindexer` binary from `~/.cargo/bin` and `~/.local/bin` (plus any
  `code-search-mcp` wrapper alongside it).
- The Claude Code skill directory `~/.claude/skills/semantic-code-search-mcp/`.
- The `semantic-code-search` entry from known JSON MCP configs — Claude Desktop, Cursor,
  Windsurf, and the project's `./.mcp.json` — each backed up to `<file>.bak` before editing.

**Left untouched** (delete by hand if you want them gone):

- Per-project index files (`.index/`) and any `indexer.yaml`.
- The Codex (`~/.codex/config.toml`) and Continue (`~/.continue/config.yaml`) entries.
- Any PATH line the installer added to your shell rc (`~/.zshrc`, `~/.bashrc`, `~/.profile`).

If you're troubleshooting a stale config after reinstalling, see the
[glossary](concepts/glossary.md) for the meaning of each backend and embedder key.
