# Security and privacy

This page consolidates everything about credentials, what data leaves your machine, and the
capabilities the MCP server can expose. Two things drive the security model: **where
credentials come from** and **which backend you choose**.

## The API key is a secret; the URL is not

The only secret SAI uses is the Qdrant **API key**, and it is read **only** from the
environment — never from `sai-cfg.yml` or any other config file. The cluster **URL** is
not a secret and may live in YAML (`qdrant.url`) or in the environment.

| Value | Where it comes from |
| ----- | ------------------- |
| `QDRANT_API_KEY` | **Environment only** (secret). There is no YAML key for it by design. |
| Qdrant URL | `qdrant.url` in `sai-cfg.yml`, or the `QDRANT_URL` env var (which overrides YAML). |

Rules:

- **Never commit the API key.** Keep it out of YAML, `.mcp.json`, and shell history that
  lands in version control. The URL is safe to commit.
- **Rotate any exposed key.** If the API key is ever leaked, rotate it in the cluster's
  *API Keys* tab and update `QDRANT_API_KEY`.
- These only matter for the **Qdrant** backend. The local DuckDB backend needs no
  credentials at all.

```bash
# The key always comes from the environment; the URL can come from YAML or here.
export QDRANT_API_KEY="<your-key>"
export QDRANT_URL="https://<your-cluster>.qdrant.io:6334"   # or set qdrant.url in sai-cfg.yml
semanticastindexer --backend qdrant --root src --ext ts,tsx
```

See [environment variables](../reference/configuration.md#environment-variables) for the full list and
[Qdrant Cloud](../integrations/qdrant-cloud.md) for cluster setup.

## What leaves the machine

The biggest privacy lever is the **backend**. With the local backend, your source code never
leaves your machine; with Qdrant Cloud, your code text is sent to the cluster for server-side
inference and storage.

| Backend / embedder | What leaves the machine |
| ------------------ | ----------------------- |
| **`duckdb` + `ort`** | Nothing leaves the machine, except a **one-time model download** from Hugging Face on first run (the ONNX embedding model). After that, fully offline — no API keys, no servers. |
| **`duckdb` + `ollama`** | Your code chunks are sent over HTTP to the **Ollama server you point at**. A local server (`ollama serve` on your box) means nothing leaves the machine; a remote server means chunks go to that host. Plus the model pull on whichever host runs Ollama. |
| **`qdrant`** | Your code **text** is sent to **Qdrant Cloud**, which performs the embedding (server-side inference) and stores both the text and the vectors. |

Choose `duckdb` + `ort` for a fully offline, nothing-leaves-the-machine setup; use
`duckdb` + `ollama` against your own local server for the same privacy with a separate
embedding process; and treat `qdrant` as a deliberate decision to send code to a third-party
cloud service.

## MCP server capabilities

The MCP server (`semanticastindexer mcp`) is **read-only by default**. Read-only tools embed
queries and run nearest-neighbour searches against the existing index — they never mutate it.
Two flags unlock additional capabilities; both are **off by default**.

### Read-only by default

With no extra flags, the server exposes only read tools:

- `sai_search_code` — semantic search over the index.
- `sai_find_similar` — neighbours of a snippet or an existing chunk.
- `sai_find_duplicates` — codebase-wide near-duplicate clusters.
- `sai_index_status` — backend / collection / model / dimension / chunk count / chunker.

The setup-helper tool `sai_prepare_mcp_setup` is also present but, without `--allow-setup`,
only **returns** the commands and config you should run — it does not execute anything.

### `--allow-write` — enables the write tool

`--allow-write` enables `sai_refresh`, which **mutates the index**: for each supplied path it
deletes existing points, then re-chunks, re-embeds, and re-upserts the files that still exist
and pass the index filters (a single call is capped at 200 paths). Without this flag,
`sai_refresh` returns a clear error and the backend is opened read-only.

### `--allow-setup` — lets the server execute the setup script

`--allow-setup` is a **meaningful capability**: it lets `sai_prepare_mcp_setup` actually
**execute the setup script** when a caller passes `execute: true`. The setup script can build
the binary and modify files (it runs `bash -c` with the resolved setup command in the target
directory). Without `--allow-setup`, an `execute: true` request is blocked and the tool only
reports the commands.

Treat `--allow-setup` as you would any tool that can run builds and change files on disk:
leave it off unless you specifically want an agent to drive setup.

```bash
# Default: read-only, no execution.
semanticastindexer mcp --backend duckdb --embedder ort

# Allow the agent to re-index files in place:
semanticastindexer mcp --backend duckdb --embedder ort --allow-write

# Additionally allow the agent to execute the setup script (build + file changes):
semanticastindexer mcp --backend duckdb --embedder ort --allow-write --allow-setup
```

The full tool list, schemas, and threshold defaults are documented in the
[MCP server reference](../reference/mcp-server.md).

## Safety note: the `sai-noindexing` marker

The literal string `sai-noindexing` is an opt-out marker: a file (or chunk) containing it is
dropped from the index. Because this is a plain substring match, the marker can also match
inside a **string literal** in your source — for example a constant whose value is
`"sai-noindexing"`. When that happens, the surrounding code is **silently dropped from the
index** even though you did not intend to exclude it.

If a file you expect to find never shows up in search results, check whether the
`sai-noindexing` string appears anywhere in it, including inside string literals.

## `.gitignore`

Add `target/` to your `.gitignore` — it is a build artifact and should never be committed.

```gitignore
target/
```
