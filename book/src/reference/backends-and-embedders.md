# Backends and embedders

SAI (`semanticastindexer`) has a pluggable vector **backend**, and — for the DuckDB
backend — a pluggable **embedder**. The backend decides where vectors are stored and how
nearest-neighbour search runs; the embedder decides how text is turned into vectors.

The same five MCP tools and the same CLI subcommands work over either backend:
`sai_search_code`, `sai_find_similar`, `sai_find_duplicates`, `sai_index_status`,
`sai_refresh` (plus `sai_prepare_mcp_setup`). See the
[MCP server and tools](mcp-server.md) page for the tool surface and the
[CLI reference](cli.md) for the equivalent subcommands.

## Backends

| Backend | Embeddings | Storage | Search | Network on first run |
| ------- | ---------- | ------- | ------ | -------------------- |
| **qdrant** | `embedder: qdrant` → Qdrant Cloud **server-side inference** (`Document` API — no local model); `embedder: ort`/`ollama` → **local embedder**, raw-vector upsert | Qdrant collection (cosine `VectorParams`) — Cloud **or** self-hosted/OSS | server-side HNSW cosine | server mode: needs the cluster; local mode: downloads the ONNX model once |
| **duckdb** | local, via an **embedder** (see below) | single DuckDB file + **VSS/HNSW** cosine index | local `array_cosine_distance` over the HNSW index | embedder-dependent + the DuckDB VSS extension |

Select the backend in `sai-cfg.yml` (`backend: qdrant | duckdb`) or override per run with
`--backend <name>`. The backend [`factory`](#how-the-backend-is-selected) is feature-gated:
selecting a backend whose Cargo feature was not compiled in fails with a clear, actionable
error, e.g.:

```text
backend 'qdrant' selected but this binary was built without the 'qdrant' feature (rebuild with --features qdrant)
```

An unknown backend name fails with `unknown backend '<name>' (expected 'qdrant' or 'duckdb')`.

### qdrant — server-side or local embedding

The `embedder` field (default `qdrant` for this backend) decides **where** embeddings are
produced; the storage and search paths are identical either way (a plain dense cosine
collection).

**`embedder: qdrant` (default).** The backend never loads a model locally. Stored code and
queries are sent as `Document::new(text, model)` and the **cluster** produces the embedding.
The backend:

- Creates the collection (when missing) with `VectorParams(vector_dim, Distance::Cosine)`
  and a keyword payload index on `path` (so delete-by-path during `sync` is fast).
- Upserts chunks as `passage:`-prefixed `Document`s in batches of 32 (server-side
  inference runs per request).
- Queries with a `query:`-prefixed `Document`; `query_by_vector` over-fetches 8x and dedups
  by point id before truncating.
- `begin_bulk` / `end_bulk` are **no-ops** (there is no local index to drop and rebuild).
- Validates an existing collection's vector dimension on open; a mismatch errors and tells
  you to re-run with `--recreate` (or delete the collection in the Qdrant Cloud UI).

**`embedder: ort`/`ollama`.** The backend embeds **on-device** with the configured
`ort`/`ollama` embedder (exactly like the DuckDB path) and upserts **raw `Vec<f32>`** points
— no `Document`, no server-side inference. The payload is byte-identical to the server path;
only the vector source differs. The query side embeds locally too and reuses the same
raw-vector NN path (`query_by_vector`). This makes the qdrant backend work against
**self-hosted / OSS Qdrant** and lets you use any local model (e.g. the code-trained
`jinaai/jina-embeddings-v2-base-code`, 768-d) without Cloud billing. Requires a binary built
with `--features qdrant,ort` (or `qdrant,ollama`); selecting a local embedder without that
feature fails with a clear rebuild hint. Walkthrough:
[Qdrant Cloud → Local-embed mode](../integrations/qdrant-cloud.md#local-embed-mode-self-hosted--oss-qdrant).

> ℹ️ Plain OSS/local Qdrant has **no** inference engine — the `Document` API
> (`embedder: qdrant`) only works against Qdrant Cloud (or an inference-enabled
> deployment). To run against **self-hosted/OSS Qdrant, use `embedder: ort`/`ollama`**
> (above), which embeds on-device and never calls the `Document` API. See
> [Qdrant Cloud](../integrations/qdrant-cloud.md) for setup.

### duckdb — local VSS/HNSW

The DuckDB backend persists everything to a single file (`duckdb.path`, e.g.
`.index/code.duckdb`). On open it loads the **VSS** extension and enables HNSW persistence:

- The collection table stores `embedding FLOAT[vector_dim]` plus the chunk metadata
  (`id`, `path`, `language`, `start_line`, `end_line`, `text`, `symbol`, `commit_sha`,
  `dirty`, `no_duplicate`).
- The HNSW index is created `USING HNSW(embedding) WITH (metric='cosine')`. Search uses
  `array_cosine_distance` and returns `score = 1 - distance` (higher is better, matching
  Qdrant's cosine score).
- HNSW can return the same id more than once, so `query_by_vector` over-fetches 8x, dedups
  by id, then truncates to the limit.
- A writable open sets `SET hnsw_enable_experimental_persistence = true` so the index
  survives across close/reopen. The MCP server opens the file **read-only** and does not
  enable persistence writes (a read-only handle must not mutate the DB).
- The VSS extension is loaded with a pure `LOAD vss;` first (works on read-only handles if
  VSS was already installed by any process), then `INSTALL vss; LOAD vss;`, then the
  community repo. If none succeed you get an actionable error; pre-install once with a
  writable run or `duckdb -c "INSTALL vss;"`. See
  [Troubleshooting and FAQ](../operations/troubleshooting.md).

> ℹ️ **DuckDB `sync` recall note.** The DuckDB VSS HNSW index loses recall after in-place
> deletes, so `sync` drops and recreates the index around its changed-file loop
> (`begin_bulk` drops the index, `end_bulk` recreates it) — effectively a full rebuild of
> the HNSW graph. This is correct but means a DuckDB `sync` is **not** as cheap as
> Qdrant's, where `begin_bulk`/`end_bulk` are no-ops. (`DELETE` alone does not trigger an
> HNSW rebuild, so `delete_by_path` needs no index teardown.)

## DuckDB embedders

The DuckDB backend produces vectors locally via a pluggable embedder
(`embedder: ort | ollama` in `sai-cfg.yml`, or `--embedder <name>`). `ort` is the default.

| Embedder | How | Network on first run |
| -------- | --- | -------------------- |
| **ort** | raw **ONNX Runtime** (`ort` 2.x) + tokenizer; downloads `onnx/model.onnx` + `tokenizer.json` from `duckdb.model_repo` via `hf-hub` | downloads the ONNX model + tokenizer from HuggingFace (first run); none afterwards |
| **ollama** | remote **Ollama** HTTP server: `POST {ollama.url}/api/embed` with `{ "model": …, "input": [...] }` | none to download — but needs a running Ollama with the model pulled |

Like backends, the embedder is feature-gated: selecting an embedder whose Cargo feature was
not compiled in fails with `embedder '<name>' selected but this binary was built without the
'<name>' feature (rebuild with --features <name>)`. An unknown embedder name fails with
`unknown embedder '<name>' (expected 'ort' or 'ollama')`.

### The `ort` pipeline

For each batch the on-device ONNX embedder runs this exact sequence:

1. **Prefix** — apply the resolved prefix policy (`format_passage` / `format_query`); see
   [Embedding prefixes](#embedding-prefixes) below.
2. **Tokenize** — pad/truncate to **512 tokens** (`MAX_TOKENS`), padding `BatchLongest` so
   every row in a batch is the same length (ONNX needs rectangular tensors).
3. **Run ONNX** — feed `input_ids` + `attention_mask` (and a zeroed `token_type_ids` *iff*
   the loaded model declares that input) and read `last_hidden_state`
   `[batch, seq, hidden]`. If the export names the first output differently, the first
   output by index is used as a fallback.
4. **Mean-pool over the attention mask** — sum hidden states weighted by the mask, divide by
   the mask sum (so padding tokens contribute nothing).
5. **L2-normalize** — divide each pooled vector by its L2 norm, so cosine similarity is a
   plain dot product.

Batches are sized at 32 passages per forward pass and length-sorted before batching (then
scattered back to caller order) so one long passage does not inflate a whole batch with
padding. Inference is synchronous CPU work sized to `available_parallelism()` intra-op
threads — acceptable for a one-shot CLI batch job.

### The `ollama` embedder

The Ollama embedder POSTs prefixed inputs to `{ollama.url}/api/embed` and reads
`{ "embeddings": [[...], ...] }`. Requirements:

- `ollama.model` is **required** — there is no default (Ollama embed models vary). Set it to
  an embed-capable model. Construction fails clearly if it is unset:
  `embedder 'ollama' selected but ollama.model is unset — set ollama.model … (e.g. nomic-embed-text)`.
- `ollama.url` defaults to `http://localhost:11434` upstream; a trailing `/` is trimmed.
- Start the server (`ollama serve`) and pull the model (`ollama pull nomic-embed-text`). A
  non-success HTTP status produces an error that suggests `ollama pull <model>`; a
  connection failure asks whether `ollama serve` is running.

See [Ollama](../integrations/ollama.md) for end-to-end setup.

## `vector_dim` must match the model

`vector_dim` is **validated at runtime** and must equal the embedder's output dimension —
the DuckDB column is literally `FLOAT[vector_dim]`. A produced-vs-configured mismatch is a
clear error:

```text
embedder produced 768-d vectors but vector_dim=384 — set vector_dim to match the model
(e5-small=384, nomic-embed-text=768, mxbai-embed-large=1024)
```

There are two layers of this check:

- **On open** (DuckDB), if the table already exists, the `embedding` column type is compared
  against `FLOAT[vector_dim]`. A mismatch is a typed `DimMismatch` error carrying the DuckDB
  file path, so the CLI can offer an interactive *delete-and-rebuild* instead of
  string-matching. The message names the actual vs expected type and tells you to delete the
  file or re-index with `--recreate`. Qdrant performs the equivalent check against the
  collection's configured vector size on open.
- **Per embedding** (`check_dim`), every produced query/passage vector is checked before it
  hits the index.

Reference dimensions: `e5-small = 384`, `nomic-embed-text = 768`, `mxbai-embed-large = 1024`,
`jina-embeddings-v2-base-code = 768`.

> ⚠️ Changing `vector_dim` (or the model) requires a fresh index. Delete the DuckDB file
> (e.g. `.index/code.duckdb`) or re-index with `--recreate`.

## Embedding prefixes

A model-aware prefix policy is resolved once when the plan is built (explicit
`prefix_style` config wins; otherwise it is auto-detected from the model name) and applied
by **both** embedders and the Qdrant `Document` path through one shared pair of helpers.

| `prefix_style` | Stored passage | Query | Auto-detected when model name contains |
| -------------- | -------------- | ----- | -------------------------------------- |
| `e5` | `passage: <text>` | `query: <text>` | `e5` |
| `qwen` | `<text>` (bare) | `Instruct: Given a code search query, retrieve relevant code\nQuery: <text>` | `qwen` |
| `none` | `<text>` (bare) | `<text>` (bare) | (anything else) |

> ⚠️ **E5 passage/query asymmetry caveat.** The E5 family is trained with the asymmetric
> `passage:` / `query:` scheme — both embedders and Qdrant apply it when `prefix_style: e5`.
> A **non-E5** model (e.g. many Ollama models, or a symmetric code model) may want different
> (or no) prefixes; relevance can suffer if it was not trained with this asymmetric scheme.
> Set `prefix_style: none` (or `qwen`) to match the model. See
> [Chunking → embedding prefixes](chunking.md#embedding-prefixes) for the chunk-side picture.

## Offline / cached `ort`

For air-gapped or repeatable runs, point `duckdb.model_cache` at a pre-populated HuggingFace
cache directory. The `ort` embedder passes it through as the `hf-hub` cache dir, so it
reuses `onnx/model.onnx` and `tokenizer.json` from disk instead of fetching them. If the
download fails, the error suggests exactly this:
`… (check network or set duckdb.model_cache to a pre-populated dir)`.

> 🔒 **Qdrant creds stay in the environment.** `QDRANT_URL` / `QDRANT_API_KEY` are read from
> the environment, never from YAML. See
> [Environment variables](configuration.md#environment-variables) and
> [Security and privacy](../operations/security.md).

## Recommended model for code de-duplication

`e5-small` is a multilingual **text** model: distinct functions in the same language all
embed ~0.91 cosine-similar, so `sai_find_duplicates` collapses into one giant cluster at any
threshold. A **code-trained** embedder spreads functions far apart and surfaces real
near-duplicates (even across different names). Recommended drop-in (stays on the offline
`ort` path):

```yaml
model: jinaai/jina-embeddings-v2-base-code   # 161M, code-trained (CodeSearchNet)
vector_dim: 768                              # MUST match the model
prefix_style: none                           # symmetric model — no passage:/query: prefix
duckdb:
  model_repo: jinaai/jina-embeddings-v2-base-code   # ort downloads onnx/model.onnx + tokenizer.json
similarity:
  duplicate_min_score: 0.88                  # code models run LOWER than e5 (no mega-cluster at any threshold)
```

See [Choosing a model](../guides/choosing-a-model.md) for the full comparison and
[Tuning similarity](../guides/tuning-similarity.md) for thresholds.

> ⚠️ **First-run download caveat (`hf-hub` 0.3 + Xet).** The pinned `hf-hub` (0.3) fails to
> fetch `tokenizer.json` from this repo because it is on HuggingFace **Xet** storage
> (`relative URL without a base`). Until `hf-hub` is upgraded, stage the tokenizer once into
> the HF cache:
>
> ```bash
> SNAP=~/.cache/huggingface/hub/models--jinaai--jina-embeddings-v2-base-code/snapshots/*/
> curl -sL https://huggingface.co/jinaai/jina-embeddings-v2-base-code/resolve/main/tokenizer.json -o $SNAP/tokenizer.json
> ```
>
> The `onnx/model.onnx` download works; only `tokenizer.json` needs staging. After that,
> normal runs use the cache — no `HF_HUB_OFFLINE` needed. Changing `vector_dim` requires a
> fresh index: delete `.index/code.duckdb` (or run with `--recreate`). More fixes live in
> [Troubleshooting and FAQ](../operations/troubleshooting.md).

## Qdrant requirements

These apply to **`embedder: qdrant`** (Qdrant Cloud server-side inference). With
**`embedder: ort`/`ollama`** you need neither Cloud Inference nor an API key — any reachable
Qdrant (including a local `docker run qdrant/qdrant`) works, since embedding happens
on-device; just set `QDRANT_URL` to the gRPC endpoint (`:6334`).

- A Qdrant Cloud cluster with **Inference enabled** and the `intfloat/multilingual-e5-small`
  model available (Cluster → *Inference* tab). Vector size **384**, context window **512
  tokens**.
- Credentials via the environment (never hard-coded):

```bash
export QDRANT_URL="https://<cluster-id>.<region>.aws.cloud.qdrant.io:6334"   # gRPC port :6334
export QDRANT_API_KEY="<key from the cluster's API Keys tab>"
```

If `QDRANT_API_KEY` is unset, the backend warns that Qdrant Cloud will reject the request.
If a server-side upsert fails it asks whether Inference is enabled on the cluster. Full
walkthrough: [Qdrant Cloud](../integrations/qdrant-cloud.md).

## How the backend is selected

The `factory` reads `backend` from the resolved plan and opens the DuckDB arm per an
`Access` mode:

- **`ReadWrite`** — normal open with index maintenance, writes, and HNSW persistence
  (indexing, `refresh`, `sync`).
- **`ReadOnly`** — search-only path used by the MCP server and the CLI `similar` /
  `duplicates` subcommands. The DuckDB file must already exist (a missing index is an
  actionable error, since read-only search never indexes).

Qdrant is a remote, already-read-capable path, so both access modes behave identically
there.

## See also

- [Choosing a model](../guides/choosing-a-model.md) — model trade-offs and recommendations.
- [Tuning similarity](../guides/tuning-similarity.md) — thresholds and scoring.
- [Chunking](chunking.md) — how source is sliced into embeddable chunks.
- [Configuration](configuration.md) — every `sai-cfg.yml` key.
- [MCP server and tools](mcp-server.md) — `sai_`-prefixed tools over either backend.
- [Qdrant Cloud](../integrations/qdrant-cloud.md) and
  [Ollama](../integrations/ollama.md) — backend/embedder setup.
- [Troubleshooting and FAQ](../operations/troubleshooting.md) — VSS, downloads, dim
  mismatches.
- [Glossary](../concepts/glossary.md) — terms used above.
