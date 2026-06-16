//! Composition root (clean-arch): build the read+write services for a [`Plan`], wiring the
//! concrete backend adapter behind the [`VectorStore`] port and sharing ONE
//! `Arc<dyn VectorStore>` across both services.
//!
//! This is the ONLY module that names a concrete repo adapter (`QdrantStore` / `DuckDbStore`)
//! — the CLI and the MCP server talk to services, the services talk to the port. It resolves
//! `plan.backend`/`plan.embedder` per [`Access`] and produces the `Send + Sync` store + the
//! two services. The sole backend-resolution path.

use std::sync::Arc;
use std::thread::JoinHandle;

use anyhow::Result;

use crate::domain::Plan;
use crate::repos::VectorStore;
use crate::service::{IndexingService, QueryService};
use crate::vectordbs::Access;

/// A built store plus its optional worker [`JoinHandle`] (`None` for qdrant, `Some` for the
/// duckdb closure-mailbox). The caller joins the thread on clean shutdown.
type StoreWithWorker = (Arc<dyn VectorStore>, Option<JoinHandle<()>>);

/// Build the read+write services for `plan`, opened per `access`. Returns the two services
/// (sharing ONE `Arc<dyn VectorStore>`) plus the DuckDB worker [`JoinHandle`] (`None` for
/// qdrant) so the caller can shut the worker thread down cleanly (drop the services to close
/// the channel, then join the thread — the duckdb worker drops the backend, checkpointing the
/// WAL).
pub fn build_services(
    plan: &Plan,
    access: Access,
) -> Result<(IndexingService, QueryService, Option<JoinHandle<()>>)> {
    let (store, thread) = build_store(plan, access)?;
    let indexing = IndexingService::new(store.clone(), plan.clone());
    let query = QueryService::new(store, plan.clone());
    Ok((indexing, query, thread))
}

/// Build the services for the INDEXING path. If opening fails because an existing DuckDB index
/// was built with a different embedding model (dimension mismatch), the caller (`app`) handles
/// the interactive delete+retry — this just surfaces the error so `app::open_index_services`
/// can offer the prompt and re-call `build_services(Access::ReadWrite)`.
///
/// Kept distinct from [`build_services`] so the dim-mismatch recovery stays in `app` next to
/// the other interactive prompts (`confirm_default_no` / `delete_duckdb_file`), mirroring the
/// old `app::open_index_backend`.
// The dim-mismatch recovery lives in `app::open_index_services` (it owns the prompts); this is
// the plain ReadWrite open it retries against.
#[allow(dead_code)]
pub fn build_services_for_index(
    plan: &Plan,
) -> Result<(IndexingService, QueryService, Option<JoinHandle<()>>)> {
    build_services(plan, Access::ReadWrite)
}

/// Resolve `plan.backend`/`plan.embedder` into a shared `Arc<dyn VectorStore>` + the optional
/// worker `JoinHandle`. Arms are cfg-gated: selecting a backend whose feature was not compiled
/// in yields a clear, actionable error.
fn build_store(plan: &Plan, access: Access) -> Result<StoreWithWorker> {
    let _ = access; // only consulted by the duckdb arm
    match plan.backend.as_str() {
        "qdrant" => {
            #[cfg(feature = "qdrant")]
            {
                use crate::repos::qdrant::QdrantStore;
                use crate::vectordbs::qdrant::QdrantBackend;
                // `embedder: qdrant` = server-side inference; `ort`/`ollama` = local embed +
                // raw-vector upsert.
                let backend = match plan.embedder.as_str() {
                    "qdrant" => QdrantBackend::connect(plan)?,
                    "ort" | "ollama" => {
                        #[cfg(any(feature = "ort", feature = "ollama"))]
                        {
                            QdrantBackend::connect_local(
                                plan,
                                crate::vectordbs::build_embedder(plan)?,
                            )?
                        }
                        #[cfg(not(any(feature = "ort", feature = "ollama")))]
                        {
                            anyhow::bail!(
                                "local embedding for qdrant requires a local embedder — rebuild with --features qdrant,ort (or qdrant,ollama)"
                            )
                        }
                    }
                    other => anyhow::bail!(
                        "unknown embedder '{other}' for the qdrant backend (expected 'qdrant', 'ort', or 'ollama')"
                    ),
                };
                // QdrantBackend is Send + Sync — no worker thread needed.
                let store: Arc<dyn VectorStore> = Arc::new(QdrantStore(backend));
                Ok((store, None))
            }
            #[cfg(not(feature = "qdrant"))]
            {
                anyhow::bail!(
                    "backend 'qdrant' selected but this binary was built without the 'qdrant' feature (rebuild with --features qdrant)"
                )
            }
        }
        "duckdb" => {
            #[cfg(feature = "duckdb")]
            {
                use crate::repos::duckdb::DuckDbStore;
                use crate::vectordbs::duckdb::DuckDbBackend;
                // Build the embedder, open the file per `access` (read-only search vs
                // read-write index maintenance).
                let embedder = crate::vectordbs::build_embedder(plan)?;
                let backend = match access {
                    Access::ReadOnly => DuckDbBackend::connect_readonly(plan, embedder)?,
                    Access::ReadWrite => DuckDbBackend::connect(plan, embedder)?,
                };
                // The `!Sync` backend is confined to a dedicated worker thread; the store is
                // the `Send + Sync` mailbox facade. The JoinHandle lets the caller join on
                // clean shutdown so the backend drops (WAL checkpoint) before exit.
                let (store, thread) = DuckDbStore::spawn(backend)?;
                let store: Arc<dyn VectorStore> = Arc::new(store);
                Ok((store, Some(thread)))
            }
            #[cfg(not(feature = "duckdb"))]
            {
                anyhow::bail!(
                    "backend 'duckdb' selected but this binary was built without the 'duckdb' feature (rebuild with --features duckdb)"
                )
            }
        }
        other => anyhow::bail!("unknown backend '{other}' (expected 'qdrant' or 'duckdb')"),
    }
}
