//! Backend worker thread (the actor that owns the `!Send` DuckDB resources).
//!
//! WHY THIS EXISTS: rmcp's tool-handler futures must be `Send` (the `ToolRoute`
//! bound). The DuckDB [`Backend`] embeds a `duckdb::Connection` (and, for the ort
//! embedder, an ONNX session) that are `!Send`/`!Sync`, so holding one across an
//! `.await` inside a `#[tool]` handler makes the handler future non-`Send` and the
//! crate fails to compile under the `mcp` feature.
//!
//! THE FIX (actor / worker-thread pattern): move the `!Send` [`Backend`] OFF the
//! async handler path onto a dedicated OS thread that OWNS it. That thread runs its
//! own **current-thread** Tokio runtime so it can drive the backend's async methods
//! (the Ollama embedder issues `reqwest` calls) locally. The MCP server (on the main
//! multi-thread runtime) holds only a [`BackendHandle`] wrapping an
//! `mpsc::Sender<Request>` — channels ARE `Send`+`Sync`, so each handler future
//! captures only `Send` types and compiles. Every tool call builds a [`Request`] +
//! a `oneshot` reply channel, sends it, and `.await`s the oneshot.
//!
//! This is backend-agnostic: Qdrant's backend is already `Send`, but routing every
//! backend uniformly through the one worker keeps the MCP handler code identical.

use anyhow::Result;
use tokio::sync::{mpsc, oneshot};

use crate::config::Plan;
use crate::vectordbs::{Backend, Hit};

/// A stored chunk paired with its embedding vector (the `get_by_location` /
/// `all_chunks` shape). Aliased to keep the channel reply types readable
/// (and below clippy's `type_complexity` bar).
type ChunkWithVector = (Hit, Vec<f32>);

/// A request sent to the backend worker thread. Each variant carries a `oneshot`
/// sender for its typed reply, so the caller can `.await` the result. Read variants
/// mirror the [`Backend`] methods the MCP tools use; `Refresh` is the single write
/// path (gated by `--allow-write` at the call site).
pub enum Request {
    /// Embed a query, NN-search by the resulting vector, return hits (over-fetched).
    /// `text` is embedded as a QUERY; `can_embed_locally=false` (Qdrant) falls back to
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
    /// Every stored chunk paired with its vector (optionally path-glob filtered).
    AllChunks {
        path_glob: Option<String>,
        reply: oneshot::Sender<Result<Vec<ChunkWithVector>>>,
    },
    /// Total stored chunk count.
    ChunkCount { reply: oneshot::Sender<Result<u64>> },
    /// Re-index the given paths in place (delete + re-chunk + re-embed + upsert),
    /// wrapped in a single begin_bulk/end_bulk window. Write path (requires the
    /// backend to have been opened writable).
    Refresh {
        paths: Vec<String>,
        reply: oneshot::Sender<Result<RefreshReport>>,
    },
}

/// Outcome of a `Refresh` batch: the paths re-indexed (with fresh chunk counts) and
/// the paths removed (gone/excluded). Mirrors the structured `refresh` tool result.
pub struct RefreshReport {
    /// `(path, chunks)` for each re-indexed file.
    pub refreshed: Vec<(String, usize)>,
    /// Paths that were removed (gone, excluded, or empty).
    pub removed: Vec<String>,
}

/// `Send`+`Sync` handle the MCP server holds. Cloning shares the same worker. Dropping
/// every clone closes the channel, which ends the worker loop (and the thread).
#[derive(Clone)]
pub struct BackendHandle {
    tx: mpsc::Sender<Request>,
}

/// The error returned when the worker thread is gone (channel closed). Surfaced to the
/// MCP layer as an internal error.
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

    /// Every stored chunk + vector, optionally path-glob filtered.
    pub async fn all_chunks(&self, path_glob: Option<String>) -> Result<Vec<ChunkWithVector>> {
        self.call(|reply| Request::AllChunks { path_glob, reply })
            .await
    }

    /// Total stored chunk count.
    pub async fn chunk_count(&self) -> Result<u64> {
        self.call(|reply| Request::ChunkCount { reply }).await
    }

    /// Re-index `paths` in place (write path).
    pub async fn refresh(&self, paths: Vec<String>) -> Result<RefreshReport> {
        self.call(|reply| Request::Refresh { paths, reply }).await
    }
}

/// Spawn the backend worker thread. It OWNS `backend` + `plan` and serves [`Request`]s
/// until the channel closes (every [`BackendHandle`] dropped). Returns the `Send`+`Sync`
/// handle for the MCP server. `can_embed_locally` selects the query embedding path
/// (DuckDB embeds locally; Qdrant falls back to its server-side text query).
///
/// The thread builds a **current-thread** Tokio runtime so the backend's async methods
/// (the Ollama embedder's `reqwest` calls) run locally — the `!Send` backend never
/// crosses a thread boundary.
pub fn spawn(backend: Backend, plan: Plan, can_embed_locally: bool) -> Result<BackendHandle> {
    // Bounded channel: requests are processed sequentially by the single worker, so a
    // small buffer is plenty and bounds memory under a burst of tool calls.
    let (tx, rx) = mpsc::channel::<Request>(32);

    std::thread::Builder::new()
        .name("semanticastindexer-backend".to_string())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    eprintln!("backend worker: failed to build runtime: {e}");
                    return;
                }
            };
            rt.block_on(worker_loop(backend, plan, can_embed_locally, rx));
        })
        .map_err(|e| anyhow::anyhow!("failed to spawn backend worker thread: {e}"))?;

    Ok(BackendHandle { tx })
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
            Request::AllChunks { path_glob, reply } => {
                let res = backend.all_chunks_with_vectors(path_glob.as_deref()).await;
                let _ = reply.send(res);
            }
            Request::ChunkCount { reply } => {
                let res = backend.chunk_count().await;
                let _ = reply.send(res);
            }
            Request::Refresh { paths, reply } => {
                let res = handle_refresh(&backend, &plan, &paths).await;
                let _ = reply.send(res);
            }
        }
    }
}

/// Re-index a batch of paths in one bulk window. Mirrors `sync`'s correctness contract:
/// begin_bulk (drop HNSW) → per-path delete + re-chunk + re-embed + upsert → end_bulk
/// (rebuild HNSW), rebuilding the index even when a path fails. Runs entirely on the
/// worker thread (owns the backend), so no `!Send` value crosses a thread boundary.
async fn handle_refresh(backend: &Backend, plan: &Plan, paths: &[String]) -> Result<RefreshReport> {
    backend.begin_bulk().await?;
    let mut refreshed: Vec<(String, usize)> = Vec::new();
    let mut removed: Vec<String> = Vec::new();
    let mut first_err = None;
    for rel in paths {
        match crate::reindex_file(backend, plan, rel).await {
            Ok(crate::ReindexOutcome::Reindexed { chunks }) => {
                refreshed.push((rel.trim_start_matches("./").to_string(), chunks));
            }
            Ok(crate::ReindexOutcome::Removed { .. }) => {
                removed.push(rel.trim_start_matches("./").to_string());
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
    Ok(RefreshReport { refreshed, removed })
}
