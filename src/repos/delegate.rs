//! `impl_vectorstore_delegate!`: one source of truth for the trivial newtype adapters.
//!
//! `QdrantStore` and the `#[cfg(test)]` `MockStore` each wrap a `Send + Sync` backend whose
//! inherent method surface already matches [`VectorStore`](crate::repos::VectorStore) 1:1, so
//! their trait impls were byte-identical forwarding bodies (`self.0.method(..).await`). This
//! macro generates that impl from the wrapped-type name, keeping the delegation in ONE place
//! (the near-duplicate gate flagged the hand-written copies). The DuckDB adapter does NOT use
//! it: every call there is marshalled across a worker thread, so it hand-writes the impl.

/// Generate `impl VectorStore for $store` where every method forwards to the same-named
/// inherent method on the wrapped backend (`self.0`). `$store` must be a newtype whose `.0`
/// is a backend exposing the full inherent method surface.
macro_rules! impl_vectorstore_delegate {
    ($store:ty) => {
        #[::async_trait::async_trait]
        impl $crate::repos::VectorStore for $store {
            async fn ensure_ready(&self, recreate: bool) -> ::anyhow::Result<()> {
                self.0.ensure_ready(recreate).await
            }
            async fn begin_bulk(&self) -> ::anyhow::Result<()> {
                self.0.begin_bulk().await
            }
            async fn end_bulk(&self) -> ::anyhow::Result<()> {
                self.0.end_bulk().await
            }
            async fn upsert(&self, chunks: &[$crate::domain::CodeChunk]) -> ::anyhow::Result<()> {
                self.0.upsert(chunks).await
            }
            async fn delete_by_path(&self, path: &str) -> ::anyhow::Result<()> {
                self.0.delete_by_path(path).await
            }
            async fn query(
                &self,
                q: &str,
                limit: u64,
            ) -> ::anyhow::Result<Vec<$crate::domain::Hit>> {
                self.0.query(q, limit).await
            }
            async fn query_by_vector(
                &self,
                v: &[f32],
                limit: u64,
                exclude_id: Option<u64>,
            ) -> ::anyhow::Result<Vec<$crate::domain::Hit>> {
                self.0.query_by_vector(v, limit, exclude_id).await
            }
            async fn get_by_location(
                &self,
                path: &str,
                line: usize,
            ) -> ::anyhow::Result<Option<($crate::domain::Hit, Vec<f32>)>> {
                self.0.get_by_location(path, line).await
            }
            async fn all_chunks_with_vectors(
                &self,
                path_glob: Option<&str>,
            ) -> ::anyhow::Result<Vec<($crate::domain::Hit, Vec<f32>)>> {
                self.0.all_chunks_with_vectors(path_glob).await
            }
            async fn chunk_count(&self) -> ::anyhow::Result<u64> {
                self.0.chunk_count().await
            }
            async fn has_dirty(&self) -> ::anyhow::Result<bool> {
                self.0.has_dirty().await
            }
            async fn flush(&self) -> ::anyhow::Result<()> {
                self.0.flush().await
            }
            async fn embed_query(&self, text: &str) -> ::anyhow::Result<Vec<f32>> {
                self.0.embed_query(text).await
            }
            async fn embed_passage(&self, text: &str) -> ::anyhow::Result<Vec<f32>> {
                self.0.embed_passage(text).await
            }
        }
    };
}

pub(crate) use impl_vectorstore_delegate;
