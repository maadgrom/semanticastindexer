# MCP server (`semanticastindexer mcp`)

`semanticastindexer mcp` runs an MCP server over stdio (semantic code search for agentic
coding tools), built on the official Rust MCP SDK (`rmcp`; feature `mcp`). Defaults to
`--backend duckdb --embedder ollama`. **Read-only by default.**

For a one-command per-platform install (Claude Code, Cursor, Windsurf, Codex, …) see the
[install page](https://maadgrom.github.io/semanticastindexer/) and [installation guide](install.md).

## Tools

All tools are prefixed `sai_` so they stand apart from other MCP servers' tools in the
agent's tool list.

- `sai_search_code` — general semantic search (query embedded as a query).
- `sai_find_similar` — neighbours of one function (inline `code`, or a stored `path`+`line`).
- `sai_find_duplicates` — codebase-wide near-duplicate clusters (NN edges + union-find).
- `sai_index_status` — backend / collection / model / vector_dim / chunk_count / chunker.
- `sai_refresh` — re-index specific files (delete + re-embed). **Write tool**: usable only
  when the server is started with `--allow-write`. Without that flag the index is opened
  read-only and `sai_refresh` returns `server is read-only; restart with --allow-write`.

With `--allow-write` the DuckDB file is opened writable (normal `connect`, HNSW
persistence intact); `sai_refresh { paths: [...] }` deletes each path's points, then
re-chunks / embeds / upserts the files that still exist and pass the index filters (ext,
globs, not generated) — reusing the SAME per-file logic as `sync` — wrapped in one
begin/end bulk window (HNSW drop → upserts → rebuild). Returns
`{ refreshed: [{path, chunks}], removed: [path] }`. Paths are clamped (max 200/call).

## Similarity thresholds (`similarity:`)

`sai_find_similar` / `sai_find_duplicates` cutoffs live in the `similarity:` block of
`indexer.yaml`. **Resolution per knob: MCP tool arg > config value > built-in default**
(`find_similar_min_score` 0.85, `duplicate_min_score` 0.93, `duplicate_min_cluster_size`
2, `top_k` 10). These cutoffs are **model-specific — tune per model (Qwen ≠ e5)**: a
Qwen3 cosine of 0.85 is a looser match than e5's 0.85, so the right "duplicate" cut
differs. Call `sai_find_similar` with an explicit low `min_score` (e.g. 0.0) to eyeball the
raw score distribution, then set the config thresholds for your model.

## Wiring (`.mcp.json` in the project you want to search)

Point `command` at the built binary (an absolute path is safest) and set `cwd` to the
indexed project root so it finds that project's DuckDB index and `indexer.yaml`:

```json
{ "mcpServers": { "code-search": {
  "command": "/path/to/semanticastindexer/target/release/semanticastindexer",
  "args": ["mcp","--backend","duckdb","--embedder","ort","--collection","source_code"]
}}}
```

Build with `--features all` and index the project once before starting the server.
The default MCP backend is `duckdb` + `ollama`; pass `--embedder ort` to use the
fully offline ONNX embedder.

## One-command setup script

For the easiest experience (especially when using this with agents), use the dedicated
setup script:

```bash
./mcp-setup/setup.sh
```

The script will:
- Build the binary with good defaults for agentic use
- Create a tuned `indexer.yaml`
- Generate ready-to-use MCP config snippets for Claude, Cursor, Windsurf, etc.
- Support fully non-interactive mode (`--non-interactive`) so agents can drive setup

See [mcp-setup/README.md](../mcp-setup/README.md) and [mcp-setup/SKILL.md](../mcp-setup/SKILL.md)
for details, or the hosted [install page](https://maadgrom.github.io/semanticastindexer/) for
per-platform one-liners.
