# Changelog

All notable changes to SemanticAstIndexer (SAI) are recorded in the project's
[`CHANGELOG.md`](https://github.com/maadgrom/semanticastindexer/blob/main/CHANGELOG.md).
The changelog format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and the project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

For downloadable builds and per-version release notes, see the
[GitHub releases page](https://github.com/maadgrom/semanticastindexer/releases).

The entries below are reproduced from the in-repo changelog; the file in the
repository is always the source of truth.

## [0.1.0] - 2026-05-31

Initial release.

### Added

- Semantic AST code indexer with pluggable vector backends: Qdrant (Cloud server-side
  inference) and DuckDB (local VSS/HNSW cosine index).
- Pluggable embedders for the DuckDB backend: `ort` (local ONNX Runtime, offline) and
  `ollama` (remote HTTP).
- Model-aware embedding prefixes (E5 / Qwen / none).
- Pluggable chunker: line-window (default) and AST (tree-sitter, symbol-aware) for
  TypeScript/TSX, Rust, and Go.
- YAML configuration controlling excluded dirs/globs, generated-marker skip, and comment
  stripping.
- CLI commands: index, `sync`, `flush`, `--dry-run`, `--query`/`--query-only`, `similar`,
  `duplicates`.
- MCP server (`mcp` subcommand) exposing read-only semantic search tools over stdio.
- Cargo feature matrix: `qdrant` (default), `duckdb`, `ort`, `ollama`, `ast`, `mcp`, `all`.

## Versioning & compatibility

SAI follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html). While the project is
in the `0.x` series, treat minor releases as potentially breaking per the SemVer pre-`1.0`
rule, and read the release notes on the
[GitHub releases page](https://github.com/maadgrom/semanticastindexer/releases) before
upgrading.

### Index-format compatibility

The index is produced by a specific combination of backend, embedder, model, and chunker.
Two indexes are only directly comparable when those are the same:

- **Embedding model.** A collection embedded with one model cannot be mixed with vectors from
  another — the vector spaces differ. Searching across a model change requires a re-index.
- **Backend.** Qdrant and DuckDB store independent indexes; switching backends means building
  a fresh index for that backend.
- **Chunker.** Switching between the `lines` and `ast` chunker changes how source is split and
  how point IDs are derived, so existing points should be rebuilt.

When in doubt after changing any of these, recreate the collection (`flush`, or index with
`--recreate`) so stale points from the previous configuration do not linger. See
[`../reference/cli.md`](../reference/cli.md) for the full flag reference.

### One-time re-index migration (point-ID hashing)

Point IDs are a stable `XxHash64(seed=0)` of `path + start_line`. This replaced the earlier,
unspecified `DefaultHasher`. Because the ID computation changed, **existing collections must be
flushed or recreated once** so that old points (keyed under the previous hashing scheme) do not
linger alongside the new ones:

```bash
# Option A: delete the whole collection, then index fresh.
./target/release/semanticastindexer flush
./target/release/semanticastindexer --root src --ext ts,tsx --collection source_code

# Option B: drop & recreate the collection in a single indexing run.
./target/release/semanticastindexer --root src --ext ts,tsx \
    --collection source_code --recreate
```

After the re-index, point IDs are
stable across subsequent `sync` runs, so incremental updates correctly replace the points for
changed files. See [`../reference/cli.md`](../reference/cli.md) for `flush`, `sync`, and
`--recreate`.
