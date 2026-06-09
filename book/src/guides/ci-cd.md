# CI/CD integration

SAI is a single static binary that runs the same in CI as it does on your laptop:
walk a tree, embed chunks, push them to a vector backend, and report duplicates.
This guide shows how to wire it into a pipeline — keeping the index in sync on every
push, caching the local ONNX model, running Ollama as a service, passing Qdrant
credentials as secrets, and failing the build when new near-duplicates appear.

> **The repo does not ship an indexing workflow.** The only workflow checked into this
> project is `.github/workflows/release.yml`, which builds and publishes release
> binaries — it does **not** index your code. The examples below are templates for
> *your* repository, not files that already exist here.

## CI is non-interactive by design

Every yes/no prompt in SAI auto-declines when there is no terminal attached. `stdin`
is not a TTY in CI, so each prompt returns "No" immediately and the run continues —
it never blocks waiting for input and never takes a destructive action by default.

Two prompts behave this way:

- **Dimension-mismatch on the DuckDB index** — if an existing local index was built
  with a different embedding model, an interactive run offers to delete and rebuild it
  (defaulting to *No*). In CI the prompt is skipped and the underlying error surfaces
  instead, so a stale index can never be silently wiped.
- **Dirty-tree warning on `duplicates`** — when the index contains chunks stamped from
  an uncommitted working tree, an interactive run asks whether to proceed. In CI the
  warning is printed to stderr and the command proceeds.

Add `--silent` to suppress timing, progress, and dirty warnings entirely — it is built
for hooks and CI and keeps logs clean:

```bash
semanticastindexer sync --silent
```

## Passing Qdrant credentials as secrets

When you target the Qdrant backend, credentials are read **only** from the environment
— never from a flag or config file. Set them once and never commit them:

| Variable | Value |
| --- | --- |
| `QDRANT_URL` | e.g. `https://<cluster-id>.<region>.aws.cloud.qdrant.io:6334` |
| `QDRANT_API_KEY` | the cluster API key |

In GitHub Actions, store both as repository secrets and expose them through `env`:

```yaml
env:
  QDRANT_URL: ${{ secrets.QDRANT_URL }}
  QDRANT_API_KEY: ${{ secrets.QDRANT_API_KEY }}
```

If a key is ever exposed, rotate it in the cluster's *API Keys* tab. See
[reference/environment.md](../reference/environment.md) for the full list of variables
SAI reads.

## Sync on every push (Qdrant backend)

The `sync` subcommand re-indexes only the files that changed in a revision range, so it
is cheap to run on every push. By default it diffs `HEAD~1..HEAD`; pass `--since` for a
different base. This workflow keeps a Qdrant collection current:

```yaml
name: Index code
on:
  push:
    branches: [main]

jobs:
  index:
    runs-on: ubuntu-latest
    env:
      QDRANT_URL: ${{ secrets.QDRANT_URL }}
      QDRANT_API_KEY: ${{ secrets.QDRANT_API_KEY }}
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 2 # need HEAD~1 for the default --since

      - name: Install SAI
        run: curl -fsSL https://maadgrom.github.io/semanticastindexer/install.sh | bash

      - name: Sync changed files
        run: semanticastindexer sync --backend qdrant --since HEAD~1 --ext ts,tsx --silent
```

`sync` deletes each changed file's old points and uploads the current content fresh;
files that are gone (deleted or now excluded) are simply removed from the collection.
For the full mechanics — staged diffs (`--staged`), explicit `--file` lists, and how it
pairs with git hooks — see [keeping in sync](./keeping-in-sync.md).

## Caching the Hugging Face ONNX model (ort embedder)

The default `ort` embedder runs ONNX Runtime on-device with no server and no API keys.
On the **first** run it pulls the model from Hugging Face — the code-trained
`jinaai/jina-embeddings-v2-base-code` (or `intfloat/multilingual-e5-small` for the
text default). That download repeats on every fresh runner unless you cache it.

The model is fetched into the Hugging Face hub cache (`~/.cache/huggingface`), so cache
that directory between runs:

```yaml
jobs:
  index:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - name: Install SAI
        run: curl -fsSL https://maadgrom.github.io/semanticastindexer/install.sh | bash

      - name: Cache Hugging Face model
        uses: actions/cache@v4
        with:
          path: ~/.cache/huggingface
          # bump the key when you change `model` so a new model is re-downloaded
          key: hf-jina-code-v2

      - name: Index with the local ort embedder
        run: semanticastindexer --backend duckdb --embedder ort --root src --ext ts,tsx --silent
```

This keeps the DuckDB index entirely local to the runner — no Qdrant credentials
needed. If you also persist the local index file across runs, the dimension-mismatch
prompt becomes relevant: it auto-declines in CI, so a model change surfaces an error
rather than silently rebuilding (see above).

## Running Ollama as a service in CI

The `ollama` embedder talks to an embedding server over HTTP, which suits CI where an
embedding service often already runs. Start `ollama serve`, pull an embedding model,
then point SAI at it. Configure `ollama.url` and `ollama.model` in `indexer.yaml`
(see [reference/configuration.md](../reference/configuration.md)):

```yaml
jobs:
  index:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - name: Install SAI
        run: curl -fsSL https://maadgrom.github.io/semanticastindexer/install.sh | bash

      - name: Start Ollama and pull an embedding model
        run: |
          curl -fsSL https://ollama.com/install.sh | sh
          ollama serve &
          # wait for the server, then pull the model
          until curl -sf http://localhost:11434/api/tags >/dev/null; do sleep 1; done
          ollama pull mxbai-embed-large

      - name: Index via the Ollama embedder
        run: semanticastindexer --backend duckdb --embedder ollama --root src --ext ts,tsx --silent
```

See the [Ollama integration guide](../integrations/ollama.md) for matching the model to
the configured `vector_dim`.

## Fail the build when new duplicates appear

The `duplicates` subcommand scans stored vectors for near-duplicate clusters and prints
them human-readably. When nothing crosses the threshold it prints a line starting with
`no near-duplicate clusters`; when something does it prints
`N near-duplicate cluster(s):` followed by each cluster. You can gate a PR on that
output.

The simplest gate fails when **any** cluster is found above your chosen threshold:

```bash
#!/usr/bin/env bash
set -euo pipefail

# Index the working tree first (local ort backend, no creds needed).
semanticastindexer --backend duckdb --embedder ort --root src --ext ts,tsx --silent

out=$(semanticastindexer duplicates --backend duckdb --min-score 0.95)
echo "$out"

if echo "$out" | grep -q '^no near-duplicate clusters'; then
  echo "No duplicates above threshold."
else
  echo "::error::near-duplicate clusters detected"
  exit 1
fi
```

The threshold knobs map directly to flags: `--min-score` (minimum cosine similarity for
an edge), `--min-cluster-size`, `--top-k`, and `--path-glob` to scope the scan. Each
resolves CLI flag > config `similarity.*` > built-in default. Tune them with
[tuning similarity](./tuning-similarity.md) and the
[search and duplicates](./search-and-duplicates.md) guide.

To catch only **newly introduced** duplicates rather than failing on a pre-existing
backlog, scope the scan to changed files with `--path-glob`, or compare the cluster
count of the base branch against the head branch and fail only when it grows.

## Read-only by default

The CLI read commands — `duplicates`, `similar`, and `--query-only` — open the index
read-only, and the MCP server is read-only unless started with `--allow-write`. A CI
job that only searches or reports duplicates can never mutate the collection. See the
[CLI reference](../reference/cli.md) for the full flag set.

## See also

- [Keeping in sync](./keeping-in-sync.md) — `sync`, git hooks, and revision ranges.
- [Environment reference](../reference/environment.md) — `QDRANT_URL`, `QDRANT_API_KEY`, and friends.
- [Troubleshooting](../operations/troubleshooting.md) — what to do when a CI run errors out.
