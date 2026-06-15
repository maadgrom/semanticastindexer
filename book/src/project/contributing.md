# Contributing

Thanks for hacking on **semanticastindexer** (SAI). This page is the developer
setup guide: how to build and test the crate, what the Cargo feature flags gate,
how the `src/` tree is laid out, and how to run the MCP server locally for
debugging. Before changing anything load-bearing, read
[how it works](../concepts/how-it-works.md) for the indexing pipeline and the
invariants it must preserve.

## Toolchain

- **MSRV 1.88**, **edition 2024** (declared in `Cargo.toml` as `rust-version =
  "1.88"` / `edition = "2024"`). The floor is imposed by `ort` 2.0.0-rc.12.
- The repo ships a `rust-toolchain.toml` that pins the **stable** channel and
  installs the `rustfmt` and `clippy` components. `rustup` activates it
  automatically when you build inside the repo.

```toml
# rust-toolchain.toml
[toolchain]
channel = "stable"
components = ["rustfmt", "clippy"]
profile = "minimal"
```

## Build & test

Always build with **`--features all`** — the canonical, supported
configuration. It gives you a single binary with every backend, embedder, the
AST chunker, and the MCP server.

```bash
cargo build --release --features all   # full-featured binary
cargo test  --release --features all   # run the test suite
```

The first `--features all` build is slow (it compiles bundled DuckDB and ONNX
Runtime); subsequent builds are incremental and fast.

The `Makefile` wraps the same commands and is the easiest way to drive the
binary. Key targets (run from the repo root):

| Target | What it does |
| ------ | ------------ |
| `make build` | `cargo build --release --features all` |
| `make test` / `make test-all` | `cargo test --release --features all` |
| `make fmt` | `cargo fmt` |
| `make clippy` / `make check-all` | `cargo clippy --release --features all -- -D warnings` |
| `make clean` | `cargo clean` |
| `make help` | List all targets |

`make clippy` and `make check-all` treat warnings as errors (`-D warnings`), so
run one of them before opening a PR.

> The `build-ort`, `build-ollama`, and `build-ast` targets are legacy aliases
> that all just call `make build` (the full `--features all` build).

## Cargo feature matrix

Features are declared in `[features]` in `Cargo.toml`. The two you care about
are `default` (a minimal Qdrant-only build) and `all` (everything).

| Feature | Pulls in | Gates |
| ------- | -------- | ----- |
| `default` = `["qdrant"]` | — | the Qdrant backend only |
| `qdrant` | `qdrant-client` | **Qdrant** vector backend (Cloud, server-side inference) |
| `duckdb` | `duckdb` (bundled) | **DuckDB** storage backend (VSS/HNSW). Needs an embedder feature to produce vectors |
| `ort` | `ort`, `tokenizers`, `ndarray`, `hf-hub` (**implies `duckdb`**) | local ONNX embedder via raw ONNX Runtime, offline |
| `ollama` | `reqwest` (**implies `duckdb`**) | remote Ollama HTTP embedder |
| `ast` | `tree-sitter` + grammars for TS/TSX, Rust, Go, Python | AST chunker (backend-free; gated only to keep heavy grammars out of the default build) |
| `mcp` | `rmcp`, `schemars` (**implies `duckdb` + `ollama`**) | MCP server (`semanticastindexer mcp`), read-only, over stdio |
| `all` = `["qdrant","ort","ollama","ast","mcp"]` | — | the full binary (qdrant + duckdb + ort + ollama + ast + mcp) |

Notes drawn straight from the manifest:

- The DuckDB backend has no embedder of its own; pair `duckdb` with `ort`
  **or** `ollama`. Both embedder features imply `duckdb`.
- `mcp` defaults to `backend=duckdb` + `embedder=ollama`, which is why it pulls
  in those features so the server is usable out of the box.
- `twox-hash` is **non-optional**: `point_id` uses `XxHash64` (seed 0)
  unconditionally so Qdrant and DuckDB produce identical, stable point IDs.
- The `release` profile uses `opt-level = 3` + `lto = "thin"` (the embedder's
  mean-pool / L2-normalize loops and the tokenizer are hot, CPU-bound code).

## Module map (`src/`)

```text
src/
├── main.rs              # CLI entry point (clap): parses args, dispatches subcommands
├── config.rs            # sai-cfg.yml parsing, filters, sai-* opt-out markers
├── git.rs               # git helpers (changed-files / since for `sync`)
├── indexer.rs           # indexing pipeline: chunk → embed → upsert
├── search.rs            # query / similar / duplicates logic
├── worker.rs            # actor thread that owns the !Send DuckDB/ort resources
├── mcp.rs               # MCP server (rmcp #[tool] handlers, read-only)
└── vectordbs/
    ├── mod.rs           # Backend enum + shared types (Hit, etc.)
    ├── qdrant.rs        # Qdrant backend (feature = "qdrant")
    ├── duckdb.rs        # DuckDB backend (feature = "duckdb")
    ├── embedder.rs      # ort / ollama embedders
    └── mock.rs          # in-memory test backend (#[cfg(test)] only)
```

`worker.rs` exists because rmcp's tool-handler futures must be `Send`, but the
DuckDB `Connection` (and the ort ONNX session) are `!Send`/`!Sync`. The worker
moves those resources onto a dedicated OS thread running its own current-thread
Tokio runtime; the MCP server holds only a `Send` channel handle. Keep that
boundary intact when touching the MCP path.

## The mock backend (tests, no network)

`src/vectordbs/mock.rs` is an in-memory backend compiled **only under
`#[cfg(test)]`** — it never ships in a release binary. It runs the *real*
orchestration code (`index_sources`, `sync`, `run_query`, `flush`) with **no
network and no real Qdrant/DuckDB**, and records every backend call so tests can
assert ordering, balance, and arguments. It also seeds rows-with-vectors so the
MCP-path methods (`query_by_vector`, `get_by_location`,
`all_chunks_with_vectors`) can be tested without a real backend. This is why
`cargo test --features all` needs no credentials or running services.

## Running the MCP server locally

The MCP server is gated behind the `mcp` feature and speaks the protocol over
**stdio**. To run it against a built binary:

```bash
cargo build --release --features all
./target/release/semanticastindexer mcp
```

The server is **read-only by default**; the write tool requires `--allow-write`.
For the full list of shipped tools (`sai_search`, `sai_similar`,
`sai_duplicates`, and the rest), their input schemas, and how to wire the server
into a client's `.mcp.json`, see the
[MCP server reference](../reference/mcp-server.md). The `mcp` build defaults to
`backend=duckdb` + `embedder=ollama`, so for an end-to-end debug session index a
project into DuckDB first (e.g. `make prod TARGET=. BACKEND=duckdb`) before
launching the server over it.

## Pull requests

- Use **Conventional Commits** for commit messages and PR titles
  (`feat:`, `fix:`, `refactor:`, `docs:`, `test:`, `chore:`, `perf:`, `ci:`).
- Run `make fmt` and `make check-all` (clippy with `-D warnings`) before opening
  a PR — CI builds and tests with `--features all`.
- Add or update tests using the mock backend; they must pass offline.
- Respect the core invariants (see [how it works → Invariants](../concepts/how-it-works.md#invariants)):
  stable point IDs, the `!Send` worker boundary, the DuckDB `begin_bulk`/`end_bulk`
  contract, and the read-only-by-default MCP server.
