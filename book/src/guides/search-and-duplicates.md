# Search and duplicates

Once a project is indexed, semanticastindexer (SAI) gives you three distinct
retrieval capabilities over the stored vectors. Each answers a different
question, and each is available **both** as a CLI subcommand and as a shipped
MCP tool, backed by one shared core (`src/search.rs`) so the CLI and the MCP
server always agree:

| Question | CLI | MCP tool |
| -------- | --- | -------- |
| "Where in the code is X?" (semantic search) | `--query` / `--query-only` | `sai_search_code` |
| "What looks like *this* snippet / *this* chunk?" | `similar` | `sai_find_similar` |
| "Where are we repeating ourselves?" (codebase-wide) | `duplicates` | `sai_find_duplicates` |

All three open the index **read-only**, so a search can run while an index is
open elsewhere. The `similar` and `duplicates` subcommands need a full build
(`--features all` is the recommended configuration) so every backend and
embedder is available. The top-level `--backend` / `--embedder` / `--collection`
/ `--config` flags still apply (before or after the subcommand) and pick up the
YAML defaults.

> The scores below are **cosine similarity** (higher = more alike). What counts
> as "similar enough" depends on your embedding model — see
> [Tuning similarity](./tuning-similarity.md) before you trust a threshold.

## Semantic search — "where is X?"

Use semantic search to find code by **meaning** rather than by literal text. The
query is embedded as a **query** vector and matched against the nearest indexed
chunks. This is the right tool for exploratory questions ("where do we create
the Qdrant collection?", "how do we parse transcripts?") where you do not yet
have a code sample in hand.

```bash
# Search only (read-only — does not upload the codebase).
./target/release/semanticastindexer --query-only --collection source_code \
    --query "where do we create the qdrant collection"

# --query without --query-only indexes first, then searches.
./target/release/semanticastindexer --root src --ext ts,tsx \
    --query "retry with exponential backoff"
```

`--limit` controls how many results are printed (default `5`). Each result line
shows the matched chunk's score, path, line range, and symbol.

The MCP equivalent is `sai_search_code`, which embeds the query and returns the
nearest indexed chunks. It additionally supports post-filters that the bare
`--query` CLI path does not expose:

```json
{
  "query": "retry with exponential backoff",
  "limit": 8,
  "language": "ts",
  "path_glob": "src/**",
  "include_text": false
}
```

- `query` (required) — natural-language or code query.
- `limit` — max results, clamped to 50 (default 8).
- `language` — keep only hits whose stored language label matches (e.g. `"ts"`).
- `path_glob` — keep only hits whose path matches the glob (e.g. `"src/**"`).
- `include_text` — return the full chunk text instead of a capped snippet
  (default `false`).

## Find similar — "what looks like this?"

Use **find-similar** when you already have a piece of code and want its nearest
neighbours. There are two distinct modes, and you provide **exactly one** of
them.

### By snippet (`--code`)

The snippet is embedded as a **passage** (code-vs-code space) and used to search
for neighbours. Use this for code you have not indexed yet — a snippet from a
PR, a function you are about to add, or text pasted from elsewhere.

```bash
./target/release/semanticastindexer similar \
    --code "function formatDuration(s) { return s }" --limit 8
```

> `similar --code` needs a **local** embedder (the DuckDB backend). Qdrant
> embeds server-side, so `--code` against `--backend qdrant` returns a clear
> error. See [Backends and embedders](../reference/backends-and-embedders.md).

### By existing chunk (`--path` + `--line`)

This locates an already-indexed chunk by its path and **1-based start line**,
reuses its **stored vector** (no re-embedding), and searches for neighbours with
**the chunk itself excluded** from its own results. Use this to ask "what else
in the codebase resembles *this specific* function I am looking at?".

```bash
./target/release/semanticastindexer similar \
    --path src/utils/transcriptParser.ts --line 103 --min-score 0.0
```

If there is no indexed chunk at that exact `path:line`, you get a clear
`no indexed chunk at <path>:<line>` error.

### Reading the output and threshold

`similar` prints one line per neighbour, ranked by score descending:

```text
score  path:start-end  symbol
```

Provide **exactly one** of `--code` **or** `--path` **and** `--line` — anything
else is a clear error. `--min-score` resolves **flag > config
`find_similar_min_score` (0.85) > default**. Pass `--min-score 0` to see the raw
score distribution before picking a cut.

The MCP tool `sai_find_similar` takes the same two modes — provide **either**
`code` **or** both `path` and `line`; anything else is a parameter error:

```json
{ "code": "function formatDuration(s) { return s }", "limit": 8 }
```

```json
{ "path": "src/utils/transcriptParser.ts", "line": 103, "min_score": 0.0 }
```

- `code` — snippet to embed as a passage (mutually exclusive with `path`/`line`).
- `path` + `line` — locate an existing chunk by path and 1-based start line.
- `limit` — max results, clamped to 50 (default 8).
- `min_score` — drop results below this cosine cut. When **omitted**,
  `sai_find_similar` falls back to the configured `find_similar_min_score`, so
  omitting the arg still applies the model-tuned cut; pass an explicit `0.0` to
  see the raw distribution.

## Find duplicates — "where do we repeat ourselves?"

Use **find-duplicates** for a codebase-wide near-duplicate audit. Unlike the
other two tools you give it **no query**: it scans every stored chunk and
reports clusters of chunks that are near-identical to one another. It works on
**stored vectors only** (no re-embedding), so it runs on either backend.

How it works: for each chunk it takes that chunk's `top_k` nearest neighbours,
keeps each edge whose similarity is `>= min_score`, and **unions** the connected
chunks into clusters (union-find). Clusters with at least `min_cluster_size`
members are returned **largest-first** (tie-break: higher max edge similarity),
truncated to `max_clusters`.

```bash
# Use config / built-in thresholds.
./target/release/semanticastindexer duplicates

# Tune the knobs and restrict the scan to a subtree.
./target/release/semanticastindexer duplicates \
    --min-score 0.85 --top-k 10 \
    --min-cluster-size 2 --max-clusters 20 \
    --path-glob "src/utils/**"
```

Each knob resolves **CLI flag > config (`similarity.*`) > built-in default**:

| Knob | Built-in default |
| ---- | ---------------- |
| `--min-score` (`similarity.duplicate_min_score`) | `0.93` |
| `--min-cluster-size` (`similarity.duplicate_min_cluster_size`) | `2` |
| `--top-k` (`similarity.top_k`) | `10` |
| `--max-clusters` | `50` |

`--path-glob` restricts which chunks are scanned.

### Reading the output

Clusters print largest-first, each with its size and the min/max edge similarity
inside the cluster:

```text
cluster (size N, sim min..max):
  path:start-end  symbol
  ...
```

A higher `min..max` band means the members are tighter copies of each other; a
lower band means a looser family. If you are getting too many or too few
clusters, raise or lower `--min-score` — see
[Tuning similarity](./tuning-similarity.md).

The MCP tool `sai_find_duplicates` exposes the same algorithm with the same
knobs (each resolving **tool arg > config value > built-in default**):

```json
{
  "min_score": 0.85,
  "min_cluster_size": 2,
  "top_k": 10,
  "max_clusters": 20,
  "path_glob": "src/utils/**"
}
```

- `min_score` — minimum cosine similarity for an edge to count.
- `min_cluster_size` — smallest cluster to report.
- `top_k` — nearest-neighbour fan-out per chunk (clamped to 50).
- `max_clusters` — max clusters returned, largest first (default 50).
- `path_glob` — restrict the scan to matching paths.

### Opting chunks out

Chunks marked with the opt-out marker are excluded from duplicate clustering
entirely — both as a cluster seed and as a neighbour of other chunks — so a
deliberately-repeated helper never pollutes the report. See
[Opt-out markers](./opt-out-markers.md).

## Choosing the right tool

- **You have a question, not code** → semantic search (`sai_search_code` /
  `--query`).
- **You have a snippet or one specific chunk and want its neighbours** →
  find-similar (`sai_find_similar` / `similar`).
- **You want a repository-wide repetition audit** → find-duplicates
  (`sai_find_duplicates` / `duplicates`).

## See also

- [Tuning similarity](./tuning-similarity.md) — picking model-appropriate
  thresholds.
- [Output schemas](../reference/output-schemas.md) — exact JSON shapes returned
  by the MCP tools.
- [MCP server](../reference/mcp-server.md) — full tool catalog and wiring.
