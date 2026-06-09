# Ollama

[Ollama](https://ollama.com) is a local HTTP server that runs embedding (and chat)
models on your machine. SAI's **`ollama` embedder** uses it as the embedding backend
for the **DuckDB** vector store: instead of running an ONNX model in-process (the
`ort` embedder), SAI POSTs your chunk text to a running Ollama server and stores the
returned vectors in a local DuckDB file.

This is a good fit when an Ollama service is **already running** — for example a shared
CI/CD box or a developer machine that hosts embedding models for several tools — so SAI
doesn't have to download or load a model itself.

> The `ollama` embedder is only used by the **`duckdb`** backend. The `qdrant` backend
> does its embedding server-side (Qdrant Cloud inference) and ignores these settings.
> See [Backends & embedders](../reference/backends-and-embedders.md).

## 1. Run the Ollama server

```bash
ollama serve
```

By default the server listens on `http://localhost:11434`, which is also SAI's default
`ollama.url`.

## 2. Pull an embedding model

Pick an **embedding-capable** model and pull it. Two common choices:

```bash
# 1024-dimensional
ollama pull mxbai-embed-large

# 768-dimensional
ollama pull nomic-embed-text
```

Browse more options in [Ollama's embedding models](https://ollama.com/search?c=embedding).

## 3. Configure SAI

Select the `duckdb` backend with the `ollama` embedder and point it at your server.
The critical part is matching **`vector_dim` to the model's output dimension** — SAI
validates this at runtime and fails with a clear error
(e.g. `embedder produced 768-d vectors but vector_dim=384 …`) on a mismatch.

```yaml
backend: duckdb
embedder: ollama
vector_dim: 1024            # MUST match the model (mxbai-embed-large = 1024)
ollama:
  url: http://localhost:11434   # default — omit if unchanged
  model: mxbai-embed-large
```

Using `nomic-embed-text` instead:

```yaml
backend: duckdb
embedder: ollama
vector_dim: 768             # nomic-embed-text = 768
ollama:
  model: nomic-embed-text
```

Reference values:

| Model | `vector_dim` |
| ----- | ------------ |
| `mxbai-embed-large` | `1024` |
| `nomic-embed-text` | `768` |

Notes from the config defaults:

- `ollama.url` defaults to `http://localhost:11434` — you only need to set it when
  Ollama runs elsewhere.
- `ollama.model` defaults to `mxbai-embed-large` (1024-d) when the `ollama` embedder is
  selected. If you set it to the empty string, SAI errors with a message telling you to
  set `ollama.model`.

You can also override the backend/embedder per run on the CLI instead of in YAML:

```bash
sai index . --backend duckdb --embedder ollama --vector-dim 1024
```

See [Configuration](../reference/configuration.md) and [the CLI reference](../reference/cli.md)
for the full set of keys and flags.

## How it works

For each batch of chunks, SAI POSTs to the embed endpoint:

```http
POST {ollama.url}/api/embed
{ "model": "<ollama.model>", "input": ["<text>", ...] }
```

and reads the embeddings back from the `embeddings` field of the JSON response. A
trailing slash on `ollama.url` is trimmed, so both `http://localhost:11434` and
`http://localhost:11434/` work. If the server is unreachable or the model isn't pulled,
SAI surfaces an actionable error (for example, suggesting `ollama pull <model>`).

## The E5-prefix caveat

SAI was built around the E5 family of text embedders, which use **asymmetric prefixes**:
indexed text gets a `passage:` prefix and search queries get a `query:` prefix. The
`ollama` embedder applies the **same prefix policy** as the `ort` embedder — controlled
by `prefix_style` — so by default it prepends `passage:`/`query:` to your inputs.

Most Ollama embedding models (`mxbai-embed-large`, `nomic-embed-text`, …) are **not**
E5 models and were not trained with this asymmetric scheme, so the injected prefixes can
**hurt relevance**. If your model isn't an E5 model, set a symmetric (bare) prefix policy:

```yaml
embedder: ollama
prefix_style: none          # don't prepend passage:/query:
ollama:
  model: nomic-embed-text
vector_dim: 768
```

`prefix_style` accepts `e5`, `qwen`, or `none`; when unset it is auto-detected from the
model name. For the full explanation of when to keep or drop prefixes, see
[Choosing a model](../guides/choosing-a-model.md).

## Good fit for CI/CD

Because the `ollama` embedder downloads **nothing** at index time — it just calls an HTTP
endpoint — it pairs well with environments where an embedding service is already up. In
CI/CD you can run `ollama serve` (with the model pre-pulled) and point every SAI job at it
via `ollama.url`, keeping the indexing step fast and network-light. See
[CI/CD](../guides/ci-cd.md).

## See also

- [Backends & embedders](../reference/backends-and-embedders.md) — how `ort` vs `ollama`
  differ on the DuckDB backend.
- [Configuration](../reference/configuration.md) — the `ollama:` and `similarity:` keys.
- [Choosing a model](../guides/choosing-a-model.md) — picking a model and prefix policy.
- [Ollama embedding models](https://ollama.com/search?c=embedding).
