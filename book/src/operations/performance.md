# Performance and scaling

This page explains where SAI spends time and memory, and how to keep an index fast on large repositories. It is honest about what is *measured* and what is not: **no benchmarks ship with SAI**. The numbers below are architectural constants read from the source (batch sizes, thread counts, token windows), not throughput figures. Treat the guidance as "where the costs are," then measure on your own repo.

## Where the time goes

For the local `ort` (ONNX) embedder, indexing is dominated by two costs:

1. **CPU embedding** — running the ONNX model forward over every chunk.
2. **HNSW index rebuild** — dropping and recreating the DuckDB vector index around each bulk write.

For the Qdrant backend, embedding happens server-side (Inference), so client-side cost is mostly network round-trips for upsert and search.

## ort embedding: CPU inference in batches of 32

The `ort` embedder runs synchronous CPU inference. Two constants set its shape:

- **Batch size 32.** Passages are embedded `32` per ONNX forward pass (`EMBED_BATCH`).
- **All cores.** The ONNX intra-op thread pool is sized to the machine: `intra_threads = std::thread::available_parallelism()` (falling back to `1` if the count is unavailable). Indexing is a throughput-bound, one-shot batch job, so every core works the forward pass.

To reduce wasted compute on padding, each batch is **length-sorted** before tokenization (so a single long passage does not inflate its whole batch under `BatchLongest` padding), then results are scattered back to the caller's order. Tokens are truncated/padded to the **512-token** E5 context window (`MAX_TOKENS`).

Because inference is CPU-bound and uses all cores, the practical levers are: fewer/smaller chunks (see [`max_chunk_chars`](#max_chunk_chars-relevance-vs-cost) below), a faster model (see [model trade-offs](#model-speed-vs-quality)), or offloading to a server (Qdrant Inference or [Ollama](../integrations/ollama.md)).

## The HNSW drop-and-rebuild cost (DuckDB)

DuckDB's HNSW vector index makes per-row `INSERT` expensive (the graph is maintained per row). To avoid that, every bulk write **drops the HNSW index, inserts, then recreates it**:

- `begin_bulk()` runs `DROP INDEX IF EXISTS <collection>_hnsw`.
- All upserts happen with no index present (each batch runs as one transaction).
- `end_bulk()` runs `CREATE INDEX ... USING HNSW(embedding) WITH (metric='cosine')`, rebuilding the whole graph from scratch.

This rebuild is a **real cost on large repos**: the index is rebuilt over *every stored vector*, not just the ones you changed. A full `index` pays it once. But every `sync` also wraps its deletes + upserts in `begin_bulk`/`end_bulk` — so even a sync that touches one file rebuilds the entire HNSW index at the end. On a large collection that rebuild can dominate a small sync.

Two consequences worth internalizing:

- **`DELETE` is cheap.** Deleting a path's rows does *not* trigger an HNSW rebuild, so `delete_by_path` needs no index teardown. The cost is the *recreate* in `end_bulk`.
- **Sync cost scales with index size, not change size.** If syncs feel slow on a huge repo, the rebuild is the likely cause. Sync less often, or scope the index smaller (see [large monorepos](#tips-for-large-monorepos)). See [Keeping in sync](../guides/keeping-in-sync.md) for when syncs run.

> HNSW persistence on a file-backed DuckDB database is experimental and is enabled with `SET hnsw_enable_experimental_persistence = true`. SAI sets this automatically on the writable connection; the read-only MCP server does not enable it (it never writes).

## Memory footprint of the ONNX session

The `ort` embedder owns an `ort::Session` (the loaded ONNX model graph + weights) plus a tokenizer, held for the life of the process. That resident footprint scales with the model:

- **e5-small** (`384`-dim) is small and light.
- **jina code** (`768`-dim) and Ollama-served large models (e.g. `1024`-dim) are larger.

Per-batch working memory is bounded by the batch (32 passages) times the padded sequence length (up to 512 tokens) times the hidden dimension — modest next to the resident weights. The single largest lever on memory is therefore **model choice**, not batch size. The `Embedder` enum boxes the `Ort` variant precisely because the ONNX session is far larger than the `Ollama` (HTTP-only) variant.

## `max_chunk_chars`: relevance vs cost

`max_chunk_chars` caps the size (in characters) of a single chunk. It is **model-aware** by default:

- **E5 / Qdrant path:** `1400` chars (E5's 512-token window ≈ 1400 chars).
- **Large-context / code models** (jina, Qwen, Ollama large models on the DuckDB path): a much larger cap, so a whole function fits in one chunk.

The trade-off:

- **Smaller chunks** → more chunks → more embedding passes and more rows. Each chunk is more focused, which can sharpen search relevance, but you pay more compute and storage, and a function may be split across windows (the line chunker overlaps windows by 8 lines to soften this).
- **Larger chunks** → fewer, broader vectors → cheaper to index, but a single vector now averages over more code, which can blur near-duplicate detection and dilute search precision.

You can override the default explicitly:

```toml
# sai.toml
max_chunk_chars = 2000
```

Keep the cap consistent with the model's real context window — a 1400-char cap on an 8K-token model wastes capacity, and a huge cap on a 512-token E5 model just gets truncated at tokenization. See [Chunking](../reference/chunking.md) for how chunks are formed and [Configuration](../reference/configuration.md) for the key.

## Model speed vs quality

There is a direct speed/quality/footprint trade-off across the three embedding paths:

| Path | Default model | Dim | Character |
| --- | --- | --- | --- |
| `ort` (local ONNX, DuckDB) | `jinaai/jina-embeddings-v2-base-code` | 768 | Code-trained, higher quality on code; larger and slower than e5-small |
| Qdrant / Ollama default | `intfloat/multilingual-e5-small` (`Xenova/multilingual-e5-small`) | 384 | Small, fast, general-purpose text model |
| `ollama` (server) | (you choose, e.g. `nomic-embed-text`) | varies | Offloads inference to an Ollama server; speed depends on that server/hardware |

Reading:

- **e5-small** is the small/fast choice — lowest CPU cost and memory, general-purpose, used as the default for Qdrant server inference and as a lightweight `ort` option.
- **jina code** is the quality choice for code on the local `ort` path — better code understanding at a larger, slower model.
- **Ollama** moves embedding off the indexing process to a separate server; throughput then depends on that server's hardware, and the model is required (no built-in default — set `ollama.model`).

Whichever you pick, `vector_dim` must match the model (e5-small=384, jina/nomic=768, mxbai-embed-large=1024) or the index rejects the dimension. Changing models requires `--recreate` (or deleting the DuckDB file). See [Choosing a model](../guides/choosing-a-model.md) and [Backends and embedders](../reference/backends-and-embedders.md).

## Qdrant upsert batch size

On the Qdrant backend, points are upserted in batches of **32** (`UPSERT_BATCH = 32`), kept modest because server-side inference runs per request. Each batch is sent with `wait(true)`. (Separately, the CLI's embed+upsert loop groups chunks 64 at a time before handing each batch to the backend; for Qdrant those are then re-chunked to 32 per request.)

If Qdrant indexing feels slow, the bottleneck is typically the cluster's Inference throughput and network latency, not the client.

## Tips for large monorepos

The cheapest way to stay fast on a huge repo is to **index less**. Scope tightly:

- **Narrow the root.** Point `--root` at the subtree you actually search instead of the repo root:

  ```bash
  sai index --root services/api
  ```

- **Restrict extensions.** Limit `--ext` to the languages you care about so the walk skips everything else:

  ```bash
  sai index --root services/api --ext rs,ts
  ```

- **Use include globs.** Include only the paths worth indexing; everything else is excluded before it is ever read or embedded:

  ```toml
  # sai.toml
  include = ["src/**", "lib/**"]
  exclude = ["**/*.generated.*"]
  ```

- **Index subtrees into one collection.** Run `sai index` over several subtrees in turn, all targeting the **same `collection`**, to build one searchable index from selected parts of a large tree without indexing the whole thing. Use `dry-run` first to see exactly what would be indexed and why files are excluded.

These reduce both costs at once: fewer chunks means fewer embedding passes, and a smaller collection means a cheaper HNSW rebuild on every sync.

Other practical measures:

- **Sync deliberately.** Because each sync rebuilds the whole HNSW index, batch your changes and sync once rather than after every edit. See [Keeping in sync](../guides/keeping-in-sync.md).
- **Skip generated and opt-out code.** Generated files and `sai-noindexing` spans are dropped before embedding, which keeps the index focused and smaller. See [Opt-out markers](../guides/opt-out-markers.md).
- **Reuse the model cache.** Set the model cache directory so the model is downloaded once and reused offline across runs and CI. See [CI/CD](../guides/ci-cd.md) and [Environment](../reference/configuration.md#environment-variables).

## What is not measured

To be explicit: SAI ships **no benchmark suite and no published throughput numbers**. The constants here — batch 32, 512-token window, all-core intra-op threads, drop-and-rebuild HNSW, Qdrant batch 32 — are real and load-bearing, but actual wall-clock time depends entirely on your hardware, model, repo size, and chunk cap. Measure on your own repo, change one variable at a time (model, `max_chunk_chars`, scope), and compare.

If indexing or search is slower than expected, see [Troubleshooting](./troubleshooting.md).
