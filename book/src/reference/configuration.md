# Configuration reference

SAI reads a single YAML file, `indexer.yaml`, to decide what gets chunked, embedded, and stored. It is loaded by default from `indexer.yaml` in the current directory, or from a path you pass with `--config <path>`. Every value here is resolved in `src/config.rs` (`build_plan`), and every key in this page maps to a real field — nothing else is read.

The resolution order for most knobs is:

```
CLI flag  >  indexer.yaml value  >  built-in default
```

The Qdrant **API key** is the exception — it is a secret read only from the `QDRANT_API_KEY` environment variable, never from YAML (see [Environment variables](#environment-variables) below). The Qdrant URL is a normal setting: `qdrant.url` in YAML, overridable by `QDRANT_URL`.

> If no `indexer.yaml` exists at the default path, SAI prints `note: no config at indexer.yaml — using built-in defaults (only hard dirs pruned)` and continues with the defaults below. If you pass an explicit `--config` to a missing file, it is a hard error instead.

For how `honor_noindex_marker` / `honor_noduplicate_marker` interact with in-source `sai-noindexing` / `sai-noduplicate` comments, see [opt-out markers](../guides/opt-out-markers.md). For the CLI flags that override these keys, see the [CLI reference](cli.md).

## Key reference

Type, default, and resolution for every recognized key. "Has CLI flag" means a `--flag` can override it; "config-only" means the YAML key is the only way to set it (the value otherwise comes from a default or auto-detection).

| Key (full path) | Type | Default | Resolution | CLI flag? |
| --- | --- | --- | --- | --- |
| `backend` | string | `qdrant` | CLI > config > default | has `--backend` |
| `embedder` | string | `ort` | CLI > config > default | has `--embedder` |
| `chunker` | string | smart: `ast` for ts/tsx/rs/go when built `--features ast`, else `lines` | CLI > config > smart default | has `--chunker` |
| `collection` | string | `source_code` | CLI > config > default | has `--collection` |
| `model` | string | `ort` → `jinaai/jina-embeddings-v2-base-code`; otherwise `intfloat/multilingual-e5-small` | CLI > config > embedder-aware default | has `--model` |
| `vector_dim` | integer | `ort` → `768`; otherwise `384` | config > embedder-aware default | config-only (runtime-validated) |
| `max_chunk_chars` | integer | model-aware (see below) | config > model-aware default | config-only |
| `prefix_style` | `e5` \| `qwen` \| `none` | auto-detected from model name | config > auto-detect | **config-only (no CLI flag)** |
| `duckdb.path` | string | `.index/code.duckdb` | config > default | config-only |
| `duckdb.model_cache` | string | unset (`None`) | config | config-only |
| `duckdb.model_repo` | string | `ort` → `jinaai/jina-embeddings-v2-base-code`; otherwise `Xenova/multilingual-e5-small` | config > embedder-aware default | config-only |
| `ollama.url` | string | `http://localhost:11434` | config > default | config-only |
| `ollama.model` | string | `mxbai-embed-large` | config > default | config-only |
| `qdrant.url` | string | unset (`None`) | **`QDRANT_URL` env > config** | config or env (see [Environment variables](#environment-variables)) |
| `exclude_dirs` | list of strings | `[]` (merged with hard-pruned dirs) | config (additive) | config-only |
| `include` | list of glob strings | `[]` (inactive = match everything) | config | config-only |
| `exclude` | list of glob strings | `[]` | config | config-only |
| `skip_generated_marker` | bool | `false` when omitted | config | config-only |
| `strip_comments` | bool | `true` | config > default | config-only |
| `honor_noindex_marker` | bool | `true` | config > default | config-only |
| `honor_noduplicate_marker` | bool | `true` | config > default | config-only |
| `similarity.find_similar_min_score` | float | `0.85` | CLI/MCP arg > config > default | has CLI flag / MCP arg |
| `similarity.duplicate_min_score` | float | `0.93` | CLI/MCP arg > config > default | has CLI flag / MCP arg |
| `similarity.duplicate_min_cluster_size` | integer | `2` | CLI/MCP arg > config > default | has CLI flag / MCP arg |
| `similarity.top_k` | integer | `10` | CLI/MCP arg > config > default | has CLI flag / MCP arg |

## Backend and embedder

`backend` selects the vector store: `qdrant` (default) or `duckdb`. Qdrant embeds server-side using its Inference tab; DuckDB embeds locally and stores vectors in a DuckDB file with a VSS/HNSW index.

`embedder` applies only to the DuckDB backend (Qdrant ignores it because it embeds server-side):

- `ort` (default) — local ONNX Runtime; downloads the model from `duckdb.model_repo`.
- `ollama` — remote Ollama HTTP server (see [`ollama`](#ollama)).

DuckDB and the embedders are feature-gated — the binary must be built with `--features ort`, `--features ollama`, or `--features all`.

## Chunker

`chunker` is `lines` or `ast`. The default is **smart**: when no chunker is set on the CLI or in config, SAI selects `ast` if the binary was built with `--features ast` **and** any requested extension is in the AST-preferred set (`ts`, `tsx`, `rs`, `go`); otherwise it falls back to `lines`. The chunker still dispatches per file, so a mixed walk AST-parses files with a grammar and line-chunks the rest.

```yaml
chunker: ast   # or: lines — CLI --chunker always wins
```

## Model, vector dimension, and chunk size

`model` is the embedding model label (and, for Qdrant, must match the cluster's Inference tab). Its default depends on the resolved embedder:

| Embedder path | Default `model` | Default `vector_dim` |
| --- | --- | --- |
| `ort` | `jinaai/jina-embeddings-v2-base-code` | `768` |
| any other (Qdrant, Ollama) | `intfloat/multilingual-e5-small` | `384` |

`vector_dim` **must** equal the embedder model's output dimensionality. It is **config-only** and **runtime-validated** — a mismatch is a clear error. If you change `model` to one with a different dimensionality, set `vector_dim` to match (e.g. `mxbai-embed-large` = `1024`, `nomic-embed-text` = `768`).

`max_chunk_chars` is the character cap both chunkers honor (a ~4-chars/token approximation of the model's window). When unset, the default is model-aware (`default_cap` in `src/config.rs`):

| Condition (checked in order) | Cap (chars) |
| --- | --- |
| `model` contains `qwen` | `32000` |
| `model` contains `e5` | `1400` |
| `model` contains `jina` | `32000` |
| backend `duckdb` + embedder `ollama` | `32000` |
| otherwise | `1400` |

## prefix_style (config-only)

`prefix_style` controls the embedding prefix policy and has **no CLI flag**. Accepted values are `e5`, `qwen`, and `none`. When omitted, it is **auto-detected from the model name**: a name containing `e5` → `e5`, containing `qwen` → `qwen`, otherwise → `none`. It is applied by both local embedders and the Qdrant document path.

```yaml
# prefix_style: e5   # e5 | qwen | none — omit to auto-detect from model
```

## duckdb

Used only when `backend: duckdb`; ignored by Qdrant.

```yaml
duckdb:
  path: .index/code.duckdb        # DuckDB file; created on first index
  # model_repo: jinaai/jina-embeddings-v2-base-code   # ort: HF repo for model.onnx + tokenizer.json
  # model_cache: .model_cache      # ort: offline ONNX/HF cache dir (unset by default)
```

`duckdb.model_repo` defaults to `jinaai/jina-embeddings-v2-base-code` for the `ort` embedder, and to `Xenova/multilingual-e5-small` otherwise. `duckdb.model_cache` is unset by default.

## ollama

Used only when `embedder: ollama`.

```yaml
ollama:
  # url: http://localhost:11434    # default
  # model: mxbai-embed-large       # default (1024-d → set vector_dim: 1024)
```

`ollama.model` defaults to `mxbai-embed-large` (1024-d). Set `vector_dim` to match the model you pull.

## File selection: exclude_dirs, include, exclude

```yaml
exclude_dirs:        # directory NAMES pruned during the walk (never descended into)
  - __tests__

include: []          # allow-list globs; empty = consider everything

exclude:             # glob patterns matched per file path; '**' spans directories
  - "**/*.test.ts"
  - "**/*.d.ts"
```

- `exclude_dirs` is **additive** — its entries are merged on top of the always-pruned dirs (see below). Listing a hard-pruned dir here is harmless.
- `include` is an **allow-list of globs**. When non-empty it becomes active: a file must match at least one include glob to be considered. When empty/omitted, everything is considered.
- `exclude` globs are matched against each file path. **Exclude always wins over include** — the gate is `(!include_active || include matches) && !exclude matches`.

### Selection order (per file)

For each file under the root, after directory pruning and the extension filter:

1. **`include`** — if active, the file must match an include glob, else it is skipped.
2. **`exclude`** — if any exclude glob matches, the file is skipped (exclude wins over include).
3. **`skip_generated_marker`**, then **`strip_comments`** are applied to the surviving content.

## skip_generated_marker (defaults to false)

`skip_generated_marker` is a **plain bool** — unlike most toggles it has no "absent = true" fallback. When the key is **omitted, it defaults to `false`** (generated-marker scanning is off). The shipped `indexer.yaml` sets it to `true` explicitly:

```yaml
skip_generated_marker: true   # scan file head for @generated / "DO NOT EDIT." markers
```

When enabled, it skips files whose head carries an autogenerated marker (e.g. `@generated`, Go's `// Code generated ... DO NOT EDIT.`) regardless of filename.

## strip_comments

`strip_comments` defaults to **`true`** when omitted. It removes `//` and `/* */` comments from C-family source before embedding so only code reaches the backend; string/template literals are preserved and line numbers stay accurate.

```yaml
strip_comments: true
```

## honor_noindex_marker / honor_noduplicate_marker

Both default to **`true`** when omitted. They are **not present in the shipped `indexer.yaml`** — they take effect via the defaults. See [opt-out markers](../guides/opt-out-markers.md) for the in-source `sai-noindexing` / `sai-noduplicate` behavior.

```yaml
honor_noindex_marker: true        # respect sai-noindexing comments (skip chunk entirely)
honor_noduplicate_marker: true    # respect sai-noduplicate comments (index, but no clustering)
```

## similarity

Thresholds for the `sai_find_similar` and `sai_find_duplicates` MCP tools and the `similar` / `duplicates` CLI subcommands. Per-knob resolution is **CLI flag / MCP tool arg > config value > built-in default**. All fields are optional. These cutoffs are model-specific — tune them per embedder.

```yaml
similarity:
  find_similar_min_score: 0.85       # default 0.85 — drop neighbors below this cosine
  duplicate_min_score: 0.93          # default 0.93 — edge cutoff between two chunks
  duplicate_min_cluster_size: 2      # default 2    — smallest cluster to report
  top_k: 10                          # default 10   — nearest-neighbor fan-out per chunk
```

## Always-pruned directories

Independent of config, these directories are **always** pruned during the walk (the `HARD_PRUNE_DIRS` set in `src/config.rs`):

```
node_modules   .git   dist   build   target   .next   coverage   .turbo
```

`exclude_dirs` entries are added on top of this set; you cannot un-prune a hard-pruned dir via config.

## AST-preferred extensions

The smart chunker default selects `ast` (when the `ast` feature is compiled in and no chunker was set explicitly) for these extensions (`AST_PREFERRED_EXTS`):

```
ts   tsx   rs   go
```

Any other extension falls back to the `lines` chunker even when `ast` support is present.

## Environment variables

The Qdrant **API key is a secret** and is read **only** from the environment — it never belongs in YAML. The cluster **URL** is not secret: set it in YAML as `qdrant.url`, or via the `QDRANT_URL` environment variable (which takes precedence over the YAML value).

| Variable | Purpose | YAML equivalent |
| --- | --- | --- |
| `QDRANT_API_KEY` | Qdrant API key (**secret** — keep it out of version control) | none (env-only by design) |
| `QDRANT_URL` | Qdrant cluster gRPC URL; overrides `qdrant.url` if set | `qdrant.url` |

If your `indexer.yaml` contains `qdrant.url` it is safe to commit (the URL is not a secret); the key never lives there.

## Footgun: unknown keys are silently ignored

The `Config` struct is deserialized with all fields optional and `#[serde(default)]`, so a partial file still parses — **and unrecognized or misspelled YAML keys are silently ignored**. A typo like `skip_generated_marekr: true` or `exclude_dir:` will not raise an error; the intended setting simply never takes effect and the default is used instead. Double-check key spelling and nesting (e.g. `duckdb.path`, `similarity.top_k`) against the [Key reference](#key-reference) table above.

## See also

- [CLI reference](cli.md) — flags that override these keys
- [Opt-out markers](../guides/opt-out-markers.md) — `sai-noindexing` / `sai-noduplicate` detail
