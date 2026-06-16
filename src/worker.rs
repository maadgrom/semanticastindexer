//! Backend worker thread: the actor that owns the [`Backend`] and the ONLY place
//! its methods are called.
//!
//! WHY THIS EXISTS — two reasons, one mechanism:
//!
//! 1. **MCP (`Send` bound):** rmcp's tool-handler futures must be `Send` (the
//!    `ToolRoute` bound). The DuckDB [`Backend`] embeds a `duckdb::Connection` (and,
//!    for the ort embedder, an ONNX session) that cannot be shared across threads, so
//!    holding one across an `.await` inside a `#[tool]` handler makes the handler
//!    future non-`Send` and the crate fails to compile under the `mcp` feature.
//! 2. **CLI (no blocking the runtime):** the DuckDB backend's `async fn`s perform
//!    synchronous DuckDB I/O. Calling them on the main multi-thread Tokio runtime
//!    blocks runtime worker threads. Confining the backend to its own OS thread
//!    keeps the main runtime free.
//!
//! THE MECHANISM (actor / worker-thread pattern): the [`Backend`] moves onto a
//! dedicated OS thread that OWNS it. That thread runs its own **current-thread**
//! Tokio runtime so it can drive the backend's async methods (the Ollama embedder
//! issues `reqwest` calls) locally. Callers — the CLI orchestration in
//! [`crate::app`] and the MCP server in [`crate::mcp`] — hold only a
//! [`BackendHandle`] wrapping an `mpsc::Sender<Request>` (channels ARE
//! `Send`+`Sync`). Every operation builds a [`Request`] + a `oneshot` reply
//! channel, sends it, and `.await`s the oneshot.
//!
//! This is backend-agnostic: Qdrant's backend has no blocking I/O, but routing every
//! backend uniformly through the one worker keeps both call sites identical.

use std::collections::HashSet;

use anyhow::Result;
use tokio::sync::{mpsc, oneshot};

use crate::config::Plan;
use crate::indexer::ReindexOutcome;
use crate::search::{DupCluster, SimilarTarget};
use crate::vectordbs::{Backend, CodeChunk, Hit};
// Transitional re-export shim (US-001): `RefreshReport` now lives in `crate::domain`.
// Re-exported so existing call sites importing `crate::worker::RefreshReport` keep
// resolving without churn (and so the worker code below names it by its short name).
pub use crate::domain::RefreshReport;

/// A stored chunk paired with its embedding vector (the `get_by_location` shape).
/// Aliased to keep the channel reply types readable (and below clippy's
/// `type_complexity` bar).
type ChunkWithVector = (Hit, Vec<f32>);

/// A request sent to the backend worker thread. Each variant carries a `oneshot`
/// sender for its typed reply, so the caller can `.await` the result. Variants mirror
/// the [`Backend`] methods (plus the shared `crate::search` orchestration) that the
/// CLI commands and MCP tools need; `EnsureReady`, `Upsert`, `Flush`, and `Refresh`
/// are the write paths (the backend must have been opened writable).
pub enum Request {
    /// Prepare storage (create collection/table + indexes) if missing.
    EnsureReady {
        recreate: bool,
        reply: oneshot::Sender<Result<()>>,
    },
    /// Begin a bulk insert window (DuckDB: drop the HNSW index). No-op for Qdrant.
    BeginBulk { reply: oneshot::Sender<Result<()>> },
    /// End a bulk insert window (DuckDB: rebuild the HNSW index). No-op for Qdrant.
    EndBulk { reply: oneshot::Sender<Result<()>> },
    /// Embed + upsert one batch of chunks.
    Upsert {
        chunks: Vec<CodeChunk>,
        reply: oneshot::Sender<Result<()>>,
    },
    /// Drop all stored vectors (delete collection/table).
    Flush { reply: oneshot::Sender<Result<()>> },
    /// Text query: embed locally (DuckDB) or server-side (Qdrant), return nearest hits.
    Query {
        query: String,
        limit: u64,
        reply: oneshot::Sender<Result<Vec<Hit>>>,
    },
    /// Embed a query, NN-search by the resulting vector, return hits (over-fetched).
    /// `query` is embedded as a QUERY; `can_embed_locally=false` (Qdrant) falls back to
    /// the server-side text `query()` path inside the worker.
    SearchByQuery {
        query: String,
        fetch: u64,
        reply: oneshot::Sender<Result<Vec<Hit>>>,
    },
    /// Embed a code snippet as a PASSAGE, then NN-search by that vector.
    SearchByPassage {
        code: String,
        limit: u64,
        reply: oneshot::Sender<Result<Vec<Hit>>>,
    },
    /// NN-search by an explicit stored vector, optionally excluding one id.
    QueryByVector {
        vector: Vec<f32>,
        limit: u64,
        exclude_id: Option<u64>,
        reply: oneshot::Sender<Result<Vec<Hit>>>,
    },
    /// Fetch a stored chunk + its vector by path + 1-based line.
    GetByLocation {
        path: String,
        line: usize,
        reply: oneshot::Sender<Result<Option<ChunkWithVector>>>,
    },
    /// Total stored chunk count.
    ChunkCount { reply: oneshot::Sender<Result<u64>> },
    /// Quick check for any dirty-stamped chunks (pre-`duplicates` warning).
    HasDirty {
        reply: oneshot::Sender<Result<bool>>,
    },
    /// Codebase-wide near-duplicate scan (the shared `crate::search::find_duplicates`
    /// orchestration: all chunks → per-chunk NN → union-find clustering).
    FindDuplicates {
        min_score: f32,
        min_cluster_size: usize,
        top_k: u64,
        max_clusters: usize,
        path_glob: Option<String>,
        /// When set, only chunks in these (e.g. PR-changed) paths may seed a cluster.
        seed_paths: Option<HashSet<String>>,
        reply: oneshot::Sender<Result<Vec<DupCluster>>>,
    },
    /// Neighbours of a snippet or an existing chunk (the shared
    /// `crate::search::find_similar` resolution), `min_score`-filtered.
    FindSimilar {
        target: SimilarTarget,
        limit: u64,
        min_score: f32,
        reply: oneshot::Sender<Result<Vec<Hit>>>,
    },
    /// Re-index the given paths in place (delete + re-chunk + re-embed + upsert),
    /// wrapped in a single begin_bulk/end_bulk window. Write path (requires the
    /// backend to have been opened writable). Used by CLI `sync` and MCP `refresh`.
    Refresh {
        paths: Vec<String>,
        reply: oneshot::Sender<Result<RefreshReport>>,
    },
}

/// `Send`+`Sync` handle the CLI orchestration and the MCP server hold. Cloning shares
/// the same worker. Dropping every clone closes the channel, which ends the worker
/// loop — join the thread handle returned by [`spawn`] to wait for the backend to be
/// dropped cleanly.
#[derive(Clone)]
pub struct BackendHandle {
    tx: mpsc::Sender<Request>,
}

/// The error returned when the worker thread is gone (channel closed). Surfaced to the
/// caller as an internal error.
fn worker_gone() -> anyhow::Error {
    anyhow::anyhow!("backend worker thread is no longer running")
}

impl BackendHandle {
    /// Send a request and await its `oneshot` reply, mapping channel closure to a clear
    /// error. Generic over the reply type so each typed method stays a one-liner.
    async fn call<T>(&self, make: impl FnOnce(oneshot::Sender<Result<T>>) -> Request) -> Result<T> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(make(reply_tx))
            .await
            .map_err(|_| worker_gone())?;
        reply_rx.await.map_err(|_| worker_gone())?
    }

    /// Prepare storage (create collection/table + indexes) if missing.
    ///
    /// Spanned at INFO so the wall-clock cost (channel + worker `ensure_ready`) shows up
    /// at the default log level. Span context does NOT cross the mpsc channel, so the
    /// inner backend call (if instrumented) appears as a separate root span by design.
    #[tracing::instrument(level = "info", skip(self), fields(recreate))]
    pub async fn ensure_ready(&self, recreate: bool) -> Result<()> {
        self.call(|reply| Request::EnsureReady { recreate, reply })
            .await
    }

    /// Begin a bulk insert window (see [`Request::BeginBulk`]).
    pub async fn begin_bulk(&self) -> Result<()> {
        self.call(|reply| Request::BeginBulk { reply }).await
    }

    /// End a bulk insert window (see [`Request::EndBulk`]).
    pub async fn end_bulk(&self) -> Result<()> {
        self.call(|reply| Request::EndBulk { reply }).await
    }

    /// Embed + upsert one batch of chunks.
    pub async fn upsert(&self, chunks: Vec<CodeChunk>) -> Result<()> {
        self.call(|reply| Request::Upsert { chunks, reply }).await
    }

    /// Drop all stored vectors (delete collection/table).
    pub async fn flush(&self) -> Result<()> {
        self.call(|reply| Request::Flush { reply }).await
    }

    /// Text query (the CLI `--query` path): nearest hits for `query`.
    ///
    /// Spanned at INFO for wall-clock query timing. `query` is skipped from the auto-fields
    /// (could be large) and recorded truncated; `limit` is captured as-is. Span context
    /// does not cross the worker channel (see `ensure_ready`).
    #[tracing::instrument(
        level = "info",
        skip(self, query),
        fields(query = %query.chars().take(80).collect::<String>(), limit)
    )]
    pub async fn query(&self, query: String, limit: u64) -> Result<Vec<Hit>> {
        self.call(|reply| Request::Query {
            query,
            limit,
            reply,
        })
        .await
    }

    /// Embed `query` and NN-search; `fetch` over-fetches for post-filtering.
    pub async fn search_by_query(&self, query: String, fetch: u64) -> Result<Vec<Hit>> {
        self.call(|reply| Request::SearchByQuery {
            query,
            fetch,
            reply,
        })
        .await
    }

    /// Embed `code` as a passage and NN-search.
    pub async fn search_by_passage(&self, code: String, limit: u64) -> Result<Vec<Hit>> {
        self.call(|reply| Request::SearchByPassage { code, limit, reply })
            .await
    }

    /// NN-search by an explicit stored vector, optionally excluding `exclude_id`.
    pub async fn query_by_vector(
        &self,
        vector: Vec<f32>,
        limit: u64,
        exclude_id: Option<u64>,
    ) -> Result<Vec<Hit>> {
        self.call(|reply| Request::QueryByVector {
            vector,
            limit,
            exclude_id,
            reply,
        })
        .await
    }

    /// Fetch a stored chunk + its vector by path + line.
    pub async fn get_by_location(
        &self,
        path: String,
        line: usize,
    ) -> Result<Option<ChunkWithVector>> {
        self.call(|reply| Request::GetByLocation { path, line, reply })
            .await
    }

    /// Total stored chunk count.
    pub async fn chunk_count(&self) -> Result<u64> {
        self.call(|reply| Request::ChunkCount { reply }).await
    }

    /// Quick check for any dirty-stamped chunks (best-effort; see `Backend::has_dirty`).
    pub async fn has_dirty(&self) -> Result<bool> {
        self.call(|reply| Request::HasDirty { reply }).await
    }

    /// Codebase-wide near-duplicate scan (shared CLI + MCP orchestration).
    pub async fn find_duplicates(
        &self,
        min_score: f32,
        min_cluster_size: usize,
        top_k: u64,
        max_clusters: usize,
        path_glob: Option<String>,
        seed_paths: Option<HashSet<String>>,
    ) -> Result<Vec<DupCluster>> {
        self.call(|reply| Request::FindDuplicates {
            min_score,
            min_cluster_size,
            top_k,
            max_clusters,
            path_glob,
            seed_paths,
            reply,
        })
        .await
    }

    /// Neighbours of a snippet or an existing chunk, `min_score`-filtered.
    pub async fn find_similar(
        &self,
        target: SimilarTarget,
        limit: u64,
        min_score: f32,
    ) -> Result<Vec<Hit>> {
        self.call(|reply| Request::FindSimilar {
            target,
            limit,
            min_score,
            reply,
        })
        .await
    }

    /// Re-index `paths` in place (write path).
    ///
    /// Spanned at INFO for wall-clock refresh timing (channel + per-path delete/re-embed
    /// on the worker). Only the input path count is known at the call site (the
    /// reindexed/removed breakdown is in the reply); the full path list is skipped.
    #[tracing::instrument(level = "info", skip(self, paths), fields(paths = paths.len()))]
    pub async fn refresh(&self, paths: Vec<String>) -> Result<RefreshReport> {
        self.call(|reply| Request::Refresh { paths, reply }).await
    }
}

/// Spawn the backend worker thread. It OWNS `backend` + `plan` and serves [`Request`]s
/// until the channel closes (every [`BackendHandle`] dropped). `can_embed_locally`
/// selects the query embedding path (DuckDB embeds locally; Qdrant falls back to its
/// server-side text query).
///
/// Returns the `Send`+`Sync` handle plus the thread's `JoinHandle`: after dropping the
/// last handle clone, `join()` waits for the worker to drop the backend — so e.g. the
/// DuckDB connection checkpoints its WAL before the process exits.
///
/// The thread builds a **current-thread** Tokio runtime so the backend's async methods
/// (the Ollama embedder's `reqwest` calls) run locally — the backend never leaves the
/// thread that owns it.
pub fn spawn(
    backend: Backend,
    plan: Plan,
    can_embed_locally: bool,
) -> Result<(BackendHandle, std::thread::JoinHandle<()>)> {
    // Bounded channel: requests are processed sequentially by the single worker, so a
    // small buffer is plenty and bounds memory under a burst of tool calls.
    let (tx, rx) = mpsc::channel::<Request>(32);

    let thread = std::thread::Builder::new()
        .name("semanticastindexer-backend".to_string())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::error!(error = %e, "backend worker: failed to build runtime");
                    return;
                }
            };
            rt.block_on(worker_loop(backend, plan, can_embed_locally, rx));
        })
        .map_err(|e| anyhow::anyhow!("failed to spawn backend worker thread: {e}"))?;

    Ok((BackendHandle { tx }, thread))
}

/// The worker loop: own the backend, serve requests one at a time. Each request is
/// handled fully (including its `.await`s) before the next is taken, so the single
/// DuckDB connection is never touched concurrently. Send errors on the `oneshot`
/// (caller dropped) are ignored — the work is simply discarded.
async fn worker_loop(
    backend: Backend,
    plan: Plan,
    can_embed_locally: bool,
    mut rx: mpsc::Receiver<Request>,
) {
    while let Some(req) = rx.recv().await {
        match req {
            Request::EnsureReady { recreate, reply } => {
                let _ = reply.send(backend.ensure_ready(recreate).await);
            }
            Request::BeginBulk { reply } => {
                let _ = reply.send(backend.begin_bulk().await);
            }
            Request::EndBulk { reply } => {
                let _ = reply.send(backend.end_bulk().await);
            }
            Request::Upsert { chunks, reply } => {
                let _ = reply.send(backend.upsert(&chunks).await);
            }
            Request::Flush { reply } => {
                let _ = reply.send(backend.flush().await);
            }
            Request::Query {
                query,
                limit,
                reply,
            } => {
                let _ = reply.send(backend.query(&query, limit).await);
            }
            Request::SearchByQuery {
                query,
                fetch,
                reply,
            } => {
                let res = if can_embed_locally {
                    match backend.embed_query(&query).await {
                        Ok(vec) => backend.query_by_vector(&vec, fetch, None).await,
                        Err(e) => Err(e),
                    }
                } else {
                    // Qdrant: no local embedder — server-side text query path.
                    backend.query(&query, fetch).await
                };
                let _ = reply.send(res);
            }
            Request::SearchByPassage { code, limit, reply } => {
                let res = match backend.embed_passage(&code).await {
                    Ok(vec) => backend.query_by_vector(&vec, limit, None).await,
                    Err(e) => Err(e),
                };
                let _ = reply.send(res);
            }
            Request::QueryByVector {
                vector,
                limit,
                exclude_id,
                reply,
            } => {
                let res = backend.query_by_vector(&vector, limit, exclude_id).await;
                let _ = reply.send(res);
            }
            Request::GetByLocation { path, line, reply } => {
                let res = backend.get_by_location(&path, line).await;
                let _ = reply.send(res);
            }
            Request::ChunkCount { reply } => {
                let _ = reply.send(backend.chunk_count().await);
            }
            Request::HasDirty { reply } => {
                let _ = reply.send(backend.has_dirty().await);
            }
            Request::FindDuplicates {
                min_score,
                min_cluster_size,
                top_k,
                max_clusters,
                path_glob,
                seed_paths,
                reply,
            } => {
                let res = crate::search::find_duplicates(
                    &backend,
                    min_score,
                    min_cluster_size,
                    top_k,
                    max_clusters,
                    path_glob.as_deref(),
                    seed_paths.as_ref(),
                )
                .await;
                let _ = reply.send(res);
            }
            Request::FindSimilar {
                target,
                limit,
                min_score,
                reply,
            } => {
                let res = crate::search::find_similar(&backend, target, limit, min_score).await;
                let _ = reply.send(res);
            }
            Request::Refresh { paths, reply } => {
                let res = handle_refresh(&backend, &plan, &paths).await;
                let _ = reply.send(res);
            }
        }
    }
}

/// Re-index a batch of paths in one bulk window: begin_bulk (drop HNSW) → per-path
/// delete + re-chunk + re-embed + upsert → end_bulk (rebuild HNSW), rebuilding the
/// index even when a path fails. Runs entirely on the worker thread (owns the
/// backend). Shared by CLI `sync` and the MCP `refresh` tool.
///
/// LOGICAL INVARIANT: end_bulk is *always* called (even on error) so the HNSW index
/// is never left dropped after a refresh operation.
async fn handle_refresh(backend: &Backend, plan: &Plan, paths: &[String]) -> Result<RefreshReport> {
    backend.begin_bulk().await?;
    let mut entries: Vec<(String, ReindexOutcome)> = Vec::with_capacity(paths.len());
    let mut first_err = None;
    for rel in paths {
        // Capture fresh git ctx per path for an accurate dirty/commit stamp (the MCP
        // server is long-lived, so a startup-time capture would go stale).
        let ctx = crate::git::capture();
        match crate::indexer::reindex_file(backend, plan, rel, &ctx).await {
            Ok(outcome) => {
                entries.push((rel.trim_start_matches("./").to_string(), outcome));
            }
            Err(e) => {
                first_err = Some(e);
                break;
            }
        }
    }
    // Always rebuild the index before returning, even on error.
    let end = backend.end_bulk().await;
    if let Some(e) = first_err {
        return Err(e);
    }
    end?;
    Ok(RefreshReport { entries })
}
