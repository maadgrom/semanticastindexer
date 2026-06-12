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

> ⚠️ **Plain OSS / local Qdrant has no inference engine.** The `Document` API only
> works against Qdrant Cloud (or another inference-enabled deployment). If you point
> the `qdrant` backend at a vanilla local Qdrant, upserts and queries that rely on
> the `Document` API will fail. To run fully locally, use the `duckdb` backend
> instead — see [backends and embedders](../reference/backends-and-embedders.md).

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

## Related pages

- [Configuration → Environment variables](../reference/configuration.md#environment-variables) — `qdrant.url`, `QDRANT_URL`, `QDRANT_API_KEY`.
- [Backends and embedders](../reference/backends-and-embedders.md) — qdrant vs. duckdb.
- [../reference/configuration.md](../reference/configuration.md) — `backend`, `model`, `vector_dim`.
