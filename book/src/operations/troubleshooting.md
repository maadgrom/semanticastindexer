# Troubleshooting and FAQ

This is the FAQ hub for `semanticastindexer` (SAI). Each entry pairs a symptom you'll
see (an error message, an empty result, a missing tool) with the cause and the
fix. Most problems fall into one of three buckets: a first-run model download, a
dimension/feature mismatch, or an MCP wiring issue.

For deeper background see [Backends and embedders](../reference/backends-and-embedders.md),
[Environment variables](../reference/configuration.md#environment-variables), and [MCP clients](../integrations/mcp-clients.md).

## Q: The first run hangs for a long time, or fails with no network. What is it doing?

On the local DuckDB path with the `ort` (ONNX Runtime) embedder, the very first run
downloads the model and tokenizer from Hugging Face: `onnx/model.onnx` and
`tokenizer.json` from the repo named by `duckdb.model_repo`. That download happens once;
later runs reuse the cache.

- **Where the cache lives:** the standard Hugging Face Hub cache at
  `~/.cache/huggingface`. Each repo lands under
  `~/.cache/huggingface/hub/models--<owner>--<name>/`.
- **Slow download:** the ONNX model is the big file (hundreds of MB for code models).
  Progress is printed; let it finish — it is cached afterward.
- **No / restricted network:** if you already have a populated cache, point the embedder
  at it instead of the default location and run offline.

```yaml
# sai-cfg.yml — reuse a pre-populated HF cache (no network on subsequent runs)
duckdb:
  model_cache: /path/to/huggingface/cache
```

`duckdb.model_cache` sets the Hugging Face cache directory the `ort` embedder reads from
(it maps to the Hub client's cache dir). To force fully offline behavior regardless of the
cache location, export `HF_HUB_OFFLINE`:

```bash
export HF_HUB_OFFLINE=1
```

With `HF_HUB_OFFLINE=1`, the Hub client never reaches the network and uses only what's
already cached — so the model and tokenizer must already be present, or the run errors.
If you pre-stage the cache once and then run normally, you do **not** need
`HF_HUB_OFFLINE` at all; the cache hit is automatic.

If the model download itself fails you'll see:

```text
failed to download onnx/model.onnx from <repo> (check network or set duckdb.model_cache to a pre-populated dir)
```

## Q: The first run fails on `tokenizer.json` with "relative URL without a base". What's wrong?

You are running a build older than the `hf-hub` 0.5 upgrade. The old pinned `hf-hub` 0.3
client could not fetch files that Hugging Face stores in **Xet** storage — for the
recommended code model `jinaai/jina-embeddings-v2-base-code`, `onnx/model.onnx` downloaded
fine but `tokenizer.json` errored with `relative URL without a base`.

**Fix:** upgrade to a current build; the first-run download of both files now works.
If you must stay on an old build, stage `tokenizer.json` into the model's HF cache
snapshot once with `curl`, then run normally:

```bash
# Old builds only: stage tokenizer.json into the cached snapshot for the jina code model
SNAP=~/.cache/huggingface/hub/models--jinaai--jina-embeddings-v2-base-code/snapshots/*/
curl -sL https://huggingface.co/jinaai/jina-embeddings-v2-base-code/resolve/main/tokenizer.json -o $SNAP/tokenizer.json
```

See [Choosing a model](../guides/choosing-a-model.md) and
[Backends and embedders](../reference/backends-and-embedders.md) for the full model
recommendation.

## Q: I get "embedder produced N-d vectors but vector_dim=M". How do I fix it?

The DuckDB table's `embedding` column is declared `FLOAT[vector_dim]`, so the embedder's
output dimension MUST equal the configured `vector_dim`. A mismatch means the model and
`vector_dim` disagree, or you switched models without rebuilding the index. The runtime
guard prints the right dimensions for the common models:

```text
embedder produced 768-d vectors but vector_dim=384 — set vector_dim to match the model (e5-small=384, nomic-embed-text=768, mxbai-embed-large=1024)
```

When you open an **existing** index whose stored column type no longer matches, you get
the column-level variant instead:

```text
DuckDB table '<collection>' has embedding column of type FLOAT[384] but config/vector_dim=768 (expected FLOAT[768]). This usually means the embedding model was changed without --recreate. Delete the DuckDB file or re-index with --recreate.
```

**Fixes (any one):**

1. Set `vector_dim` to match your model — `e5-small` = 384, `nomic-embed-text` = 768,
   `mxbai-embed-large` = 1024, `jinaai/jina-embeddings-v2-base-code` = 768.
2. Re-index with `--recreate` (drops and recreates the collection):

   ```bash
   ./target/release/semanticastindexer --backend duckdb --recreate --root src --ext rs
   ```

3. Delete the DuckDB file (and its `.wal` sidecar) and re-index:

   ```bash
   rm -f .index/code.duckdb .index/code.duckdb.wal
   ```

**The interactive delete prompt.** On a normal `index` run (not `--query-only`), if SAI
opens an index that has a dimension mismatch, it offers to delete and rebuild for you:

```text
The index at '<path>' was built with a different embedding model (dimension mismatch). Delete it and re-index from scratch? [y/N]
```

This prompt **defaults to No** and only deletes on an explicit `y`/`yes`. When stdin is
**not** an interactive terminal (CI, git hooks, the MCP stdio server), it auto-declines
and the original error propagates — automation never blocks on input and never destroys an
index by default. A `--query-only` run never re-indexes, so it just surfaces the error
rather than offering to delete (deleting would only leave an empty DB to query).

## Q: I get "Document API only works on Qdrant Cloud". Why?

The `qdrant` backend uses Qdrant's server-side inference (the `Document` API) — there is
no local model on this path; the cluster embeds your text. Plain OSS/local Qdrant has
**no** inference engine, so the `Document` API only works against Qdrant Cloud (or another
inference-enabled deployment) with **Inference enabled** and the embedding model available
on the cluster.

**Fixes:**

- Use a Qdrant Cloud cluster with Inference enabled and the embedding model present
  (`intfloat/multilingual-e5-small`, vector size 384, context window 512). Provide the
  API key in the environment (the URL can be `qdrant.url` in YAML or `QDRANT_URL`):

  ```bash
  export QDRANT_URL="https://<cluster-id>.<region>.aws.cloud.qdrant.io:6334"   # gRPC :6334
  export QDRANT_API_KEY="<key from the cluster's API Keys tab>"
  ```

- Or switch to the fully local DuckDB backend, which embeds on-device and needs no
  cluster:

  ```bash
  ./target/release/semanticastindexer --backend duckdb --root src --ext rs
  ```

See [Qdrant Cloud setup](../integrations/qdrant-cloud.md) and the
[Environment](../reference/configuration.md#environment-variables) reference.

## Q: I get a "rebuild with --features ..." error. What does that mean?

Backends and embedders are compiled behind Cargo feature flags. Selecting a backend or
embedder whose feature was not built into the binary fails with a clear "rebuild with
--features …" message. Likewise, if no embedder feature is compiled in, the embedder
errors with:

```text
no embedder compiled in (build with --features ort or --features ollama)
```

**Fix:** rebuild with the feature you need. The simplest catch-all enables everything:

```bash
cargo build --release --features all
```

Or enable just what you use, for example the local DuckDB + ONNX + AST path:

```bash
cargo build --release --features "duckdb,ort,ast"
```

The `ast` chunker is also feature-gated — selecting `chunker: ast` (or auto-selecting it)
without the `ast` feature errors early. See
[Backends and embedders](../reference/backends-and-embedders.md) for the feature matrix.

## Q: I get zero results, or `duplicates`/`similar` misses obvious matches. Why?

A few independent causes:

- **The index is empty.** Run an index first. With the DuckDB MCP server, a missing index
  is an explicit error: `DuckDB index not found at <path> — run an index first (the MCP
  server is read-only)`.
- **Recall degraded after deletes (DuckDB only).** DuckDB's experimental HNSW index loses
  recall after in-place deletes. SAI's `sync` therefore wraps its changed-file loop in a
  bulk window (`begin_bulk` / `end_bulk`) that **drops and recreates** the HNSW graph,
  restoring full recall — effectively a full rebuild of the graph. If rows were deleted by
  some other means, run a `sync` (or re-index) so the graph is rebuilt:

  ```bash
  ./target/release/semanticastindexer sync --backend duckdb
  ```

- **Threshold too high for your model.** Code-trained models score lower than E5, so a
  `duplicate_min_score` tuned for E5 can hide real near-duplicates. Lower the threshold
  for the jina code model (e.g. `0.88`). See
  [Tuning similarity](../guides/tuning-similarity.md).
- **Wrong prefix style for the model.** Both embedders apply E5's `passage:`/`query:`
  prefixes by default; a symmetric or non-E5 model wants `prefix_style: none`, otherwise
  relevance suffers.

## Q: My MCP client doesn't show the `sai_*` tools. How do I wire it up?

A handful of wiring issues account for almost every "the server doesn't appear" report:

- **Use an absolute command path.** Point the client at the built binary by absolute path,
  e.g. `/abs/path/to/target/release/semanticastindexer`, not a bare name your client may
  not resolve.
- **Set the working directory to the indexed project.** The server resolves the index and
  config relative to its cwd, so the client must launch it with `cwd` = the project you
  indexed (where `.index/code.duckdb` / `sai-cfg.yml` live).
- **Restart the client.** Most MCP clients only read server config at startup; after
  editing the config, fully restart the client so it spawns the server again.
- **The server is read-only by default.** It runs with `--backend duckdb --embedder
  ollama` defaults and is read-only; the `sai_refresh` write tool is **not** registered
  unless you start it with `--allow-write`. Without that flag, `sai_refresh` returns a
  clear "restart with --allow-write" error. The `sai_prepare_mcp_setup` tool only executes
  the setup script when started with `--allow-setup`.
- **Index first.** A read-only server cannot index; if `.index/code.duckdb` does not exist
  yet, run an index before launching the server.

Minimal stdio launch (read-only) and the writable variant:

```bash
# read-only MCP server over stdio (defaults: duckdb + ollama)
/abs/path/to/target/release/semanticastindexer mcp

# writable: also registers the sai_refresh tool
/abs/path/to/target/release/semanticastindexer mcp --allow-write
```

The full list of `sai_*` tools and their argument schemas lives in the
[MCP server reference](../reference/mcp-server.md); per-client config snippets are in
[MCP clients](../integrations/mcp-clients.md).

## Q: The MCP server can't load the DuckDB VSS extension. What now?

The DuckDB backend needs the VSS extension (for HNSW search and `array_cosine_distance`).
SAI tries a plain `LOAD vss` first (works on read-only connections if VSS was previously
installed), then `INSTALL vss; LOAD vss`, then the community repo. If all fail you get an
actionable error. The common fix is to install VSS once with a writable run:

- Run the indexer at least once with write access so it can `INSTALL vss`, or
- Pre-install for a read-only MCP server: `duckdb -c "INSTALL vss;"` as a user who can
  write to DuckDB's extension directory, or
- In an air-gapped environment, copy the VSS extension into DuckDB's extension search path
  before starting the read-only server.

See [Performance](performance.md) and [Security](security.md) for related operational
notes, and [Keeping in sync](../guides/keeping-in-sync.md) for the `sync` workflow that
keeps recall healthy after edits.
