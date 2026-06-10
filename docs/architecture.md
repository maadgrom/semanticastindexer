# How it works

```text
walk --root → prune dirs → ext filter → include allow-list → exclude globs
            → generated-marker skip → strip comments
            → chunk (lines: ~60 lines / max_chunk_chars, 8-line overlap; or ast: per symbol)
            → Document("passage: <code>", model) → upsert (batch 32, server-side embed)
```

- **Point IDs** are a deterministic hash of `path + start_line`, so re-running updates
  points in place instead of duplicating. The hash is a stable `XxHash64(seed=0)`.
- **Payload** per chunk: `path`, `language`, `start_line`, `end_line`, `text` (raw code),
  and `symbol` (the captured name — only present for AST chunks; the DuckDB `symbol`
  column is nullable).
- Chunk size is bounded by `max_chunk_chars` (model-aware default; see [chunking](chunking.md)).

## Indexing flow in detail

For each file under `--root` (after directory pruning + extension filter):

1. **`include`** (allow-list) — if non-empty, the file must match one include glob, else skipped.
2. **`exclude`** globs — if matched, skipped (exclude always wins over include).
3. **`skip_generated_marker`**, then **`strip_comments`** on the surviving content.
4. Chunk the surviving content (see [chunking](chunking.md)).
5. Embed each chunk and upsert it into the configured [backend](backends-and-embedders.md).

See [configuration](configuration.md) for the full filtering rules and `indexer.yaml` reference.

## Logical audits

Key algorithmic invariants (chunking "nothing dropped", DuckDB HNSW bulk contract,
cross-backend point IDs, prefix consistency, worker isolation, dimension guards, etc.)
are documented in the repository's internal audit notes. Re-read those before making
changes to core indexing, clustering, or backend logic.
