# Environment variables

SAI reads only **two** environment variables, and both are specific to the
**Qdrant** backend. Everything else — embedder URLs, model names, cache
directories — is set through the YAML config file, never through the
environment. See [Configuration](./configuration.md) for the full set of config
keys.

## Supported variables

| Variable | Backend | Required? | Behavior when missing |
| --- | --- | --- | --- |
| `QDRANT_URL` | `qdrant` | **Yes** | SAI exits with a clear error telling you to set it to your cluster gRPC endpoint. |
| `QDRANT_API_KEY` | `qdrant` | No | SAI prints a warning to stderr and proceeds. Qdrant Cloud will then reject the unauthenticated request. |

### `QDRANT_URL`

The gRPC endpoint of your Qdrant Cloud cluster. SAI reads it when connecting the
Qdrant backend; if it is unset, the run fails immediately with:

```
set QDRANT_URL to your cluster gRPC endpoint, e.g. https://<id>.<region>.aws.cloud.qdrant.io:6334
```

Set it to your cluster's gRPC URL (note the `:6334` gRPC port, not the `:6333`
REST port):

```sh
export QDRANT_URL="https://<id>.<region>.aws.cloud.qdrant.io:6334"
```

### `QDRANT_API_KEY`

Your Qdrant Cloud API key. When it is set and non-empty, SAI attaches it to the
client. When it is unset (or empty), SAI continues but prints:

```
warning: QDRANT_API_KEY not set — Qdrant Cloud will reject the request
```

So although the run does not abort, any request against Qdrant Cloud will fail
authentication. In practice, treat it as required for Cloud:

```sh
export QDRANT_API_KEY="<your-qdrant-cloud-api-key>"
```

See [Qdrant Cloud setup](../integrations/qdrant-cloud.md) for where to find the
endpoint and key in the Qdrant console.

## Not supported

These environment variables are **not** read by SAI. Setting them has no effect.

| You might expect | Reality |
| --- | --- |
| An env var for the Ollama URL | Config-only: set `ollama.url` in the YAML (default `http://localhost:11434`). |
| An env var for the Ollama model | Config-only: set `ollama.model` in the YAML (default `mxbai-embed-large`). |
| `OLLAMA_HOST` | Not handled. Use `ollama.url`. |
| `HF_HOME` / `HF_TOKEN` | Not handled. The `ort` embedder downloads from the configured `duckdb.model_repo`; no HuggingFace token is read. |

For offline reuse of the local ONNX (`ort`) model, point the indexer at a
pre-populated cache directory with the config key `duckdb.model_cache` — there
is no environment variable for this.

```yaml
# indexer.yaml — these are config keys, not env vars
ollama:
  url: http://localhost:11434
  model: mxbai-embed-large
duckdb:
  model_cache: .cache/models   # offline ONNX reuse for the ort embedder
```

## Environment vs. the resolution chain

The usual SAI knobs follow the precedence **CLI flag > config value > built-in
default** (resolved in `build_plan`). Environment variables sit **outside** that
chain entirely: they are read directly when the Qdrant backend connects and are
not part of the `Plan` that CLI flags and config merge into. There is no CLI
flag or config key that overrides `QDRANT_URL` or `QDRANT_API_KEY`, and these
two variables do not influence any other resolved setting.

See [Configuration](./configuration.md) for the flag-and-config knobs and
[Qdrant Cloud setup](../integrations/qdrant-cloud.md) for end-to-end Cloud
configuration.
