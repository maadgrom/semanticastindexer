# MCP server and tools

`semanticastindexer mcp` runs an [MCP](../concepts/glossary.md) server over **stdio**,
exposing SAI's semantic code search to agentic coding tools (Claude Code, Cursor, Windsurf,
Codex, and others). It is built on the official Rust MCP SDK (`rmcp`, Cargo feature `mcp`).

This page is the authoritative reference for the server and every tool it exposes. The
`mcp-setup/SKILL.md` agent skill defers to this page for the canonical tool contracts.

For per-client wiring (Claude Code, Cursor, Windsurf, Codex, …) see
[MCP clients](../integrations/mcp-clients.md). For the exact JSON shapes each tool returns,
see [Output schemas](../reference/output-schemas.md).

## Server behavior

- **Read-only by default.** The server exposes no index/upsert/flush tools. Only
  `sai_refresh` writes, and only when explicitly enabled (see below).
- **Defaults to `--backend duckdb --embedder ollama`.** Pass `--embedder ort` to use the
  fully offline ONNX embedder. See [Backends and embedders](../reference/backends-and-embedders.md).
- **Backend built once at startup.** The backend and embedder are constructed a single time
  and reused across every tool call.
- **Tools are `sai_`-prefixed** so they stand apart from other MCP servers' tools in the
  agent's tool list.

### Worker-thread model

The DuckDB backend is `!Send`/`!Sync`, but rmcp's tool-handler futures must be `Send`. So
the backend lives on a **dedicated worker thread**, and the server holds only a `Send + Sync`
channel handle (`BackendHandle`). Each tool handler builds a request, sends it to the worker,
and awaits a `oneshot` reply — keeping handler futures `Send` while serializing all backend
access through the single-threaded connection. For the full rationale see
[Internals: logical audit](../project/logical-audit.md).

### Threshold and limit resolution

Two resolution rules apply throughout the tools:

- **Threshold resolution (per knob):** `MCP tool arg > config value > built-in default`. The
  config values come from the `similarity:` block of `indexer.yaml`; the built-in defaults are
  `find_similar_min_score = 0.85`, `duplicate_min_score = 0.93`, `duplicate_min_cluster_size = 2`,
  and `top_k = 10`. These cutoffs are **model-specific** — tune them per embedding model. See
  [Tuning similarity](../guides/tuning-similarity.md).
- **Limit clamping:** any caller-supplied `limit` / `top_k` is clamped to **`[1, 50]`** so a
  single call can't request the world.

## Tools

The server exposes six tools. Four are read-only; `sai_refresh` writes and `sai_prepare_mcp_setup`
can execute a setup script — both are gated behind explicit flags.

| Tool | Purpose | Gating |
|------|---------|--------|
| `sai_search_code` | General semantic search (query embedded as a query) | none (read-only) |
| `sai_find_similar` | Neighbours of one snippet or one stored chunk | none (read-only) |
| `sai_find_duplicates` | Codebase-wide near-duplicate clusters | none (read-only) |
| `sai_index_status` | Index metadata (backend, model, dim, count, …) | none (read-only) |
| `sai_prepare_mcp_setup` | Return setup commands; optionally run the setup script | execution requires `--allow-setup` |
| `sai_refresh` | Re-index specific files in place (delete + re-embed) | requires `--allow-write` |

### `sai_search_code`

General semantic search over the indexed code. The query is embedded **as a query** and the
nearest indexed chunks are returned. The server over-fetches (about `limit × 4`, still clamped)
so the `language` / `path_glob` post-filters can still return up to `limit` rows. When
`include_text` is false, each snippet is capped to the first ~8 lines and ~800 chars.

| Arg | Type | Required | Default |
|-----|------|----------|---------|
| `query` | string | **required** | — |
| `limit` | integer | optional | `8` (clamped to `[1, 50]`) |
| `language` | string | optional | unset (no language filter; e.g. `"ts"`) |
| `path_glob` | string | optional | unset (e.g. `"src/**"`) |
| `include_text` | boolean | optional | `false` (return capped snippet) |

### `sai_find_similar`

Find code similar to either an inline `code` snippet **or** an existing indexed chunk addressed
by `path` + `line`. Provide **either `code` OR both `path` and `line`** — not a mix:

- `code` is embedded as a **passage** (code-vs-code space) and requires a local embedder
  (the `duckdb` backend); calling it against a non-local-embedding backend returns an
  `invalid_params` error.
- `path` + `line` looks up the **exact stored vector** for that chunk (no re-embed) and
  excludes the chunk itself from its own results. If no indexed chunk exists at that
  location the call returns `no indexed chunk at <path>:<line>`.

`min_score` resolves as `arg > config > built-in default 0.85`. Omitting it still applies the
configured (model-tuned) cut; pass an explicit `0.0` to see the raw score distribution.

| Arg | Type | Required | Default |
|-----|------|----------|---------|
| `code` | string | one of `code` **or** `path`+`line` | unset |
| `path` | string | use together with `line` | unset |
| `line` | integer | 1-based start line; use with `path` | unset |
| `limit` | integer | optional | `8` (clamped to `[1, 50]`) |
| `min_score` | number | optional | config `find_similar_min_score`, else `0.85` |

### `sai_find_duplicates`

Find near-duplicate clusters across the index. For each chunk it takes the chunk's `top_k`
nearest neighbours, keeps the edges whose similarity is `>= min_score`, and unions them into
clusters via union-find. Clusters with size `>= min_cluster_size` are returned, largest first.

| Arg | Type | Required | Default |
|-----|------|----------|---------|
| `min_score` | number | optional | config `duplicate_min_score`, else `0.93` |
| `min_cluster_size` | integer | optional | config `duplicate_min_cluster_size` (else `2`), then floored at `max(…, 1)` |
| `path_glob` | string | optional | unset (restrict the scan to matching paths) |
| `max_clusters` | integer | optional | `50` (local constant; not configurable via `similarity:`) |
| `top_k` | integer | optional | config `top_k` (else `10`), clamped to `[1, 50]` |

Note: `min_score`, `min_cluster_size`, and `top_k` follow `arg > config > built-in default`,
while `max_clusters` is a fixed local default of `50` and has no config knob.

### `sai_index_status`

Report index metadata for freshness and sanity checks. Takes **no arguments**. Returns the
backend, collection, embedding model, vector dimension, total chunk count, and chunker.

| Arg | Type | Required | Default |
|-----|------|----------|---------|
| _(none)_ | — | — | — |

### `sai_prepare_mcp_setup`

Help an agent set up SAI as an MCP server for a project. By default it only **returns** the
exact commands and an MCP config snippet to run; it executes the setup script **only** when
`execute: true` **and** the server was started with `--allow-setup`. Without `--allow-setup`,
an `execute: true` call is reported as blocked (`Server not started with --allow-setup`) and no
script runs.

| Arg | Type | Required | Default |
|-----|------|----------|---------|
| `target_directory` | string | optional | current working directory |
| `backend` | string | optional | `"duckdb"` (or `"qdrant"`) |
| `embedder` | string | optional | `"ollama"` (or `"ort"` for fully offline) |
| `use_ast_chunker` | boolean | optional | `false` (requires a binary built with `--features ast`) |
| `install_globally` | boolean | optional | `false` (installs into `~/.local/bin` as a `code-search-mcp` wrapper) |
| `execute` | boolean | optional | `false` (only runs the script when also started with `--allow-setup`) |

The response includes the recommended setup command, an `mcp_server_config_example`, and a
numbered `next_steps` list. The first build can take several minutes (much longer for `ort`).

### `sai_refresh`

**Write tool.** Re-index specific files in place: for each path it deletes that path's existing
points, then re-chunks, re-embeds, and re-upserts the files that still exist and pass the index
filters (extension, globs, not generated). Paths that are gone or excluded are removed.
The whole batch runs in one bulk window (HNSW drop → per-path delete + re-embed + upsert →
rebuild), reusing the same per-file logic as the `sync` command.

| Arg | Type | Required | Default |
|-----|------|----------|---------|
| `paths` | array of string | **required** | — (non-empty; max **200** per call) |

Gating: the server must be started with `--allow-write`. Without it the backend is opened
read-only and any call returns:

```text
server is read-only; restart with --allow-write to enable refresh
```

An empty `paths` array returns `refresh requires at least one path`; more than 200 paths
returns `too many paths (max 200)`. On success the tool returns the refreshed paths (with
chunk counts) and the removed paths.

## Wiring (`.mcp.json`)

Point `command` at the built binary (an absolute path is safest) and set `cwd` to the indexed
project root so the server finds that project's index and `indexer.yaml`:

```json
{
  "mcpServers": {
    "code-search": {
      "command": "/path/to/semanticastindexer/target/release/semanticastindexer",
      "args": ["mcp", "--backend", "duckdb", "--embedder", "ollama", "--collection", "source_code"],
      "cwd": "/path/to/your/project"
    }
  }
}
```

Build with the needed features and index the project once before starting the server. To enable
the write tool, add `--allow-write` to `args`; to allow `sai_prepare_mcp_setup` to execute its
script, add `--allow-setup`:

```json
{
  "mcpServers": {
    "code-search": {
      "command": "/path/to/semanticastindexer/target/release/semanticastindexer",
      "args": ["mcp", "--backend", "duckdb", "--embedder", "ollama", "--allow-write"],
      "cwd": "/path/to/your/project"
    }
  }
}
```

See [MCP clients](../integrations/mcp-clients.md) for per-client config locations and the
[CLI reference](../reference/cli.md) for the full `mcp` flag list. For the response shape of
each tool, see [Output schemas](../reference/output-schemas.md).
