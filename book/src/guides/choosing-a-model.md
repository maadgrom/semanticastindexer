# Choosing a model

The quality of your search and near-duplicate results depends almost entirely on
the embedding model you pick. This guide helps you choose an **embedder** (the
thing that turns code into vectors) and a **model** that suits your task — and
shows the YAML you need so the pieces line up.

The two settings that matter most are `embedder` (how vectors are produced) and
`model` (which weights). A third, `vector_dim`, **must** match the model — get it
wrong and SAI errors at runtime.

## Two embedders for the DuckDB backend

The DuckDB backend produces vectors locally through a pluggable **embedder**,
selected with `embedder: ort | ollama` in `indexer.yaml` (or `--embedder <name>`
on the CLI). The default is `ort`.

| Embedder | How it runs | Network | Needs a server? |
| -------- | ----------- | ------- | --------------- |
| **ort** (default) | Raw ONNX Runtime in-process: downloads `onnx/model.onnx` + `tokenizer.json` from a Hugging Face repo, then runs CPU inference locally | Only on first run, to fetch the model + tokenizer (then cached) | No — fully offline after the first download |
| **ollama** | HTTP `POST {ollama.url}/api/embed` to a running Ollama server | None to download | Yes — `ollama serve` must be running with the model pulled |

Pick `ort` when you want a self-contained, offline-after-first-run binary. Pick
`ollama` when you already run Ollama and want to reuse its model library (or want
a larger, higher-dimension model than the bundled ONNX defaults).

> The third backend, **qdrant**, does not use a local embedder at all — it relies
> on Qdrant Cloud server-side inference. See
> [Backends & embedders](../reference/backends-and-embedders.md) and
> [Qdrant Cloud](../integrations/qdrant-cloud.md).

## Models at a glance

| Model | Dim | Embedder | Trained on | De-dup quality |
| ----- | --- | -------- | ---------- | -------------- |
| `jinaai/jina-embeddings-v2-base-code` | **768** | `ort` (this is the ort default) | Code (CodeSearchNet) | Good — spreads functions apart |
| `intfloat/multilingual-e5-small` (Xenova ONNX variant) | **384** | `ort` / qdrant | General multilingual **text** | Poor — collapses functions into one cluster |
| `mxbai-embed-large` | **1024** | `ollama` (the Ollama default) | General text | Use only after tuning thresholds |
| `nomic-embed-text` | **768** | `ollama` | General text | Use only after tuning thresholds |

### Why a code-trained model matters for de-dup

`intfloat/multilingual-e5-small` is a multilingual **text** model. Distinct
functions written in the same language all embed at roughly `0.91` cosine
similarity to each other, so `duplicates` collapses everything into one giant
mega-cluster — it cannot tell real near-duplicates from merely "both are Rust."

A **code-trained** embedder such as `jinaai/jina-embeddings-v2-base-code` spreads
unrelated functions far apart and surfaces genuine near-duplicates (even when the
two copies have different names). That is why it is the default for the offline
`ort` path. Code models also run at *lower* absolute cosine scores than e5, so
their duplicate thresholds sit lower (see below and
[Tuning similarity](./tuning-similarity.md)).

## `vector_dim` MUST match the model

`vector_dim` is validated at runtime. A mismatch is a hard error, e.g.:

```text
embedder produced 768-d vectors but vector_dim=384 …
```

Use these values:

- `jinaai/jina-embeddings-v2-base-code` → `768`
- `intfloat/multilingual-e5-small` → `384`
- `nomic-embed-text` → `768`
- `mxbai-embed-large` → `1024`

If you change the model (and therefore `vector_dim`) on an existing DuckDB index,
you need a **fresh index**: delete `.index/code.duckdb` or run with `--recreate`.

## Recipes

### Default (offline ONNX, code-trained) — recommended

This is what you get with the `ort` embedder and no `model` set, stated
explicitly:

```yaml
backend: duckdb
embedder: ort
model: jinaai/jina-embeddings-v2-base-code   # code-trained (CodeSearchNet)
vector_dim: 768                              # MUST match the model
prefix_style: none                           # symmetric model — no passage:/query: prefix
duckdb:
  model_repo: jinaai/jina-embeddings-v2-base-code   # ort downloads onnx/model.onnx + tokenizer.json
similarity:
  duplicate_min_score: 0.88                  # code models run lower than e5
```

> **First-run note for this repo.** The pinned `hf-hub` cannot fetch
> `tokenizer.json` from the Jina repo (Hugging Face Xet storage). Stage it once
> into the HF cache before the first index — see the caveat in
> [Backends & embedders](../reference/backends-and-embedders.md). The
> `onnx/model.onnx` download itself works.

### Ollama (HTTP server)

The `ollama` embedder requires `ollama.model` — there is **no** E5 fallback, and
construction fails clearly if it is unset. Start the server and pull the model
first:

```bash
ollama serve
ollama pull nomic-embed-text
```

```yaml
backend: duckdb
embedder: ollama
model: nomic-embed-text   # informational label
vector_dim: 768           # nomic-embed-text is 768-d
ollama:
  url: http://localhost:11434   # default
  model: nomic-embed-text       # required — the model Ollama actually runs
```

For `mxbai-embed-large` instead, set `ollama.model: mxbai-embed-large` and
`vector_dim: 1024`.

> Ollama text models are not code-trained, so expect the same de-dup limitation
> as e5: index and search work, but you will likely need to retune the duplicate
> thresholds. See [Tuning similarity](./tuning-similarity.md).

## Prefix styles auto-detect

Embedding prefixes (the `passage:`/`query:` scheme E5 was trained with) are
chosen automatically from the model name when you do not set `prefix_style`:

- model name contains `e5` → `e5` (asymmetric `passage:` / `query:`)
- model name contains `qwen` → `qwen` (bare passages, instructed query)
- otherwise → `none` (both sides bare)

So the Jina code model auto-detects to `none` (correct — it is symmetric), and
e5 auto-detects to `e5`. Override with `prefix_style: e5 | qwen | none` only when
the auto-detection guesses wrong for your model — for example a non-E5 Ollama
model that should run with no prefix. Relevance can suffer if you force an
asymmetric prefix on a model that was not trained with one.

## Offline / cached models (ort)

The `ort` embedder downloads from `duckdb.model_repo` on first run and caches the
result. To run fully offline (air-gapped CI, no network), point
`duckdb.model_cache` at a pre-populated Hugging Face cache directory:

```yaml
duckdb:
  model_repo: jinaai/jina-embeddings-v2-base-code
  model_cache: /path/to/huggingface/cache   # reuse a pre-populated HF cache, no network
```

## Where to find models

- **ONNX models for `ort`:** any Hugging Face repo that ships `onnx/model.onnx`
  and `tokenizer.json`. Browse
  [Hugging Face ONNX models](https://huggingface.co/models?library=onnx). The
  defaults live at
  [jinaai/jina-embeddings-v2-base-code](https://huggingface.co/jinaai/jina-embeddings-v2-base-code)
  and [Xenova/multilingual-e5-small](https://huggingface.co/Xenova/multilingual-e5-small).
- **Embedding models for `ollama`:** browse
  [Ollama embedding models](https://ollama.com/search?c=embedding), including
  [mxbai-embed-large](https://ollama.com/library/mxbai-embed-large) and
  [nomic-embed-text](https://ollama.com/library/nomic-embed-text).

## See also

- [Backends & embedders](../reference/backends-and-embedders.md) — the full
  backend/embedder reference and the Jina first-run staging caveat.
- [Configuration](../reference/configuration.md) — every `indexer.yaml` key,
  including the `duckdb` and `ollama` sub-sections.
- [Tuning similarity](./tuning-similarity.md) — adjusting `duplicate_min_score`
  and the other thresholds once you pick a model.
