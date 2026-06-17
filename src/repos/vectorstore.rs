//! The [`VectorStore`] port: storage + search + embedding behind one `#[async_trait]`
//! trait so dispatch is `Arc<dyn VectorStore>` across the runtime-selected backends.
//!
//! PATH B: the embedder is OWNED by the backend in this codebase (server-side inference
//! for Qdrant, a local ONNX/Ollama embedder for DuckDB and qdrant local-embed mode), so
//! we do NOT split embedding into a separate port. This trait mirrors the concrete backends'
//! inherent method surface 1:1, so each concrete repo implements it as a thin delegation. The
//! DuckDb adapter additionally confines its `!Sync` backend to a worker thread via a
//! closure-mailbox (see [`crate::repos::duckdb`]).

use anyhow::Result;
use async_trait::async_trait;

use crate::domain::{CodeChunk, Hit};

/// Storage + search + embedding port. Mirrors the existing backend method surface so the
/// concrete repos (qdrant, duckdb, mock) implement it as thin delegations. `Send + Sync`
/// so it can be shared as `Arc<dyn VectorStore>` across the services and transports.
#[async_trait]
pub trait VectorStore: Send + Sync {
    /// Prepare storage (create collection/table + indexes) if missing.
    async fn ensure_ready(&self, recreate: bool) -> Result<()>;
    /// Begin a bulk insert window (e.g. drop index). No-op for Qdrant.
    async fn begin_bulk(&self) -> Result<()>;
    /// End a bulk insert window (e.g. recreate index). No-op for Qdrant.
    async fn end_bulk(&self) -> Result<()>;
    /// Upsert a batch of chunks. Embeds internally (as the backends do today).
    async fn upsert(&self, chunks: &[CodeChunk]) -> Result<()>;
    /// Delete every stored chunk for a given file path.
    async fn delete_by_path(&self, path: &str) -> Result<()>;
    /// Nearest-neighbour search by query text.
    async fn query(&self, q: &str, limit: u64) -> Result<Vec<Hit>>;
    /// Nearest-neighbour search by a RAW vector, optionally excluding one id.
    async fn query_by_vector(
        &self,
        v: &[f32],
        limit: u64,
        exclude_id: Option<u64>,
    ) -> Result<Vec<Hit>>;
    /// Fetch a single stored chunk (and its vector) by file path + 1-based start line.
    async fn get_by_location(&self, path: &str, line: usize) -> Result<Option<(Hit, Vec<f32>)>>;
    /// Every stored chunk paired with its vector, optionally restricted to a path glob.
    async fn all_chunks_with_vectors(
        &self,
        path_glob: Option<&str>,
    ) -> Result<Vec<(Hit, Vec<f32>)>>;
    /// Total stored chunk count.
    async fn chunk_count(&self) -> Result<u64>;
    /// Quick check for any dirty-stamped chunks.
    async fn has_dirty(&self) -> Result<bool>;
    /// Drop all stored vectors (delete collection/table).
    async fn flush(&self) -> Result<()>;
    /// Embed a search query (asymmetric `query:` side) using the backend's embedder.
    async fn embed_query(&self, text: &str) -> Result<Vec<f32>>;
    /// Embed code as a stored PASSAGE (asymmetric `passage:` side / code-vs-code space).
    async fn embed_passage(&self, text: &str) -> Result<Vec<f32>>;
}
