# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2026-05-31

Initial release.

### Added

- Semantic AST code indexer with pluggable vector backends: Qdrant (Cloud server-side
  inference) and DuckDB (local VSS/HNSW cosine index).
- Pluggable embedders for the DuckDB backend: `ort` (local ONNX Runtime, offline) and
  `ollama` (remote HTTP).
- Model-aware embedding prefixes (E5 / Qwen / none).
- Pluggable chunker: line-window (default) and AST (tree-sitter, symbol-aware, TS/TSX).
- YAML configuration controlling excluded dirs/globs, generated-marker skip, and comment
  stripping.
- CLI commands: index, `sync`, `flush`, `--dry-run`, `--query`/`--query-only`, `similar`,
  `duplicates`.
- MCP server (`mcp` subcommand) exposing read-only semantic search tools over stdio.
- Cargo feature matrix: `qdrant` (default), `duckdb`, `ort`, `ollama`, `ast`, `mcp`, `all`.
