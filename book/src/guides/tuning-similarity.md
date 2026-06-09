# Tuning similarity

Semantic search ranks results by **cosine similarity**: a number between roughly
`0.0` and `1.0` where higher means "closer in embedding space". Whether a given
score means "a real near-duplicate" or "loosely related" depends entirely on the
embedding model you chose. This page explains the four threshold knobs SAI exposes,
how they resolve, and a concrete workflow for picking values that fit *your* model.

For what these thresholds gate (the `similar` / `duplicates` features themselves),
see [Search and duplicates](./search-and-duplicates.md). For where to put the
config, see [Configuration](../reference/configuration.md). For the MCP tool args,
see [MCP server](../reference/mcp-server.md).

## The four knobs

All four live in the `similarity:` block of `indexer.yaml`:

| Knob | Default | Used by | Meaning |
|------|---------|---------|---------|
| `find_similar_min_score` | `0.85` | `sai_find_similar` / `similar` | Drop neighbours scoring below this cosine cut. |
| `duplicate_min_score` | `0.93` | `sai_find_duplicates` / `duplicates` | Minimum cosine for an *edge* between two chunks to count as a near-duplicate. |
| `duplicate_min_cluster_size` | `2` | `sai_find_duplicates` / `duplicates` | Smallest cluster (number of members) that gets reported. |
| `top_k` | `10` | `sai_find_duplicates` / `duplicates` | Per-chunk nearest-neighbour fan-out gathered before clustering. |

`find_duplicates` works by taking each stored chunk, fetching its `top_k` nearest
neighbours, keeping every edge whose similarity is `>= duplicate_min_score`, and
running union-find over the kept edges. Components with at least
`duplicate_min_cluster_size` members become reported clusters (largest first).

> `find_similar_min_score` is a flat post-filter on a single query's results.
> `duplicate_min_score` is an *edge* threshold inside the clustering graph, which
> is why its default sits higher: a duplicate should be a tighter match than a
> merely "similar" result.

## Resolution order

Every knob resolves the same way, **per knob, independently**:

```text
CLI flag / MCP tool arg  >  config (similarity.*)  >  built-in default
```

So if `indexer.yaml` sets `duplicate_min_score: 0.90` but a `sai_find_duplicates`
call passes `min_score: 0.95`, the call uses `0.95`. Omit the arg and the call
falls back to the configured `0.90`; omit the config key too and it falls back to
the built-in `0.93`.

```yaml
# indexer.yaml
similarity:
  find_similar_min_score: 0.80
  duplicate_min_score: 0.90
  duplicate_min_cluster_size: 2
  top_k: 12
```

Every field is optional — a partial `similarity:` block (or none at all) just uses
built-in defaults for whatever you leave out.

## Cutoffs are model-specific

**Tune the thresholds per model.** A cosine of
`0.85` does not mean the same thing across embedders: a code-trained model (e.g.
Jina code, the default for the `ort` embedder) generally produces *lower* raw
cosines for the same pair of snippets than a general text model like
`multilingual-e5-small`, and a Qwen-style model differs again. Concretely, a Qwen3
cosine of `0.85` is a *looser* match than e5's `0.85`, so the value that cleanly
separates "duplicate" from "merely similar" is different for each.

That means the built-in defaults (`0.85` / `0.93`) are a starting point, **not** a
universal truth. If you switch the model — or the embedder, which changes the
default model — re-tune the `similarity:` block. See
[Choosing a model](./choosing-a-model.md) for the model/embedder pairings.

## Workflow: read the distribution, then set thresholds

Don't guess. Turn the filter off, look at the actual scores your model produces on
your code, then pick cutoffs from the gap in the distribution.

**1. See the raw scores.** Call `find_similar` with `min_score` set to `0` so
nothing is filtered out:

```bash
# CLI: neighbours of an existing indexed chunk, unfiltered
semanticastindexer similar \
  --path src/utils/parse.ts --line 42 \
  --limit 20 \
  --min-score 0

# CLI: neighbours of an inline snippet, unfiltered
semanticastindexer similar \
  --code 'function clamp(x, lo, hi) { return Math.max(lo, Math.min(hi, x)); }' \
  --limit 20 \
  --min-score 0
```

From an MCP client, call `sai_find_similar` with `min_score: 0`:

```json
{ "name": "sai_find_similar",
  "arguments": { "path": "src/utils/parse.ts", "line": 42, "limit": 20, "min_score": 0 } }
```

**2. Read the gap.** You'll typically see a cluster of high scores (the genuine
matches) then a drop-off into noise. Pick `find_similar_min_score` just above the
noise floor and `duplicate_min_score` up near the tight top of the distribution.

**3. Probe the duplicate edge threshold.** Run `duplicates` with a deliberately low
`--min-score` to see what clusters at all, then raise it until only true
near-duplicates survive:

```bash
# Start permissive to see everything that clusters, then tighten min_score upward
semanticastindexer duplicates --min-score 0.80 --min-cluster-size 2 --top-k 10
```

**4. Write the chosen values** into `similarity:` in `indexer.yaml` so every search,
CLI run, and MCP server picks them up by default. The CLI flags / MCP args remain
available for one-off overrides.

## Scoping duplicates with a path glob

Both the `duplicates` CLI subcommand and the `sai_find_duplicates` tool accept a
path glob to restrict the scan to part of the tree — useful for hunting copy-paste
within one module without scanning the whole repo:

```bash
# Only cluster chunks under src/utils
semanticastindexer duplicates --path-glob 'src/utils/**' --min-score 0.93
```

```json
{ "name": "sai_find_duplicates",
  "arguments": { "path_glob": "src/utils/**", "min_score": 0.93, "min_cluster_size": 2 } }
```

The glob filters which chunks enter the scan; the threshold knobs above still decide
which of those chunks cluster together.

## Knob-tuning cheatsheet

- **Too many weak "similar" hits** → raise `find_similar_min_score`.
- **Real matches getting dropped** → lower `find_similar_min_score` (re-check the raw
  distribution first).
- **Duplicate clusters that aren't actually duplicates** → raise `duplicate_min_score`.
- **Known copies not clustering** → lower `duplicate_min_score`, or raise `top_k` so
  the neighbour fan-out reaches them.
- **Want only larger duplicate groups** → raise `duplicate_min_cluster_size` (it
  defaults to `2`, the smallest meaningful cluster).

## See also

- [Search and duplicates](./search-and-duplicates.md) — running the searches these knobs tune.
- [Configuration](../reference/configuration.md) — the full `indexer.yaml` schema.
- [MCP server](../reference/mcp-server.md) — `sai_find_similar` / `sai_find_duplicates` tool args.
- [Choosing a model](./choosing-a-model.md) — why the model changes what a score means.
