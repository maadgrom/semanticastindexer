//! [`DuckDbStore`]: the [`VectorStore`] adapter that confines the `Send`-but-`!Sync`
//! [`DuckDbBackend`] to a dedicated worker thread and exposes a `Send + Sync` async facade
//! over it. The store holds only an mpsc [`Sender<Job>`](worker::Job); every method clones
//! its borrowed args to owned values (B7) and ships a closure to the worker, which borrows
//! the backend, runs the call, and replies on a oneshot. NO `Request` enum, NO `worker_loop`
//! match arms — that is the whole point of the closure-mailbox.

mod worker;

use std::future::Future;
use std::pin::Pin;

use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot};

use crate::domain::{CodeChunk, Hit};
use crate::repos::VectorStore;
use crate::vectordbs::duckdb::DuckDbBackend;

use worker::Job;

/// A `Send + Sync` handle to a worker-owned [`DuckDbBackend`]. Cloneable senders are not
/// exposed; the single store owns the only sender, so dropping the store ends the worker.
pub struct DuckDbStore {
    tx: mpsc::Sender<Job>,
}

/// The worker thread has ended (its receiver/sender dropped) — surfaced when a `call` can
/// neither be sent nor awaited.
fn worker_gone() -> anyhow::Error {
    anyhow::anyhow!("duckdb worker thread is no longer running")
}

impl DuckDbStore {
    /// Move a (`Send`) [`DuckDbBackend`] onto a dedicated thread that owns it forever; return
    /// a `Send + Sync` handle plus the thread's `JoinHandle`. Mirrors `worker::spawn` but
    /// carries type-erased closures, not a `Request` enum.
    pub fn spawn(backend: DuckDbBackend) -> Result<(Self, std::thread::JoinHandle<()>)> {
        let (tx, thread) = worker::spawn_worker(backend)?;
        Ok((Self { tx }, thread))
    }

    /// Ship `f` to the worker, which borrows the backend for the call's lifetime, runs it,
    /// and replies on a oneshot. `f` is `Send + 'static` (it captures only owned args); the
    /// HRTB (`for<'a>`) ties the backend borrow to the produced future so the `!Sync`
    /// backend never enters the returned (`Send`) future.
    async fn call<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: for<'a> FnOnce(&'a DuckDbBackend) -> Pin<Box<dyn Future<Output = Result<T>> + 'a>>
            + Send
            + 'static,
    {
        let (reply_tx, reply_rx) = oneshot::channel::<Result<T>>();
        let job: Job = Box::new(move |b| {
            Box::pin(async move {
                let _ = reply_tx.send(f(b).await);
            })
        });
        self.tx.send(job).await.map_err(|_| worker_gone())?;
        reply_rx.await.map_err(|_| worker_gone())?
    }
}

#[async_trait]
impl VectorStore for DuckDbStore {
    async fn ensure_ready(&self, recreate: bool) -> Result<()> {
        self.call(move |b| Box::pin(b.ensure_ready(recreate))).await
    }

    async fn begin_bulk(&self) -> Result<()> {
        self.call(|b| Box::pin(b.begin_bulk())).await
    }

    async fn end_bulk(&self) -> Result<()> {
        self.call(|b| Box::pin(b.end_bulk())).await
    }

    async fn upsert(&self, chunks: &[CodeChunk]) -> Result<()> {
        let chunks = chunks.to_vec();
        self.call(move |b| Box::pin(async move { b.upsert(&chunks).await }))
            .await
    }

    async fn delete_by_path(&self, path: &str) -> Result<()> {
        let path = path.to_string();
        self.call(move |b| Box::pin(async move { b.delete_by_path(&path).await }))
            .await
    }

    async fn query(&self, q: &str, limit: u64) -> Result<Vec<Hit>> {
        let q = q.to_string();
        self.call(move |b| Box::pin(async move { b.query(&q, limit).await }))
            .await
    }

    async fn query_by_vector(
        &self,
        v: &[f32],
        limit: u64,
        exclude_id: Option<u64>,
    ) -> Result<Vec<Hit>> {
        let v = v.to_vec();
        self.call(move |b| Box::pin(async move { b.query_by_vector(&v, limit, exclude_id).await }))
            .await
    }

    async fn get_by_location(&self, path: &str, line: usize) -> Result<Option<(Hit, Vec<f32>)>> {
        let path = path.to_string();
        self.call(move |b| Box::pin(async move { b.get_by_location(&path, line).await }))
            .await
    }

    async fn all_chunks_with_vectors(
        &self,
        path_glob: Option<&str>,
    ) -> Result<Vec<(Hit, Vec<f32>)>> {
        let path_glob = path_glob.map(str::to_owned);
        self.call(move |b| {
            Box::pin(async move { b.all_chunks_with_vectors(path_glob.as_deref()).await })
        })
        .await
    }

    async fn chunk_count(&self) -> Result<u64> {
        self.call(|b| Box::pin(b.chunk_count())).await
    }

    async fn has_dirty(&self) -> Result<bool> {
        self.call(|b| Box::pin(b.has_dirty())).await
    }

    async fn flush(&self) -> Result<()> {
        self.call(|b| Box::pin(b.flush())).await
    }

    async fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        let text = text.to_string();
        self.call(move |b| Box::pin(async move { b.embed_query(&text).await }))
            .await
    }

    async fn embed_passage(&self, text: &str) -> Result<Vec<f32>> {
        let text = text.to_string();
        self.call(move |b| Box::pin(async move { b.embed_passage(&text).await }))
            .await
    }
}

// Compile-assert: the whole point of the closure-mailbox is that the `!Sync` backend is
// confined to the worker, so the handle is `Send + Sync` and shareable as `Arc<dyn …>`.
const _: () = {
    fn assert_send_sync<T: Send + Sync>() {}
    let _ = assert_send_sync::<DuckDbStore>;
};

#[cfg(all(test, feature = "ort"))]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// End-to-end smoke of the closure-mailbox over the REAL worker thread + a real ort
    /// embedder: build a `DuckDbBackend` over a tempdir, `spawn` the store, then round-trip
    /// `ensure_ready` + `chunk_count` THROUGH the channel. Proves the worker thread + the
    /// HRTB `call` end-to-end (uses the HF cache for the e5-small model); it asserts plumbing
    /// (Ok + a count), NOT embedding scores. Drops the store, then joins the thread.
    #[tokio::test]
    async fn duckdb_store_round_trips_through_the_mailbox() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("smoke.duckdb");
        // ort embedder over the e5-small (384-d) repo — same defaults the duckdb tests use.
        let mut plan = crate::config::test_support::duckdb_plan(&db_path, 384);
        plan.embedder = "ort".to_string();
        let embedder = crate::vectordbs::embedder::Embedder::Ort(
            crate::vectordbs::embedder::ort_embedder(&plan)
                .expect("ort embedder builds (downloads/uses HF cache)"),
        );
        let backend =
            DuckDbBackend::connect(&plan, embedder).expect("duckdb backend opens on a tempdir");

        let (store, thread) = DuckDbStore::spawn(backend).expect("worker thread spawns");
        store
            .ensure_ready(false)
            .await
            .expect("ensure_ready round-trips through the mailbox");
        let count = store
            .chunk_count()
            .await
            .expect("chunk_count round-trips through the mailbox");
        assert_eq!(count, 0, "a freshly-created table has no chunks");

        drop(store);
        thread.join().expect("worker thread joins cleanly");
    }
}
