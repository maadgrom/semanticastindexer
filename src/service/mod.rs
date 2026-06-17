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

/// Generate the `new(store, plan)` constructor shared by the services. [`IndexingService`]
/// (write side) and [`QueryService`] (read side) hold the SAME two fields — an `Arc<dyn
/// VectorStore>` and the resolved [`crate::domain::Plan`] — so their constructors were
/// byte-identical; one definition keeps them DRY (and out of the near-duplicate index).
/// Invoked inside each service's own module so the private fields stay in scope.
macro_rules! impl_service_new {
    ($svc:ty) => {
        impl $svc {
            /// Construct the service over a shared [`crate::repos::VectorStore`] and the
            /// resolved [`crate::domain::Plan`].
            pub fn new(
                store: ::std::sync::Arc<dyn $crate::repos::VectorStore>,
                plan: $crate::domain::Plan,
            ) -> Self {
                Self { store, plan }
            }
        }
    };
}
pub(crate) use impl_service_new;
