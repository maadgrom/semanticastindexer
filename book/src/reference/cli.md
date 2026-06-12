# CLI reference

The `semanticastindexer` (SAI) binary is a single CLI with an optional subcommand. With **no subcommand** it runs a full index of `--root`; subcommands cover storage maintenance (`flush`), incremental re-indexing (`sync`), similarity search (`similar`, `duplicates`), and the MCP server (`mcp`).

Run the binary from the **target project's repo root** so stored payload paths stay project-relative (or point `--root` at the project's source dir). Connection settings for the Qdrant backend are read from the environment, never hard-coded — see [environment variables](../reference/configuration.md#environment-variables) (`QDRANT_URL`, `QDRANT_API_KEY`). Persistent configuration lives in `indexer.yaml`; see the [configuration reference](../reference/configuration.md) for every key.

## Synopsis

```text
semanticastindexer [GLOBAL FLAGS] [INDEX FLAGS]            # default: full index of --root
semanticastindexer [GLOBAL FLAGS] flush
semanticastindexer [GLOBAL FLAGS] sync       [SYNC FLAGS]
semanticastindexer [GLOBAL FLAGS] similar    [SIMILAR FLAGS]
semanticastindexer [GLOBAL FLAGS] duplicates [DUPLICATES FLAGS]
semanticastindexer [GLOBAL FLAGS] mcp        [MCP FLAGS]
```

## Resolution order

Every value resolves in this order:

**CLI flag > `indexer.yaml` config value > built-in default.**

A CLI flag always overrides the config file, which always overrides the compiled-in default. This applies to the global flags, the index-only flags, and the per-subcommand similarity thresholds documented below.

## Global flags (before or after the subcommand)

These flags are declared `global = true`, so they are accepted in any position — before **or** after the subcommand name.

| Flag | Type | Default | Behavior |
| ---- | ---- | ------- | -------- |
| `--backend <s>` | string | config, else `qdrant` | Vector backend: `qdrant` or `duckdb`. Overrides config. (The `mcp` subcommand applies its own default of `duckdb` when this is unset — see [MCP defaults](#mcp).) |
| `--embedder <s>` | string | config, else `ort` | DuckDB embedder: `ort` or `ollama`. Overrides config; ignored by the qdrant backend. (The `mcp` subcommand defaults this to `ollama` when unset.) |
| `--config <path>` | string | `indexer.yaml` | Path to the YAML exclusion/settings config. If the default file is absent, built-in defaults are used (a note is printed to stderr). If an **explicit** non-default path is missing, the run errors. |
| `--collection <s>` | string | config, else `source_code` | Target collection / table name. Overrides config. |
| `--silent` | flag | `false` | Suppress timing, progress, dirty warnings, and non-essential notes. Ideal for hooks/CI. See [timing & output](#timing-and-output). |

> Note: `--backend`, `--embedder`, `--config`, `--collection`, and `--silent` are global. The index-only flags below (`--root`, `--ext`, `--chunker`, `--model`, `--query`, `--query-only`, `--recreate`, `--dry-run`, `--limit`) are **not** global — they belong to the default (no-subcommand) index action.

## Default action — full index (no subcommand)

With no subcommand, SAI walks `--root`, chunks each matching file, embeds the chunks, and upserts them into the collection. The flags below are specific to this default action.

| Flag | Type | Default | Behavior |
| ---- | ---- | ------- | -------- |
| `--root <dir>` | string | `src` | Directory to walk for source files. |
| `--ext <list>` | comma list | `ts,tsx` | Comma-separated extensions (no dots; leading dots are stripped). Each chunk's `language` payload label is derived **per file** from its extension (`.ts` → `ts`, `.tsx` → `tsx`). |
| `--chunker <s>` | string | config, else auto | `lines` or `ast` (tree-sitter). When omitted, `ast` is auto-selected **only if** the binary was built with the `ast` feature **and** any requested extension has a grammar (`ts`, `tsx`, `rs`, `go`); otherwise `lines`. An explicit `--chunker` always wins. `ast` requires the `ast` feature or the run errors early. |
| `--model <s>` | string | config, else model-default | Inference/embedding model. The default depends on the embedder: `ort` → `jinaai/jina-embeddings-v2-base-code`; otherwise `intfloat/multilingual-e5-small`. Overrides config. |
| `--query <s>` | string | — | Run a semantic query **after** indexing. Prints the top `--limit` hits as `score  path:start-end`. |
| `--query-only` | flag | `false` | Skip indexing; only run `--query` against the existing collection. (Opens the backend read-write but never re-indexes.) |
| `--recreate` | flag | `false` | Drop and recreate the collection before indexing. |
| `--dry-run` | flag | `false` | Walk and report what would be indexed/skipped. No network, no upload. Exits after reporting. |
| `--limit <n>` | u64 | `5` | Number of nearest results to print for a query. **Note:** this top-level default is `5`; the `similar` subcommand has its own `--limit` default of `8` (see [two distinct `--limit` defaults](#two-distinct---limit-defaults)). |

Examples:

```bash
BIN="$(pwd)/target/release/semanticastindexer"
cd /path/to/your/project

# See exactly what would be indexed/skipped — no network, no upload.
"$BIN" --root src --dry-run

# Index the TypeScript tree into a named collection.
"$BIN" --root src --ext ts,tsx --collection source_code

# Index Go later, into the same collection.
"$BIN" --root path/to/go --ext go --collection source_code

# Search only (read-only — does not upload the codebase).
"$BIN" --query-only --collection source_code \
    --query "where do we create the qdrant collection" --limit 10

# Drop & rebuild the collection before indexing.
"$BIN" --root src --ext ts,tsx --recreate
```

### Two distinct `--limit` defaults

`--limit` exists in two places with **different** defaults — do not confuse them:

| Context | Default | Meaning |
| ------- | ------- | ------- |
| Top-level `--limit` (default index / `--query`) | `5` | Number of query hits printed. |
| `similar --limit` | `8` | Max nearest-neighbour results from `similar`. |

### DuckDB dimension-mismatch prompt

On the indexing path (not `--query-only`), if opening the DuckDB backend fails because the existing index file was built with a different embedding model (a vector-dimension mismatch), SAI offers to delete the file and re-index from scratch. The prompt **defaults to NO**, and on a non-interactive stdin (CI, git hooks, the MCP stdio server) it **auto-declines immediately** and propagates the original error — it is never destructive in automation. Declining (or piping non-TTY input) leaves the index untouched. `--query-only` never re-indexes, so it surfaces the error without offering to delete anything.

## `flush`

Delete the entire collection from the vector storage.

```bash
semanticastindexer flush
```

`flush` takes no subcommand-local flags (only the global flags apply). Useful for the one-time re-index when point-ID hashing changed, or to fully reset a collection.

## `sync`

Re-index only changed files — intended for git hooks (post-commit, post-merge, pre-commit). For each changed file: delete its existing points, then upload the current content fresh. Files that were deleted or are now excluded are removed from the collection. `sync` honors the same `--ext` and `indexer.yaml` filters as a full index.

| Flag | Type | Default | Behavior |
| ---- | ---- | ------- | -------- |
| `--since <rev>` | string | `HEAD~1` | Git revision to diff against; the changed set is `<since>..HEAD` (via `git diff --name-only <since>`). |
| `--staged` | flag | `false` | Use staged changes (`git diff --name-only --cached`) instead of `--since`. |
| `--file <path>` | repeatable string | — | Explicit changed file path(s); repeat for multiple. When given, this **overrides git detection** entirely (neither `--since` nor `--staged` is consulted). Existing files are re-indexed; missing files are deleted from the collection. |

Examples:

```bash
# Diff HEAD~1..HEAD (e.g. post-commit / post-merge).
semanticastindexer sync --since HEAD~1

# Staged changes (e.g. pre-commit).
semanticastindexer sync --staged

# Explicit file list (overrides git).
semanticastindexer sync --file src/a.ts --file src/b.ts
```

If git detection (or the explicit list) yields no changed files, `sync` prints `sync: no changed files` and exits. Run hooks from the repo root so the paths git reports match the stored payload paths. See the [keeping the index in sync guide](../guides/keeping-in-sync.md) for full hook examples.

## `similar`

Print the nearest neighbours of either a code snippet or an existing indexed chunk, as `score  path:start-end  symbol`. Requires a vector backend feature (`duckdb` or `qdrant`).

| Flag | Type | Default | Behavior |
| ---- | ---- | ------- | -------- |
| `--code <s>` | string | — | A code snippet to find neighbours of. Embedded as a **passage** (code-vs-code space). Mutually exclusive with `--path`/`--line`. |
| `--path <s>` | string | — | Path of an existing indexed chunk. Use **with** `--line`; reuses the stored vector and excludes the chunk itself. |
| `--line <n>` | usize | — | 1-based start line of an existing indexed chunk. Use **with** `--path`. |
| `--limit <n>` | u64 | `8` | Max results. (Distinct from the top-level `--limit` default of `5`.) |
| `--min-score <f>` | f32 | config `similarity.find_similar_min_score`, else `0.85` | Drop results scoring below this cosine similarity. Pass `--min-score 0` to see the raw score distribution. |

### Exactly-one-of validation

`similar` requires **exactly one** target:

- **either** `--code`
- **or** both `--path` **and** `--line`

Any other combination is a clear error:

| Input | Result |
| ----- | ------ |
| `--code` + (`--path` or `--line`) | Error: provide EITHER `--code` OR (`--path` and `--line`), not both. |
| `--path` without `--line` (or vice versa) | Error: `--path` and `--line` must be given together. |
| none of them | Error: provide either `--code` or both `--path` and `--line`. |

Examples:

```bash
# Nearest neighbours of a snippet (embedded as a passage — code-vs-code).
semanticastindexer similar --code "function formatDuration(s) { return s }" --limit 8

# Nearest neighbours of an existing indexed chunk (stored vector, self-excluded).
semanticastindexer similar --path src/utils/transcriptParser.ts --line 103 --min-score 0.0
```

> The `similar --code` path needs a **local** embedder (the DuckDB backend). Qdrant embeds server-side, so `--code` against `--backend qdrant` returns a clear error. `similar --path/--line` works on either backend (it reuses the stored vector, no re-embed).

## `duplicates`

Codebase-wide near-duplicate clusters. For each chunk, takes its nearest neighbours, keeps edges with similarity `>= min-score`, and unions them into clusters (union-find). Prints clusters largest-first. Uses stored vectors only (no re-embed). Requires a vector backend feature (`duckdb` or `qdrant`).

| Flag | Type | Default | Behavior |
| ---- | ---- | ------- | -------- |
| `--min-score <f>` | f32 | config `similarity.duplicate_min_score`, else `0.93` | Minimum cosine similarity for an edge to count as a near-duplicate. |
| `--min-cluster-size <n>` | usize | config `similarity.duplicate_min_cluster_size`, else `2` | Minimum cluster size to report (clamped to at least 1). |
| `--top-k <n>` | u64 | config `similarity.top_k`, else `10` | Nearest-neighbour fan-out per chunk. |
| `--max-clusters <n>` | usize | `50` | Max clusters to print (largest first). **This is a flag-or-`50` default — it is NOT read from config.** |
| `--path-glob <s>` | string | — | Restrict the scan to paths matching this glob (e.g. `"src/utils/**"`). |

Note the resolution asymmetry: `--min-score`, `--min-cluster-size`, and `--top-k` each resolve **flag > config (`similarity.*`) > built-in default**, while `--max-clusters` resolves **flag > built-in default `50`** with no config key.

Output format:

```text
N near-duplicate cluster(s) (min_score 0.93, min_cluster_size 2, top_k 10):
cluster (size 3, sim 0.9412..0.9871):
  src/a.ts:10-40  formatDuration
  src/b.ts:5-35   formatTime
  ...
```

When no clusters meet the thresholds, SAI prints a single `no near-duplicate clusters (...)` line.

Examples:

```bash
# Config / built-in thresholds.
semanticastindexer duplicates

# Fully specified.
semanticastindexer duplicates --min-score 0.85 --top-k 10 \
    --min-cluster-size 2 --max-clusters 20 \
    --path-glob "src/utils/**"
```

### Dirty-index prompt

Before scanning, `duplicates` checks whether the index contains **dirty-stamped** chunks (chunks recorded from an uncommitted working tree). If so:

- On an **interactive TTY**, it warns and asks to proceed; the prompt **defaults to NO**, and declining aborts the scan (the results may reflect uncommitted work).
- On a **non-interactive stdin** (CI, git hooks, MCP), it prints the warning to stderr and proceeds without prompting — it never blocks.
- With `--silent`, the dirty check is skipped entirely.

Like the dimension-mismatch prompt, this never triggers a destructive or blocking action in automation.

## `mcp`

Run the MCP server (semantic code search for AI agents) over stdio. Requires the `mcp` feature. When `--backend`/`--embedder` are unset, the MCP server applies its own defaults of `duckdb` + `ollama` (the offline, no-quota path); explicit CLI flags and config values still take precedence via the normal merge. The shipped tool names are `sai_`-prefixed: `sai_search_code`, `sai_find_similar`, `sai_find_duplicates`, `sai_index_status`, `sai_prepare_mcp_setup`, and (gated) `sai_refresh`.

| Flag | Type | Default | Behavior |
| ---- | ---- | ------- | -------- |
| `--allow-write` | flag | `false` | Open the index **writable** and register the `sai_refresh` tool. Without it the server is read-only and `sai_refresh` returns a clear "restart with `--allow-write`" error. |
| `--allow-setup` | flag | `false` | Permit the `sai_prepare_mcp_setup` tool to **actually execute** the mcp-setup script (which can trigger long builds and file modifications). Without it, that tool does not execute the setup. Use with caution. |

By default the MCP server is **read-only**: it never modifies the index or the filesystem. `--allow-write` gates the only write tool (`sai_refresh`); `--allow-setup` gates execution of `sai_prepare_mcp_setup`.

```bash
# Read-only MCP server (default duckdb + ollama).
semanticastindexer mcp

# Enable the sai_refresh write tool.
semanticastindexer mcp --allow-write

# Also allow sai_prepare_mcp_setup to run the setup script.
semanticastindexer mcp --allow-write --allow-setup
```

See the [MCP server and tools reference](../reference/mcp-server.md) for full tool details, and the [glossary](../concepts/glossary.md) for terminology.

## `update`

Self-update: download and install the latest GitHub release over the current binary by
running the official release installer. Config-independent — works from any directory,
needs no `indexer.yaml`, and is always compiled in (no feature gate).

```bash
semanticastindexer update
```

On macOS and Linux the installer replaces the binary in place; the new version takes
effect on the next invocation (restart any running MCP servers to pick it up). On
Windows a running executable cannot overwrite itself, so `update` prints the exact
PowerShell one-liner to run instead.

## Feature gating

SAI is built with Cargo features. Subcommands and chunkers are compiled in only when the matching feature is present:

| Capability | Required feature(s) |
| ---------- | ------------------- |
| `mcp` subcommand | `mcp` |
| `similar` / `duplicates` subcommands | `duckdb` **or** `qdrant` |
| `ast` chunker (`--chunker ast` / auto-AST) | `ast` |

The `--features all` build enables every backend, embedder, and chunker so any project can be indexed fully offline (ort + DuckDB) out of the box. Requesting `--chunker ast` (or auto-selecting it) without the `ast` feature fails early with a clear, actionable error before any work begins.

## Timing and output

Every top-level command is wrapped in a timing helper (`run_timed`). On completion it prints a single line to **stderr**:

```text
done at <git-sha>[, dirty] in <seconds>s
```

(`(dry-run)` is appended for a dry-run.) This timing/summary line — along with progress lines, dirty warnings, and the "no config" note — is suppressed by the global `--silent` flag, which is recommended for hooks and CI. Indexing progress (`embedded N/M chunks`, per-file lines) is also written to stderr; the actual results (`indexed ...`, query hits, similarity output) go to **stdout**.

## See also

- [Configuration reference](../reference/configuration.md) — every `indexer.yaml` key.
- [Environment variables](../reference/configuration.md#environment-variables) — `QDRANT_URL`, `QDRANT_API_KEY`.
- [Glossary](../concepts/glossary.md) — backend, embedder, chunker, and marker terms.
