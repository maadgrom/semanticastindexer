# Backends & embedders

`semanticastindexer` has a pluggable vector backend, and — for the DuckDB backend — a
pluggable embedder.

## Backends

| Backend | Embeddings | Storage | Build | Network on first run |
| ------- | ---------- | ------- | ----- | -------------------- |
| **qdrant** | Qdrant Cloud **server-side inference** (`Document` API — no local model) | Qdrant Cloud collection | included in `--features all` | none locally; needs the cluster |
| **duckdb** | local, via an **embedder** (see below) | DuckDB file + **VSS/HNSW** cosine index | included in `--features all` | embedder-dependent + the DuckDB VSS extension |

Select the backend in `indexer.yaml` (`backend: qdrant | duckdb`) or override per run
with `--backend <name>`. Selecting a backend whose feature was not compiled in fails
with a clear "rebuild with --features …" error.

## DuckDB embedders

The DuckDB backend produces vectors locally via a pluggable embedder
(`embedder: ort | ollama` in `indexer.yaml`, or `--embedder <name>`):

| Embedder | How | Build | Network on first run |
| -------- | --- | ----- | -------------------- |
| **ort** | raw **ONNX Runtime** (`ort` 2.x) + tokenizer; downloads `onnx/model.onnx` + `tokenizer.json` from `duckdb.model_repo` | included in `--features all` | downloads the ONNX model + tokenizer from HuggingFace (first run) |
| **ollama** | remote **Ollama** HTTP server: `POST {ollama.url}/api/embed` | included in `--features all` | none to download — but needs a running Ollama with the model pulled |

- The **ort** pipeline: prefix → tokenize (pad/truncate to 512) → ONNX `last_hidden_state`
  → **mean-pool over the attention mask** → **L2-normalize** (384d for e5-small).
- The **ollama** embedder requires `ollama.model` (no default; e.g. `nomic-embed-text`).
  Start the server (`ollama serve`) and pull the model (`ollama pull nomic-embed-text`).
- **`vector_dim` MUST match the embedder model** and is validated at runtime — a mismatch
  is a clear error (`embedder produced 768-d vectors but vector_dim=384 …`). e5-small = 384;
  Ollama models differ: nomic-embed-text = 768, mxbai-embed-large = 1024.
- **E5 prefix caveat:** both embedders apply E5's `passage:`/`query:` prefixes. A
  **non-E5** Ollama model may want different (or no) prefixes — relevance can suffer if it
  was not trained with this asymmetric scheme. See [chunking → prefixes](chunking.md#embedding-prefixes).
- **Offline ort:** set `duckdb.model_cache` to a pre-populated HF cache dir.
- **Qdrant creds stay in the environment** (`QDRANT_URL` / `QDRANT_API_KEY`) — never in YAML.

### Recommended model for code de-duplication

`e5-small` is a multilingual **text** model: distinct functions in the same language all
embed ~0.91 cosine-similar, so `duplicates` collapses into one giant cluster. A
**code-trained** embedder spreads functions far apart and surfaces real near-duplicates
(even across different names). Recommended drop-in (stays on the offline `ort` path):

```yaml
model: jinaai/jina-embeddings-v2-base-code   # 161M, code-trained (CodeSearchNet)
vector_dim: 768                              # MUST match the model
prefix_style: none                           # symmetric model — no passage:/query: prefix
duckdb:
  model_repo: jinaai/jina-embeddings-v2-base-code   # ort downloads onnx/model.onnx + tokenizer.json
similarity:
  duplicate_min_score: 0.88                  # code models run LOWER than e5 (no mega-cluster at any threshold)
```

> ⚠️ **First-run download caveat.** The pinned `hf-hub` (0.3) fails to fetch
> `tokenizer.json` from this repo (HuggingFace **Xet** storage → `relative URL without a
> base`). Until `hf-hub` is upgraded, stage the tokenizer once into the HF cache:
>
> ```bash
> SNAP=~/.cache/huggingface/hub/models--jinaai--jina-embeddings-v2-base-code/snapshots/*/
> curl -sL https://huggingface.co/jinaai/jina-embeddings-v2-base-code/resolve/main/tokenizer.json -o $SNAP/tokenizer.json
> ```
>
> (The `onnx/model.onnx` download works; only `tokenizer.json` needs staging. After that,
> normal runs use the cache — no `HF_HUB_OFFLINE` needed.) `vector_dim` changes require
> a fresh index: delete `.index/code.duckdb` (or run with `--recreate`).

## Qdrant requirements

- A Qdrant Cloud cluster with **Inference enabled** and the `intfloat/multilingual-e5-small`
  model available (Cluster → *Inference* tab). Vector size **384**, context window **512 tokens**.
- Credentials via environment (never hard-coded):

```bash
export QDRANT_URL="https://<cluster-id>.<region>.aws.cloud.qdrant.io:6334"   # gRPC port :6334
export QDRANT_API_KEY="<key from the cluster's API Keys tab>"
```

> ⚠️ Plain OSS/local Qdrant has **no** inference engine — the `Document` API only works
> against Qdrant Cloud (or an inference-enabled deployment).

> ℹ️ **DuckDB sync recall note.** The DuckDB VSS HNSW index loses recall after in-place
> deletes, so `sync` drops and recreates the index around its changed-file loop
> (`begin_bulk`/`end_bulk`) — effectively a full index rebuild of the HNSW graph. This is
> correct but means a DuckDB `sync` is not as cheap as Qdrant's (where begin/end are no-ops).
