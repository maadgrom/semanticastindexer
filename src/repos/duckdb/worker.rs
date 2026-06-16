//! The DuckDb worker thread: owns the `Send`-but-`!Sync` [`DuckDbBackend`] FOREVER on a
//! dedicated thread with a current-thread Tokio runtime, and serves type-erased [`Job`]
//! closures off a bounded mpsc channel one at a time. This is the closure-mailbox the
//! plan calls for — NO `Request` enum, NO match arms (cf. the old `worker::worker_loop`).
//!
//! Mirrors `worker::spawn`'s thread/runtime setup (named thread, current-thread rt) so the
//! backend's async methods — the Ollama embedder's `reqwest` calls, the synchronous DuckDB
//! connection — run on the thread that owns the backend and the backend never moves.

use std::future::Future;
use std::pin::Pin;

use anyhow::Result;
use tokio::sync::mpsc;

use crate::vectordbs::duckdb::DuckDbBackend;

/// Bounded channel depth. Jobs are processed sequentially by the single worker, so a
/// small buffer is plenty and bounds memory under a burst of calls. Matches the old
/// `worker::spawn` channel(32).
const MAILBOX_DEPTH: usize = 32;

/// A type-erased unit of work: borrow the worker-owned backend for the duration of the
/// returned future, run it to completion, and reply on its captured oneshot. The HRTB
/// (`for<'a>`) ties the backend borrow to the future so the `!Sync` backend never escapes
/// the worker thread. `+ Send` is required to move the boxed closure across the channel.
pub type Job =
    Box<dyn for<'a> FnOnce(&'a DuckDbBackend) -> Pin<Box<dyn Future<Output = ()> + 'a>> + Send>;

/// Spawn the worker thread that owns `backend` forever. Returns the job sender and the
/// thread `JoinHandle`; dropping every sender ends the loop, then `join()` waits for the
/// backend to drop (so the DuckDB connection checkpoints its WAL before exit).
pub fn spawn_worker(
    backend: DuckDbBackend,
) -> Result<(mpsc::Sender<Job>, std::thread::JoinHandle<()>)> {
    let (tx, rx) = mpsc::channel::<Job>(MAILBOX_DEPTH);
    let thread = std::thread::Builder::new()
        .name("sai-duckdb-store".to_string())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::error!(error = %e, "duckdb store worker: failed to build runtime");
                    return;
                }
            };
            rt.block_on(worker_loop(backend, rx));
        })
        .map_err(|e| anyhow::anyhow!("failed to spawn duckdb store worker thread: {e}"))?;
    Ok((tx, thread))
}

/// Own the backend; run each job fully (including its `.await`s) before taking the next,
/// so the single DuckDB connection is never touched concurrently. The loop ends when the
/// last sender drops.
async fn worker_loop(backend: DuckDbBackend, mut rx: mpsc::Receiver<Job>) {
    while let Some(job) = rx.recv().await {
        job(&backend).await;
    }
}
