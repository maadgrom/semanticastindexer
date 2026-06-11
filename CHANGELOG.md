# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- AST chunking for Rust and Go (function-only, like TS/TSX).
- Library target (`src/lib.rs`): the indexing pipeline, config resolution, vector
  backends, and similarity core are now reusable from other crates; the binary is a
  thin clap wrapper around `app::run`.
- Integration tests (`tests/`) exercising the public library API: CLI/config
  precedence, walk/filter/chunk pipeline, opt-out markers, deterministic point ids.
- CI workflow (`ci.yml`): rustfmt, clippy (`-D warnings`), tests on Linux/macOS/Windows,
  MSRV (1.88) check, and a `cargo hack --each-feature` matrix.
- Hosted install page and one-line installer/uninstaller (GitHub Pages + cargo-dist).

### Changed

- All CLI commands now route backend access through the dedicated worker thread
  (previously MCP-only), so the DuckDB backend's synchronous I/O never blocks the
  main Tokio runtime; the worker thread is joined on exit so the DuckDB connection
  checkpoints its WAL cleanly. The `find_similar`/`find_duplicates` orchestration
  is now shared between the CLI and MCP (one code path in `search`).
- mdBook (`book/`) is now the single documentation source; the legacy `docs/*.md`
  pages were removed and all links repointed at the book.
- `indexer.yaml` is the single example config (`sai-cfg.yaml` removed); its chunker
  comments now describe the function-only TS/TSX/Rust/Go AST behavior.

### Fixed

- Windows builds with `--features all` no longer fail to link (LNK2038 runtime-
  library mismatch): tokenizers' `esaxx_fast` feature is dropped, removing the
  esaxx-rs C++ object whose hardcoded static CRT conflicted with bundled DuckDB's
  dynamic CRT. esaxx only accelerates unigram training, which this project never
  does; tokenizer loading/encoding is unaffected.
- CLI `sync` no longer leaves the DuckDB HNSW index dropped when a file fails
  mid-batch — it now uses the same always-rebuild refresh path as the MCP
  `sai_refresh` tool.
- `Cargo.toml` `repository` URL now points at the actual GitHub repo.
- The Makefile no longer passes a nonexistent `--language` flag to the binary
  (it broke `make run`/`make prod`; extensions are selected with `--ext`).

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
