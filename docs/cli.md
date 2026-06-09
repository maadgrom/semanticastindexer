# CLI usage

Build once (see [installation](install.md)), then run the binary from the **target project's
repo root** so the stored paths are project-relative (or point `--root` at the project's source
dir). The shipped `indexer.yaml` is a sane offline default (`backend: duckdb`, `embedder: ort`,
`chunker: lines`) — copy it next to the binary / into the project, or pass
`--config /path/to/indexer.yaml`. With a full build (`--features all`) you can index any project
fully offline out of the box (ort + DuckDB).

```bash
BIN="$(pwd)/target/release/semanticastindexer"   # absolute path to the built binary

# Move into the project you want to index (so payload paths are project-relative).
cd /path/to/your/project

# 1. See exactly what WOULD be indexed/skipped — no network, no upload, no quota used.
"$BIN" --root src --dry-run

# 2. Index the TypeScript tree (creates the collection/table if missing). Each chunk's
#    `language` label is derived per-file from its extension (`.ts` → "ts", `.tsx` → "tsx").
"$BIN" --root src --ext ts,tsx --collection source_code

# 3. Index Go later, into the same collection.
"$BIN" --root path/to/go --ext go --collection source_code

# 4. Search (read-only — does not upload the codebase).
"$BIN" --query-only --collection source_code \
    --query "where do we create the qdrant collection"

# 5. Flush — delete the whole collection.
"$BIN" flush

# 6. Sync — re-index only changed files (for git hooks). For each changed file:
#    delete its existing points, then upload the current content fresh; files that
#    were deleted or are now excluded are just removed from the collection.
"$BIN" sync --since HEAD~1                 # diff HEAD~1..HEAD (e.g. post-commit / post-merge)
"$BIN" sync --staged                       # staged changes (e.g. pre-commit)
"$BIN" sync --file src/a.ts --file src/b.ts   # explicit file list
```

## CLI flags

| Flag | Default | Description |
| ---- | ------- | ----------- |
| `--root <dir>` | `src` | Directory to walk. |
| `--ext <list>` | `ts,tsx` | Comma-separated extensions (no dots). Each chunk's `language` payload label is derived per-file from its extension (`.ts` → "ts", `.tsx` → "tsx"). |
| `--backend <s>` | config / `qdrant` | Vector backend: `qdrant` or `duckdb` (overrides config). |
| `--embedder <s>` | config / `ort` | DuckDB embedder: `ort` or `ollama` (overrides config; ignored by qdrant). |
| `--chunker <s>` | config / `lines` | Chunker: `lines` or `ast` (overrides config; `ast` needs `--features ast`). |
| `--config <path>` | `indexer.yaml` | YAML exclusion config. |
| `--collection <s>` | config / `source_code` | Target collection (overrides config). |
| `--model <s>` | config (ort → jinaai/jina-embeddings-v2-base-code, else e5-small) | Inference model (overrides config). |
| `--query <s>` | — | Run a semantic search after indexing. |
| `--query-only` | `false` | Skip indexing; only search. |
| `--recreate` | `false` | Drop & recreate the collection before indexing. |
| `--dry-run` | `false` | Report inclusions/exclusions; no network. |
| `--limit <n>` | `5` | Number of search results to print. |

CLI flags take precedence over `indexer.yaml`. Always **dry-run first** before a real index to
confirm the exclusion set.

## Git hook example

`.git/hooks/post-commit` (or `post-merge`) — keep the index in lockstep with commits:

```sh
#!/bin/sh
QDRANT_URL="https://<id>.<region>.aws.cloud.qdrant.io:6334" \
QDRANT_API_KEY="$QDRANT_API_KEY" \
/path/to/semanticastindexer/target/release/semanticastindexer sync --since HEAD~1 --ext ts,tsx >/dev/null 2>&1 &
```

`sync` respects the same `--ext` and `indexer.yaml` filters as a full index, so a changed
test/shadcn/generated file is removed (not re-added). Run hooks from the repo root so the
paths git reports match the stored payload paths.

> ⚠️ **One-time re-index for existing Qdrant collections.** Point IDs are a stable
> `XxHash64(seed=0)` of `path + start_line` (previously the unspecified `DefaultHasher`).
> Existing collections must be flushed/recreated once so old points don't linger:
> run `./target/release/semanticastindexer flush` or index with `--recreate`.

## Similarity search (`similar` / `duplicates`)

The same similarity search exposed as MCP tools is also runnable from the shell — no MCP
client needed. Build with `--features all` (the recommended configuration) so all backends
and embedders are available. The index is opened **read-only** (so a search can run while an
index is open elsewhere). The top-level `--backend` / `--embedder` / `--collection` / `--config`
flags still apply (before or after the subcommand) and pick up the YAML defaults.

```bash
# Codebase-wide near-duplicate clusters (stored vectors only — no re-embed).
./target/release/semanticastindexer duplicates                                   # config/built-in thresholds
./target/release/semanticastindexer duplicates --min-score 0.85 --top-k 10 \
    --min-cluster-size 2 --max-clusters 20 \
    --path-glob "src/utils/**"

# Nearest neighbours of a code snippet (embedded as a PASSAGE — code-vs-code space).
./target/release/semanticastindexer similar --code "function formatDuration(s) { return s }" --limit 8

# Nearest neighbours of an existing indexed chunk (stored vector, self-excluded).
./target/release/semanticastindexer similar --path src/utils/transcriptParser.ts --line 103 --min-score 0.0
```

- **`duplicates`** — for each chunk, takes its `top-k` nearest neighbours, keeps edges with
  similarity `>= min-score`, and unions them into clusters (union-find). Prints clusters
  largest-first:

  ```text
  cluster (size N, sim min..max):
    path:start-end  symbol
    ...
  ```

  Each knob resolves **CLI flag > config (`similarity.*`) > built-in default**
  (`duplicate_min_score` 0.93, `duplicate_min_cluster_size` 2, `top_k` 10; `--max-clusters`
  defaults to 50). `--path-glob` restricts the scan.

- **`similar`** — prints `score  path:start-end  symbol` for the nearest neighbours. Provide
  **exactly one** of `--code` (embedded as a passage) **or** `--path` **and** `--line` (the
  stored vector is reused and the chunk itself is excluded) — anything else is a clear error.
  `--min-score` resolves **flag > config `find_similar_min_score` (0.85) > default**; pass
  `--min-score 0` to see the raw score distribution.

> The `similar --code` path needs a **local** embedder (the DuckDB backend) — Qdrant embeds
> server-side, so `--code` against `--backend qdrant` returns a clear error. `duplicates` and
> `similar --path/--line` work on either backend (stored vectors, no re-embed).

The same logic backs the MCP `find_similar` / `find_duplicates` tools — the union-find
clustering and the find_similar resolution live in one shared module (`src/search.rs`), used
by both the CLI handlers and the MCP server, so there is no duplicated algorithm.
