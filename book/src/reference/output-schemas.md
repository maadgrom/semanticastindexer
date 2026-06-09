# Output schemas

This page is the precise, parseable contract for everything SAI returns: the
**structured JSON** emitted by each MCP tool, and the **plain-text** lines printed
by the CLI subcommands. Field names, nesting, and defaults are taken directly from
the implementation (`src/mcp.rs`, `src/search.rs`, `src/main.rs`) — nothing here is
invented. For the tools and flags that produce these results, see the
[MCP server and tools reference](./mcp-server.md) and the [CLI reference](./cli.md); for the
meaning of terms like *chunk*, *symbol*, and *cosine similarity*, see the
[glossary](../concepts/glossary.md).

All MCP tools return their payload as a **structured** result (the object shown in
each section below is the structured content). Every numeric `score` / `sim` /
`min_sim` / `max_sim` is a cosine similarity in roughly `[-1, 1]`, with `1.0` being
identical direction.

## MCP tool: `sai_search_code`

Semantic search. The query is embedded and matched against indexed chunks; optional
`language` / `path_glob` post-filters are applied, then results are truncated to the
(clamped) `limit`.

```json
{
  "hits": [
    {
      "path": "src/auth/session.ts",
      "start_line": 42,
      "end_line": 88,
      "symbol": "createSession",
      "score": 0.8123,
      "snippet": "export function createSession(user: User): Session {\n  // ...\n}"
    }
  ]
}
```

| Field        | Type             | Notes |
|--------------|------------------|-------|
| `hits`       | array of objects | Ranked best-first. Empty array when nothing matches. |
| `path`       | string           | Repo-relative path of the chunk. |
| `start_line` | integer          | 1-based first line of the chunk. |
| `end_line`   | integer          | 1-based last line of the chunk. |
| `symbol`     | string \| null   | Enclosing symbol (e.g. function/class name). **`null` for line-chunked indexes** (the `lines` chunker emits no symbol). |
| `score`      | number           | Cosine similarity to the query. |
| `snippet`    | string           | First ~8 lines of the chunk, capped to ~800 chars (a trailing `…` is appended when cut). With `include_text: true` this is the **full** chunk text, uncapped. |

## MCP tool: `sai_find_similar`

Neighbours of a `code` snippet (embedded as a passage) **or** of an existing indexed
chunk located by `path` + `line` (its stored vector is reused and the chunk excludes
itself). Returns the **same hit shape** as `sai_search_code`.

```json
{
  "hits": [
    {
      "path": "src/auth/legacy_session.ts",
      "start_line": 10,
      "end_line": 55,
      "symbol": "makeSession",
      "score": 0.9412,
      "snippet": "function makeSession(u) {\n  // ...\n}"
    }
  ]
}
```

Differences from `sai_search_code`:

- `snippet` is **always** capped (~8 lines / ~800 chars); `sai_find_similar` has no
  `include_text` option.
- Results are filtered by `min_score` before being returned. When `min_score` is
  omitted, the configured `find_similar_min_score` default applies; pass an explicit
  `min_score: 0.0` to see the raw, unfiltered distribution.
- `symbol` is `null` for line-chunked indexes, same as above.

## MCP tool: `sai_find_duplicates`

Codebase-wide near-duplicate clusters. For each chunk, its top-`k` neighbours are
taken; edges with similarity `>= min_score` union chunks into clusters; clusters with
`>= min_cluster_size` members are returned largest-first (tie-break: higher `max_sim`),
truncated to `max_clusters`.

```json
{
  "clusters": [
    {
      "size": 3,
      "members": [
        { "path": "src/a.ts", "start_line": 1,  "end_line": 40, "symbol": "parse" },
        { "path": "src/b.ts", "start_line": 12, "end_line": 51, "symbol": "parseInput" },
        { "path": "src/c.ts", "start_line": 5,  "end_line": 44, "symbol": null }
      ],
      "min_sim": 0.9512,
      "max_sim": 0.9987
    }
  ]
}
```

| Field        | Type             | Notes |
|--------------|------------------|-------|
| `clusters`   | array of objects | Largest-first; tie-break by higher `max_sim`. Empty array when no cluster meets the thresholds. |
| `size`       | integer          | Number of members in the cluster (equals `members.length`). |
| `members`    | array of objects | Sorted deterministically by `path`, then `start_line`. |
| `members[].path`       | string         | Repo-relative path. |
| `members[].start_line` | integer        | 1-based first line. |
| `members[].end_line`   | integer        | 1-based last line. |
| `members[].symbol`     | string \| null | Enclosing symbol; **`null` for line-chunked indexes**. Members carry **no** `score` field — per-edge similarity is summarized at the cluster level only. |
| `min_sim`    | number           | Lowest kept-edge similarity within the cluster. |
| `max_sim`    | number           | Highest kept-edge similarity within the cluster. |

## MCP tool: `sai_index_status`

Index metadata for freshness / sanity checks. Takes no arguments.

```json
{
  "backend": "duckdb",
  "collection": "source_code",
  "model": "nomic-embed-text",
  "vector_dim": 768,
  "chunk_count": 1842,
  "chunker": "lines"
}
```

| Field         | Type    | Notes |
|---------------|---------|-------|
| `backend`     | string  | Active vector backend, e.g. `"duckdb"` or `"qdrant"`. |
| `collection`  | string  | Collection / table name the index lives in. |
| `model`       | string  | Embedding model label. For `backend=duckdb` + `embedder=ollama` this is the Ollama model name; otherwise the configured `model`. |
| `vector_dim`  | integer | Embedding dimensionality. |
| `chunk_count` | integer | Total number of indexed chunks. |
| `chunker`     | string  | Chunking strategy, e.g. `"lines"` or `"ast"`. |

## MCP tool: `sai_refresh`

Re-index specific files in place (write tool; requires the server to be started with
`--allow-write`). Each path's existing points are deleted first; paths that still exist
and pass the index filters are re-chunked, re-embedded, and upserted. Gone or excluded
paths are reported under `removed`.

```json
{
  "refreshed": [
    { "path": "src/a.ts", "chunks": 7 },
    { "path": "src/b.ts", "chunks": 3 }
  ],
  "removed": [
    "src/deleted.ts"
  ]
}
```

| Field                  | Type             | Notes |
|------------------------|------------------|-------|
| `refreshed`            | array of objects | Paths that were re-chunked and upserted. |
| `refreshed[].path`     | string           | Repo-relative path that was refreshed. |
| `refreshed[].chunks`   | integer          | Number of chunks produced for that path. |
| `removed`              | array of strings | Paths whose points were deleted with nothing re-indexed (file gone or now excluded). |

## MCP tool: `sai_prepare_mcp_setup`

Returns ready-to-run commands and an MCP server config block. With `execute: true`
**and** the server started with `--allow-setup`, it additionally attempts to run the
setup script and adds execution fields.

```json
{
  "target_directory": "/path/to/project",
  "recommended_command": "/path/to/mcp-setup/setup.sh --non-interactive --backend duckdb --embedder ollama",
  "mcp_server_config_example": {
    "mcpServers": {
      "semantic-code-search": {
        "command": "<path-to-semanticastindexer>",
        "args": ["mcp", "--backend", "duckdb", "--embedder", "ollama", "--collection", "source_code"],
        "cwd": "/path/to/project"
      }
    }
  },
  "next_steps": [
    "1. Run the recommended_command in a terminal (it can take 5-20 minutes the first time).",
    "2. After it finishes, index your project: cd <your-project> && <binary> --dry-run",
    "3. Then run without --dry-run to actually build the index.",
    "4. Add the mcp_server_config_example to your agent's MCP settings.",
    "5. Restart your agentic tool."
  ],
  "notes": "For fully offline use, prefer embedder=ort (much longer first build). The setup script lives next to the binary in mcp-setup/setup.sh."
}
```

| Field                       | Type   | Notes |
|-----------------------------|--------|-------|
| `target_directory`          | string | The directory being set up (defaults to the server's current working directory). |
| `recommended_command`       | string | Exact `setup.sh` invocation, reflecting the chosen `backend`, `embedder`, and the `--install-global` / AST flags. |
| `mcp_server_config_example` | object | Drop-in `mcpServers` block; `command` is a placeholder to replace with the real binary path. |
| `next_steps`                | array of strings | Ordered instructions. |
| `notes`                     | string | Offline / setup-script guidance. |

**Conditional execution fields** (added only when `execute: true` was requested):

| Field                     | Type    | When present |
|---------------------------|---------|--------------|
| `execution_attempted`     | boolean | The server had `--allow-setup` and tried to run the script. |
| `stdout` / `stderr`       | string  | Captured script output (on a successful spawn). |
| `success`                 | boolean | Exit status of the script. |
| `error`                   | string  | The script could not be spawned. |
| `execution_blocked`       | boolean | `execute: true` was requested but the server was **not** started with `--allow-setup`. |
| `execution_blocked_reason`| string  | `"Server not started with --allow-setup"`. |

## CLI text output

The CLI prints human-readable lines to **stdout** (informational/progress messages go
to stderr). Numeric similarity values are formatted to **4 decimal places**. Indented
result lines are prefixed with two spaces.

### `--query` (semantic search)

A header line followed by one `score  path:start-end` line per hit:

```text
top 8 for: hash a password

  0.8421  src/auth/hash.ts:12-37
  0.7993  src/auth/verify.ts:5-29
```

- Header: `top <N> for: <query>` (`N` = number of hits).
- Each hit row: two leading spaces, then `<score>  <path>:<start_line>-<end_line>`.
- The query path prints **no symbol** column.

### `duplicates`

A summary line, then one block per cluster:

```text
2 near-duplicate cluster(s) (min_score 0.93, min_cluster_size 2, top_k 8):
cluster (size 3, sim 0.9512..0.9987):
  src/a.ts:1-40  parse
  src/b.ts:12-51  parseInput
  src/c.ts:5-44  
cluster (size 2, sim 0.9301..0.9442):
  src/x.ts:3-20  
  src/y.ts:7-24  
```

- Summary: `<N> near-duplicate cluster(s) (min_score <m>, min_cluster_size <c>, top_k <k>):`.
- Cluster header: `cluster (size <S>, sim <min_sim>..<max_sim>):`.
- Member rows: two leading spaces, then `<path>:<start_line>-<end_line>  <symbol>`.
- When nothing qualifies, a single line is printed instead:
  `no near-duplicate clusters (min_score <m>, min_cluster_size <c>, top_k <k>)`.

### `similar`

A summary line, then one row per neighbour:

```text
3 similar (min_score 0.85):
  0.9412  src/auth/legacy_session.ts:10-55  makeSession
  0.8810  src/auth/session.ts:42-88  createSession
  0.8602  src/auth/token.ts:1-30  
```

- Summary: `<N> similar (min_score <m>):`.
- Each row: two leading spaces, then `<score>  <path>:<start_line>-<end_line>  <symbol>`.

### Note on the `symbol` column

In the `duplicates` and `similar` text output, `symbol` is the **last** field on each
member/hit row. For **line-chunked** indexes (the `lines` chunker) there is no symbol,
so that field renders as an **empty string** — the row ends right after the line range
plus the two trailing spaces (shown above for `src/c.ts`, `src/x.ts`, `src/y.ts`, and
`src/auth/token.ts`). This is the text-output counterpart of `symbol: null` in the JSON
schemas.
