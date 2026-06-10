# MCP clients

`semanticastindexer mcp` runs a Model Context Protocol (MCP) server over stdio, exposing
the `sai_`-prefixed tools (`sai_search_code`, `sai_find_similar`, `sai_find_duplicates`,
`sai_index_status`, `sai_refresh`) to any agentic coding client. This page is a per-client
integration guide: where each client's config file lives, the exact server snippet, the
`install.sh --platform <id>` command that wires it, and how to restart, verify, and
test-drive the connection.

For the tool reference (arguments, thresholds, write tool) see
[../reference/mcp-server.md](../reference/mcp-server.md). For installing the binary first,
see [../installation.md](../installation.md). If a client doesn't list the tools after a
restart, jump to [../operations/troubleshooting.md](../operations/troubleshooting.md).

## Before you start

1. **Install the binary** ([../installation.md](../installation.md)). Note its absolute
   path — an absolute `command` path is the safest choice in every client config below.
   When built from source the binary lands at `./target/release/semanticastindexer`.
2. **Index the project once** before starting the server.

The server defaults to `--backend duckdb --embedder ollama` and is **read-only by
default**. The snippets below pass `--embedder ort` for the fully offline ONNX embedder;
drop that flag (or set `--embedder ollama`) if you run an Ollama embedding server. The
server's `cwd` must be the indexed project root so it finds that project's DuckDB index
and `indexer.yaml`.

## Using the installer to wire a client

On macOS/Linux the install script can do the wiring for you. Add `--platform <id>`:

```bash
curl -fsSL https://maadgrom.github.io/semanticastindexer/install.sh | bash -s -- --platform cursor
```

Supported ids: `claude-code`, `claude-desktop`, `cursor`, `windsurf`, `continue`,
`codex`, `hermes`, `ollama`, `generic`.

- By default the installer **prints** the config snippet and the exact target path so you
  can paste it yourself.
- Pass `--write` to merge the snippet into your client's config file (best-effort, with a
  backup).
- Run the installer **without** `--platform` and it can prompt interactively to pick a
  client.
- On Windows: install the binary first, then paste the printed block into your client's
  config by hand.

The server entry is named `semantic-code-search` in the configs the installer manages.

---

## Claude Code

**Config:** project `.mcp.json` (in the repository you want to search), plus the
`semantic-code-search-mcp` skill installed into
`~/.claude/skills/semantic-code-search-mcp/`.

The `--platform claude-code` install gives you the full skill experience — it wires the
project `.mcp.json` and installs the skill:

```bash
curl -fsSL https://maadgrom.github.io/semanticastindexer/install.sh | bash -s -- --platform claude-code --write
```

Server snippet for `.mcp.json`:

```json
{
  "mcpServers": {
    "code-search": {
      "command": "/path/to/semanticastindexer/target/release/semanticastindexer",
      "args": ["mcp", "--backend", "duckdb", "--embedder", "ort", "--collection", "source_code"]
    }
  }
}
```

**Reload:** Claude Code loads `.mcp.json` from the project root on startup — restart the
session (or reopen the project) to pick up changes.

**Verify:** the `sai_` tools appear in the tool list; the skill prompt also references
`sai_search_code`, `sai_find_similar`, and `sai_find_duplicates`.

**First test query:** ask Claude to "search the codebase for where authentication tokens
are validated" — it should call `sai_search_code`.

---

## Claude Desktop

**Config:** `claude_desktop_config.json`, per OS:

| OS | Path |
| -- | ---- |
| macOS | `~/Library/Application Support/Claude/claude_desktop_config.json` |
| Linux | `~/.config/Claude/claude_desktop_config.json` |

Install command:

```bash
curl -fsSL https://maadgrom.github.io/semanticastindexer/install.sh | bash -s -- --platform claude-desktop --write
```

Server snippet:

```json
{
  "mcpServers": {
    "code-search": {
      "command": "/path/to/semanticastindexer/target/release/semanticastindexer",
      "args": ["mcp", "--backend", "duckdb", "--embedder", "ort", "--collection", "source_code"],
      "cwd": "/absolute/path/to/your/indexed/project"
    }
  }
}
```

Claude Desktop has no per-project working directory, so set `cwd` explicitly to the
indexed project root.

**Restart:** fully quit and relaunch the Claude Desktop app.

**Verify:** open the tools / MCP servers indicator and confirm the server is connected and
the `sai_` tools are listed.

**First test query:** "find functions similar to this one" with a pasted snippet — it
should call `sai_find_similar`.

---

## Cursor

**Config:** `~/.cursor/mcp.json` (global), or project `.cursor/mcp.json`.

Install command:

```bash
curl -fsSL https://maadgrom.github.io/semanticastindexer/install.sh | bash -s -- --platform cursor --write
```

Server snippet:

```json
{
  "mcpServers": {
    "code-search": {
      "command": "/path/to/semanticastindexer/target/release/semanticastindexer",
      "args": ["mcp", "--backend", "duckdb", "--embedder", "ort", "--collection", "source_code"],
      "cwd": "/absolute/path/to/your/indexed/project"
    }
  }
}
```

**Reload:** open Cursor Settings → MCP and toggle the server, or restart Cursor.

**Verify:** the MCP settings panel shows the server connected with its tools enumerated.

**First test query:** "look for near-duplicate code across the repo" — it should call
`sai_find_duplicates`.

---

## Windsurf / Cascade

**Config:** `~/.codeium/windsurf/mcp_config.json`.

Install command:

```bash
curl -fsSL https://maadgrom.github.io/semanticastindexer/install.sh | bash -s -- --platform windsurf --write
```

Server snippet:

```json
{
  "mcpServers": {
    "code-search": {
      "command": "/path/to/semanticastindexer/target/release/semanticastindexer",
      "args": ["mcp", "--backend", "duckdb", "--embedder", "ort", "--collection", "source_code"],
      "cwd": "/absolute/path/to/your/indexed/project"
    }
  }
}
```

**Reload:** in Cascade, open the MCP / plugins panel and press refresh, or restart
Windsurf.

**Verify:** the Cascade MCP panel lists the server and its `sai_` tools.

**First test query:** "what does the indexing pipeline do?" — it should call
`sai_search_code`.

---

## Continue.dev

**Config:** `~/.continue/config.yaml` — add an `mcpServers` block (YAML).

Install command:

```bash
curl -fsSL https://maadgrom.github.io/semanticastindexer/install.sh | bash -s -- --platform continue --write
```

Server snippet (YAML):

```yaml
mcpServers:
  - name: code-search
    command: /path/to/semanticastindexer/target/release/semanticastindexer
    args:
      - mcp
      - --backend
      - duckdb
      - --embedder
      - ort
      - --collection
      - source_code
    cwd: /absolute/path/to/your/indexed/project
```

**Reload:** reload the Continue extension (or restart the IDE) so it re-reads
`config.yaml`.

**Verify:** the Continue assistant's tool list includes the `sai_` tools.

**First test query:** "find where config defaults are resolved" — it should call
`sai_search_code`.

---

## Codex CLI

**Config:** `~/.codex/config.toml`, under an `[mcp_servers.semantic-code-search]` table
(TOML).

Install command:

```bash
curl -fsSL https://maadgrom.github.io/semanticastindexer/install.sh | bash -s -- --platform codex --write
```

Server snippet (TOML):

```toml
[mcp_servers.semantic-code-search]
command = "/path/to/semanticastindexer/target/release/semanticastindexer"
args = ["mcp", "--backend", "duckdb", "--embedder", "ort", "--collection", "source_code"]
cwd = "/absolute/path/to/your/indexed/project"
```

**Reload:** start a new Codex CLI session so it re-reads `config.toml`.

**Verify:** Codex enumerates the MCP server's tools at session start; the `sai_` tools
should be present.

**First test query:** "search for the embedding model loader" — it should call
`sai_search_code`.

---

## Generic / manual client

Any stdio MCP client works. Use `--platform generic` to print a portable `.mcp.json`
block and its target path:

```bash
curl -fsSL https://maadgrom.github.io/semanticastindexer/install.sh | bash -s -- --platform generic
```

Then paste the printed block into your client's MCP config. The shape is the same
`mcpServers` object used above:

```json
{
  "mcpServers": {
    "code-search": {
      "command": "/path/to/semanticastindexer/target/release/semanticastindexer",
      "args": ["mcp", "--backend", "duckdb", "--embedder", "ort", "--collection", "source_code"],
      "cwd": "/absolute/path/to/your/indexed/project"
    }
  }
}
```

The rules are the same for any client:

- `command` points at the binary (absolute path is safest).
- `args` starts with `mcp` and selects backend, embedder, and collection.
- `cwd` is the indexed project root.

Restart the client, confirm the `sai_` tools load, and run a `sai_search_code` query as a
smoke test.

## Enabling the write tool

All snippets above start the server read-only. `sai_refresh` (re-index specific files) is
a **write tool** and is only usable when the server is started with `--allow-write` —
without it, the index is opened read-only and `sai_refresh` returns
`server is read-only; restart with --allow-write`. To enable it, add the flag to `args`:

```json
"args": ["mcp", "--backend", "duckdb", "--embedder", "ort", "--collection", "source_code", "--allow-write"]
```

See [../reference/mcp-server.md](../reference/mcp-server.md) for what `sai_refresh` does
and the similarity thresholds that govern `sai_find_similar` / `sai_find_duplicates`.

## Verifying and troubleshooting

If a client starts but the `sai_` tools don't appear, or queries error out:

- Confirm the `command` path is correct and the binary was built with `--features all`.
- Confirm `cwd` is the project root you actually indexed (so the DuckDB index and
  `indexer.yaml` are found).
- Make sure you indexed the project once before starting the server.
- Fully restart the client after editing its config.

See [../operations/troubleshooting.md](../operations/troubleshooting.md) for more.
