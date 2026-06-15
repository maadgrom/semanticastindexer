# Qdrant Cloud

The `qdrant` backend stores vectors in a Qdrant Cloud collection and produces
embeddings using **Qdrant Cloud's server-side inference** — no embedding model
runs locally. This page covers how to set up a cluster, what `semanticastindexer`
(SAI) expects from it, and how the collection is created and validated.

For the full backend matrix (qdrant vs. duckdb), see
[backends and embedders](../reference/backends-and-embedders.md). For how the connection
URL and API key are configured, see [Configuration → Environment variables](../reference/configuration.md#environment-variables).

## How server-side inference works

With the `qdrant` backend, SAI never loads a tokenizer or ONNX model. Instead it
hands raw text to Qdrant Cloud via the **`Document` API**: each stored chunk is
sent as a `Document` (a text string plus a model name), and the cluster computes
the vector inside the cluster. The same applies to searches — a query is sent as
a `Document`, embedded server-side, then used for nearest-neighbour lookup.

Stored chunks are embedded as `passage: <code>` and queries as `query: <text>`,
matching E5's asymmetric prefix scheme.

> ⚠️ **Server-side inference requires Qdrant Cloud (or an inference-enabled deployment).**
> The `Document` API used by the default `embedder: qdrant` path does not exist in plain
> OSS / self-hosted Qdrant. If you point the `qdrant` backend at a vanilla local Qdrant
> without switching to local-embed mode, upserts and queries will fail. To use the `qdrant`
> backend with OSS / self-hosted Qdrant, set `embedder: ort` (or `ollama`) — see
> [Local-embed mode](#local-embed-mode-self-hosted--oss-qdrant) below. To avoid running
> Qdrant at all, use the `duckdb` backend instead — see
> [backends and embedders](../reference/backends-and-embedders.md).

## Cluster requirements

Set up a Qdrant Cloud cluster with:

- **Inference enabled** on the cluster.
- The **`intfloat/multilingual-e5-small`** model available — enable it under the
  **Cluster → Inference** tab.
- Vector size **384**.
- Context window **512 tokens**.

These match SAI's expectations for the Qdrant path; chunks are tokenized
within the 512-token context window server-side.

## Connection

The cluster **URL** can live in `sai-cfg.yml` (`qdrant.url`) or come from the
`QDRANT_URL` environment variable, which **overrides** the YAML value. The **API key is a
secret** and is read **only** from `QDRANT_API_KEY` in the environment — never put it in YAML.

In `sai-cfg.yml`:

```yaml
backend: qdrant
qdrant:
  url: https://<cluster-id>.<region>.aws.cloud.qdrant.io:6334   # gRPC port :6334
```

The API key always comes from the environment (optionally the URL too):

```bash
export QDRANT_API_KEY="<key from the cluster's API Keys tab>"
# optional: supply or override the URL from the environment instead of YAML
export QDRANT_URL="https://<cluster-id>.<region>.aws.cloud.qdrant.io:6334"
```

Notes from the connection code:

- A URL is **required**: set `qdrant.url` or `QDRANT_URL`. If neither is set, SAI errors
  telling you to provide the cluster gRPC endpoint
  (`https://<id>.<region>.aws.cloud.qdrant.io:6334`).
- Use the **gRPC port `:6334`**, not the REST port.
- If `QDRANT_API_KEY` is unset (or empty), SAI prints a warning and proceeds — Qdrant
  Cloud will then reject the request, so set the key.

Select the backend in `sai-cfg.yml` (`backend: qdrant`) or override per run with
`--backend qdrant`. See the [configuration reference](../reference/configuration.md)
and the [CLI reference](../reference/cli.md).

## How the collection is created

The first time SAI runs against a missing collection, it creates one configured for
the Qdrant inference path:

- Vector params: size = `vector_dim` (384), **distance = Cosine**.
- A **keyword payload index on `path`**, which makes the delete-by-path filter used
  during `sync` fast.

On success you'll see output like:

```text
created collection '<name>' (384 dims, cosine, path index)
```

If the collection already exists and you are **not** recreating it, SAI prints
`using existing collection '<name>'` and validates the stored dimension (see below)
before reusing it.

## Dimension validation on reuse

When reusing an existing collection, SAI reads the collection's configured vector
dimension and compares it to the `vector_dim` of the current run. If they differ,
the run **fails fast** with a message like:

```text
Qdrant collection '<name>' has vector dimension <N> but this run uses vector_dim=<M>.
This usually means the embedding model was changed without recreating the collection.
Re-run with --recreate (or manually delete the collection in the Qdrant Cloud UI).
```

This catches the common mistake of pointing the indexer at an old collection after
changing the embedding model / `vector_dim`. Without it, Qdrant would otherwise fail
later with dimension-mismatch errors during upsert or query.

## Changing models: one-time re-index

The collection's vector size is fixed at creation. If you change the model (and thus
`vector_dim`), you must recreate the collection — there is no in-place migration.
Re-index once with `--recreate`:

```bash
# Drops the existing collection, recreates it with the new dims, and re-indexes.
sai index --backend qdrant --recreate
```

`--recreate` drops the existing collection (`dropped existing collection '<name>'`)
and creates a fresh one with the current vector params and the `path` payload index.
Alternatively, delete the collection manually in the Qdrant Cloud UI and let the next
run create it.

See the [CLI reference](../reference/cli.md) for the `index` command and
`--recreate` flag.

## Local-embed mode (self-hosted / OSS Qdrant)

Setting `embedder: ort` (or `ollama`) switches the `qdrant` backend from Qdrant Cloud's
`Document` API to local embedding. (`embedder: qdrant`, the default for this backend, is the
server-side path.) In local-embed mode SAI embeds code and queries locally (using the `ort`
or `ollama` embedder) and upserts raw `Vec<f32>` vectors directly — no server-side inference
engine is required. This unlocks self-hosted / OSS Qdrant instances, which have no inference
engine, and lets code-trained models such as `jinaai/jina-embeddings-v2-base-code` run
against a local cluster with no Cloud billing.

### Configuration

```yaml
backend: qdrant
embedder: ort            # ort or ollama — selects the local embedder (qdrant = server-side)
model: jinaai/jina-embeddings-v2-base-code
vector_dim: 768          # MUST match the model
prefix_style: none       # symmetric code model — no passage:/query: prefix
qdrant:
  url: http://localhost:6334   # gRPC port :6334
```

The `embedder` field is the single knob: `qdrant` (default) means Qdrant Cloud server-side
inference; `ort`/`ollama` mean embed locally. There is no separate `qdrant.inference` knob.

### Build requirement

Local-embed mode requires the `ort` or `ollama` Cargo feature:

```bash
cargo build --release --features qdrant,ort
# or, for Ollama:
cargo build --release --features qdrant,ollama
```

Selecting `embedder: ort`/`ollama` for the qdrant backend in a binary compiled without either
feature fails with a clear error that tells you to rebuild with the appropriate feature flag.

### OSS Qdrant notes

- Use the **gRPC port `:6334`**, not the REST port.
- No API key is needed for an unauthenticated OSS Qdrant instance — omit `QDRANT_API_KEY`.
- The `duplicates` sweep (`sai_find_duplicates` / `duplicates` CLI subcommand) was already
  inference-free and is unaffected by this setting; it always operates on raw stored vectors.
- To run OSS Qdrant locally: `docker run -p 6333:6333 -p 6334:6334 qdrant/qdrant`.

## Related pages

- [Configuration → Environment variables](../reference/configuration.md#environment-variables) — `qdrant.url`, `QDRANT_URL`, `QDRANT_API_KEY`.
- [Backends and embedders](../reference/backends-and-embedders.md) — qdrant vs. duckdb.
- [../reference/configuration.md](../reference/configuration.md) — `backend`, `model`, `vector_dim`.
