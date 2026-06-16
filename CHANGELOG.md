# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Structured tracing-based logging facade** — All diagnostics now flow through `tracing`
  to **stderr** (preserving clean **stdout** for JSON-RPC and CLI data). Logs are leveled (`debug`,
  `info`, `warn`, `error`), filterable via `RUST_LOG`, and honor per-operation timing spans (e.g.
  `INFO sync{rev=HEAD~1}: … in 1.8s`). New global flags:
  - `-v, --verbose` (repeatable; `-vv` for trace level)
  - `--log-format pretty|json` (default `pretty`; JSON for machine consumption)
  - `--silent` now maps to a quiet log filter (`error` level) in addition to suppressing CLI output
  
  Default behavior: `info` level, pretty format, stderr. `RUST_LOG=semanticastindexer=debug`
  overrides all flags (power-user priority). The `mcp` subcommand now keeps stdout clean of
  diagnostic noise, enabling write-mode (`--allow-write`) with strict stdio clients (e.g. Grok).
- **`-V, --version` flag** — print the binary version (was previously unsupported).
- **`update` skips a no-op reinstall** — `update` now checks the latest GitHub release and exits
  early ("already the latest release") when the running binary already matches, instead of always
  re-running the installer. Falls back to a plain reinstall if the version check fails (offline).

### Fixed

- **MCP `--allow-write` stdout corruption** — Write-mode MCP was broken for strict JSON-RPC
  clients (e.g. Grok) because backend diagnostics (e.g. "using existing collection") were printed
  to stdout, corrupting the JSON-RPC stream. All such diagnostics now route to **stderr** via
  `tracing`. In-session `sai_refresh` and `sai_sync` are now usable with strict-client MCP
  integrations.
- **Git diagnostics leaking to stderr** — the dirty-state check (`git diff --quiet`) inherited the
  process stdio, so running outside a git repo (or any git error) leaked raw git text to stderr.
  Its output is now silenced; diagnostics come only from the `tracing` subscriber.

## [0.1.5] - 2026-06-16

### Added

- **Qdrant local-embed mode** — `embedder: ort`/`ollama` on the `qdrant` backend now embeds
  on-device (via the existing ONNX/Ollama embedder) and upserts raw `Vec<f32>` points, instead of
  requiring Qdrant Cloud server-side inference. This makes the `qdrant` backend work with
  **self-hosted / OSS Qdrant** (which has no inference engine) and lets code models such as
  `jinaai/jina-embeddings-v2-base-code` (768-d) run against a local cluster without Cloud billing.
  The default `embedder: qdrant` keeps server-side inference, so existing Qdrant Cloud configs are
  unchanged. (#12)
- `sai-deslop` agent skill (`.agents/skills/sai-deslop/`): when to reach for the `sai_` tools
  while coding, plus a triage protocol that judges each duplicate/similarity finding (read the
  real source → classify real / boilerplate / intentional / fragment → propose a verified fix)
  before acting.
- `dedup-auditor` Claude Code subagent (`.claude/agents/`): runs the repo-wide
  `sai_find_duplicates` sweep with that triage protocol in an isolated context and returns a
  classified digest instead of a raw cluster dump.
- `mcp-setup/setup.sh` gains `--platform <id>` / `--write` to wire any MCP client
  (claude-code, claude-desktop, cursor, windsurf, continue, codex, hermes, generic) — the same
  multi-client experience as the one-line `install.sh`, via a shared `mcp-setup/lib/mcp-config.sh`.
- `mcp-setup/tests/test_setup.sh` (run by `make test` / `make test-setup`): asserts artifact
  paths, generated config fields, command strings, and `install.sh`↔`lib` snippet parity.

### Changed

- `embedder` is now backend-scoped: it selects how the `qdrant` backend embeds (`qdrant` =
  server-side inference, `ort`/`ollama` = local), and a config's `embedder` no longer leaks across
  a `--backend` override — the override re-derives the backend's natural default (qdrant →
  server-side, every other backend → `ort`). (#12)
- `mcp-setup/setup.sh` now generates `sai-cfg.yml` by copying the canonical
  `mcp-setup/templates/sai-cfg.yml` (duckdb + `ort` + jina-768 + AST + tuned thresholds) and
  patching only backend/embedder/collection, instead of emitting its own divergent inline YAML.
  The default `--embedder` is now `ort` (fully offline), matching the template and the book.
- `mcp-setup/setup.sh` and `docs/install.sh` now install both the `sai` and `sai-deslop` skills
  (plus the `dedup-auditor` subagent); the skill installer is renamed `install_agent_skill`.

### Fixed

- `sai_prepare_mcp_setup` now includes `--target-dir` in `recommended_command` (so a copied
  command targets the intended project) and a correct, explicitly-derived `--features` list
  (previously a dead variable plus a hardcoded `mcp,duckdb,ollama,ast` that ignored `ort`/`qdrant`).
  For prebuilt/release binaries where `mcp-setup/setup.sh` is not on disk, `recommended_command`
  falls back to the `install.sh` one-liner and an `execute: true` call is reported as blocked
  rather than running a stale path.

## [0.1.4] - 2026-06-15

### Fixed

- `ort` embedder: retry transient HuggingFace model downloads (e.g. HTTP 429) instead of
  failing the run, and cache the downloaded model in CI to avoid repeated fetches. (#10)

### Documentation

- Promote Python AST chunking across the landing page and the book. Python AST support
  shipped in 0.1.2, but the static site and several book pages still advertised only
  TypeScript/TSX, Rust, and Go. (#11)
- The documentation site and book now default to a light theme; dark is still served to
  visitors whose browser or OS prefers it. (#11)

## [0.1.3] - 2026-06-15

### Added

- `sai_sync` MCP tool: reconcile the index with the working tree from an agent — the MCP
  analog of the CLI `sync` (git-changed set → re-chunk/re-embed survivors, drop deleted or
  now-excluded paths). Write tool; requires `--allow-write`, like `sai_refresh`.
- `duplicates --since` / `--staged` / `--file` (+ `--json`): seed near-duplicate clusters from
  the changed-file set, so the CI dedup gate can flag when changed code near-duplicates the
  already-indexed code.

### Changed

- MCP server entry renamed from `semantic-code-search` / `code-search` to `sai` across all
  example configs (`.mcp.json.example`, `claude-desktop-config.example.json`). The
  `~/.local/bin` convenience wrapper and the `~/.claude/skills/` skill directory are likewise
  now named `sai`, and the installed wrapper is a true passthrough (`sai mcp`/`sai index`/… work).
- The MCP server now honors `backend:` / `embedder:` from `sai-cfg.yml`. Precedence is
  `--flag > sai-cfg.yml > duckdb/ort` (the fully-offline default); previously the server forced
  its own duckdb/ollama defaults *over* the config. MCP example configs are now just
  `["mcp", "--config", "sai-cfg.yml"]` — the redundant `--backend`/`--embedder`/`--collection`
  flags were removed.
- Config loader: an explicit `--config sai-cfg.yml` / `sai-cfg.yaml` that is absent on
  disk now falls back to built-in defaults instead of erroring, so the MCP server works
  in projects that have not yet run `init`.
- `mcp-setup/setup.sh` now installs the Claude Code skill into `~/.claude/skills/sai/`, matching
  the human `install.sh` (the agent-facing setup path previously installed only the MCP server).
- `uninstall.sh` and the installers now also remove the old `code-search`-named artifacts
  (`~/.local/bin/code-search-mcp`, `~/.claude/skills/semantic-code-search-mcp/`) when
  upgrading from a prior release.

## [0.1.2] - 2026-06-13

### Added

- AST chunking for Python (function-only, like TS/TSX/Rust/Go): every `def` /
  `async def` — free functions, class methods, and nested functions — becomes one
  symbol-tagged chunk; `lambda`s are not captured. `py` joins the smart-default
  extension list, so `--ext py` auto-selects the `ast` chunker on `--features ast`
  builds.
- `dedup-gate.yml` PR workflow: the repo now dogfoods its own near-duplicate
  detection. It builds the PR binary, indexes the base branch into a per-PR Qdrant
  Cloud collection (server-side e5-small inference), runs the real `sync --since` to
  advance the index to the PR head, and fails only when the near-duplicate cluster
  count in `src/` grows. A companion workflow flushes the collection when the PR
  closes.

### Fixed

- Qdrant `duplicates` / `similar`: read dense vectors from the nested `vector` oneof
  returned by Qdrant >= 1.16 (the deprecated flat `data` field is now empty), so
  retrieved vectors are no longer length 0. Without this, both commands failed
  against current Qdrant Cloud with "Vector dimension error: expected dim: N, got 0".

## [0.1.1] - 2026-06-12

### Added

- `init` subcommand: generates the standard, fully-commented `sai-cfg.yml` via a short
  interview (backend, embedder, collection, model, optional connection settings and
  extra excludes). `--yes` accepts every default non-interactively; `--force`
  overwrites; `--output <path>` redirects. `vector_dim` is auto-filled for recognized
  models and asked for otherwise; the generated file is validated before writing.
- `update` subcommand: self-update to the latest GitHub release via the official
  release installer (on Windows it prints the PowerShell one-liner instead, since a
  running executable cannot replace itself).
- Native Windows installer wrapper (`install.ps1`): same agent-wiring flags as
  `install.sh`, PowerShell-style (`-Platform`, `-All`, `-Write`, …), hosted on the
  install page alongside the shell installer.
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

- `sai-cfg.yml` is the standard config name: it is what `init` generates and the first
  file sought when `--config` is omitted (then `sai-cfg.yaml`, then the legacy
  `indexer.yaml`, which keeps working). The repo's example config is now `sai-cfg.yml`,
  generated by `init --yes`.
- All CLI commands now route backend access through the dedicated worker thread
  (previously MCP-only), so the DuckDB backend's synchronous I/O never blocks the
  main Tokio runtime; the worker thread is joined on exit so the DuckDB connection
  checkpoints its WAL cleanly. The `find_similar`/`find_duplicates` orchestration
  is now shared between the CLI and MCP (one code path in `search`).
- mdBook (`book/`) is now the single documentation source; the legacy `docs/*.md`
  pages were removed and all links repointed at the book.
- The example config's chunker comments now describe the function-only
  TS/TSX/Rust/Go AST behavior.

### Fixed

- The installers' "Next steps" now print the bare `semanticastindexer` command (the
  absolute path could be wrong when the install dir was not yet on the current
  shell's PATH) and tell you to open a new terminal when needed; absolute paths are
  still used where they belong — inside the generated MCP config snippets.
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
