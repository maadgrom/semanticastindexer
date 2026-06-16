//! Service layer (clean-arch): use-case orchestration over the [`crate::repos::VectorStore`]
//! port. Two services — [`IndexingService`] (write side: ensure_ready / index_sources /
//! refresh / flush) and [`QueryService`] (read side: query / find_similar / find_duplicates)
//! — dispatched over `Arc<dyn VectorStore>`.
//!
//! The sole orchestration path: the CLI (`crate::app`) and the MCP server (`crate::mcp`)
//! both drive these services. The orchestration loops run over the `Send + Sync` port; the
//! PURE shared bits — `indexer::collect_chunks` and `search::cluster_duplicates` — are
//! CALLED, never duplicated.

pub mod indexing;
pub mod query;

pub use indexing::IndexingService;
pub use query::QueryService;
