//! [`MockStore`]: a `#[cfg(test)]` [`VectorStore`] over the existing in-memory
//! [`MockBackend`], so service unit tests (US-003) can run fully in-process — NO worker
//! thread, NO network. A trivial newtype delegation, like [`crate::repos::qdrant`].

use anyhow::Result;
use async_trait::async_trait;

use crate::domain::{CodeChunk, Hit};
use crate::repos::VectorStore;
use crate::vectordbs::mock::MockBackend;

/// Newtype adapter wrapping the in-memory [`MockBackend`].
pub struct MockStore(pub MockBackend);

#[async_trait]
impl VectorStore for MockStore {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vectordbs::mock::MockRow;

    /// In-process round-trip through the trait (NO worker thread, NO network): a seeded
    /// `MockStore` returns its canned `query` hits and ranks seeded rows by `query_by_vector`.
    /// Proves the `VectorStore` trait + the in-process mock path that the services will use.
    #[tokio::test]
    async fn mock_store_round_trips_query_and_query_by_vector() {
        // `query` returns the backend's two canned hits (alpha, beta), in order.
        let store: &dyn VectorStore = &MockStore(MockBackend::new());
        let hits = store.query("anything", 10).await.unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].path, "src/alpha.ts");
        assert_eq!(hits[1].path, "src/beta.ts");

        // `query_by_vector` ranks the seeded rows by cosine to the probe; the row whose
        // vector equals the probe ranks first, and self-exclusion drops it by id.
        let rows = vec![
            MockRow::new(1, "src/one.ts", 1, vec![1.0, 0.0, 0.0, 0.0]),
            MockRow::new(2, "src/two.ts", 1, vec![0.0, 1.0, 0.0, 0.0]),
        ];
        let store = MockStore(MockBackend::with_rows(rows));
        let ranked = store
            .query_by_vector(&[1.0, 0.0, 0.0, 0.0], 10, None)
            .await
            .unwrap();
        assert_eq!(
            ranked.first().map(|h| h.id),
            Some(1),
            "closest row ranks first"
        );

        let excluded = store
            .query_by_vector(&[1.0, 0.0, 0.0, 0.0], 10, Some(1))
            .await
            .unwrap();
        assert!(
            excluded.iter().all(|h| h.id != 1),
            "exclude_id drops the self row"
        );
    }
}
