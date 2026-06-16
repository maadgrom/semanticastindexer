//! [`QdrantStore`]: the [`VectorStore`] adapter over the existing, unchanged
//! [`QdrantBackend`]. `QdrantBackend` is already `Send + Sync` (the `qdrant_client`
//! channel is), so this is a TRIVIAL newtype: every method delegates to the inner
//! backend's same-named inherent method. Behavior is preserved exactly — in particular
//! `embed_query`/`embed_passage` bail in server mode just as the backend does today.

use anyhow::Result;
use async_trait::async_trait;

use crate::domain::{CodeChunk, Hit};
use crate::repos::VectorStore;
use crate::vectordbs::qdrant::QdrantBackend;

/// Newtype adapter wrapping the existing [`QdrantBackend`].
pub struct QdrantStore(pub QdrantBackend);

#[async_trait]
impl VectorStore for QdrantStore {
    async fn ensure_ready(&self, recreate: bool) -> Result<()> {
        self.0.ensure_ready(recreate).await
    }

    async fn begin_bulk(&self) -> Result<()> {
        self.0.begin_bulk().await
    }

    async fn end_bulk(&self) -> Result<()> {
        self.0.end_bulk().await
    }

    async fn upsert(&self, chunks: &[CodeChunk]) -> Result<()> {
        self.0.upsert(chunks).await
    }

    async fn delete_by_path(&self, path: &str) -> Result<()> {
        self.0.delete_by_path(path).await
    }

    async fn query(&self, q: &str, limit: u64) -> Result<Vec<Hit>> {
        self.0.query(q, limit).await
    }

    async fn query_by_vector(
        &self,
        v: &[f32],
        limit: u64,
        exclude_id: Option<u64>,
    ) -> Result<Vec<Hit>> {
        self.0.query_by_vector(v, limit, exclude_id).await
    }

    async fn get_by_location(&self, path: &str, line: usize) -> Result<Option<(Hit, Vec<f32>)>> {
        self.0.get_by_location(path, line).await
    }

    async fn all_chunks_with_vectors(
        &self,
        path_glob: Option<&str>,
    ) -> Result<Vec<(Hit, Vec<f32>)>> {
        self.0.all_chunks_with_vectors(path_glob).await
    }

    async fn chunk_count(&self) -> Result<u64> {
        self.0.chunk_count().await
    }

    async fn has_dirty(&self) -> Result<bool> {
        self.0.has_dirty().await
    }

    async fn flush(&self) -> Result<()> {
        self.0.flush().await
    }

    async fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        self.0.embed_query(text).await
    }

    async fn embed_passage(&self, text: &str) -> Result<Vec<f32>> {
        self.0.embed_passage(text).await
    }
}
