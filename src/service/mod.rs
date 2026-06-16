//! Service layer (clean-arch): use-case orchestration over the [`crate::repos::VectorStore`]
//! port. Two services — [`IndexingService`] (write side: ensure_ready / index_sources /
//! refresh / flush) and [`QueryService`] (read side: query / find_similar / find_duplicates)
//! — dispatched over `Arc<dyn VectorStore>`.
//!
//! ADDITIVE (US-003): nothing consumes these yet (US-004 wires the CLI + a backend factory);
//! the old `crate::vectordbs::Backend` + `crate::worker` + `&Backend`-based
//! `search`/`indexer` paths stay the active ones. The orchestration loops are re-implemented
//! here over the `Send + Sync` port (the `!Sync` `Backend` cannot impl the trait); the PURE
//! shared bits — `indexer::collect_chunks` and `search::cluster_duplicates` — are CALLED,
//! never duplicated.

pub mod indexing;
pub mod query;

pub use indexing::IndexingService;
pub use query::QueryService;
