// Recurrence guard for the MCP stdout bug: diagnostics must use `tracing` (stderr),
// never `println!`. Intentional CLI *data*-output sites opt out with an explicit
// per-statement `#[allow(clippy::print_stdout)]`. `warn` (not `deny`) so a forgotten
// annotation fails clippy CI loudly without blocking unrelated local `cargo build`.
#![warn(clippy::print_stdout)]

//! Near-duplicate detection and semantic code search, as a library.
//!
//! The binary (`src/main.rs`) is a thin clap wrapper around [`app::run`]. Everything
//! else is reusable: walk/filter/chunk a source tree ([`indexer`]), resolve YAML config
//! and CLI flags into a [`config::Plan`], store/search vectors behind the
//! [`repos::VectorStore`] port (Qdrant or DuckDB, feature-gated), cluster near-duplicates
//! ([`search`]), and serve it all over MCP ([`mcp`], feature `mcp`).
//!
//! Feature flags mirror the binary: `qdrant` (default), `duckdb`, `ort`, `ollama`,
//! `ast`, `mcp`, and the canonical `all`.

// A build with no vector backend cannot index, search, or serve anything (no
// `VectorStore` adapter would compile). Reject it up front with a clear message —
// `ort`, `ollama`, `mcp`, and `all` all imply a backend, so this only fires for
// `--no-default-features` (alone or with just `ast`).
#[cfg(not(any(feature = "qdrant", feature = "duckdb", test)))]
compile_error!(
    "semanticastindexer needs at least one vector backend: enable `qdrant` and/or `duckdb` \
     (or a feature that implies one: `ort`, `ollama`, `mcp`, `all`)."
);

pub mod app;
pub mod cli;
pub mod config;
pub mod domain;
// Composition root (clean-arch): builds the read+write services + the shared
// `Arc<dyn VectorStore>` (qdrant store / duckdb closure-mailbox) for both the CLI and the
// MCP server. The sole backend-resolution path.
pub mod factory;
pub mod git;
pub mod indexer;
pub mod init;
pub mod logging;
#[cfg(feature = "mcp")]
pub mod mcp;
// Repository layer (clean-arch): the `VectorStore` port + thin adapters over the existing
// backends (qdrant/duckdb/mock), incl the DuckDb closure-mailbox. The sole storage path —
// every command talks to a backend through this port.
pub mod repos;
// Shared similarity-search core (union-find clustering): the PURE `cluster_duplicates`
// algorithm used by `QueryService::find_duplicates`. Not feature-gated (the `duplicates` /
// `similar` subcommands work with just a vector backend + embedder).
pub mod search;
// Service layer (clean-arch): the two use-case services (IndexingService + QueryService)
// over `Arc<dyn VectorStore>`. The sole orchestration path for both the CLI and the MCP
// server.
pub mod service;
pub mod vectordbs;
