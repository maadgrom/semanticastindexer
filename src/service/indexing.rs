//! [`IndexingService`]: the write-side service over the [`VectorStore`] port — prepare
//! storage, bulk-index a source tree, refresh changed files, and flush, dispatched over
//! `Arc<dyn VectorStore>`.
//!
//! The orchestration loops run over the `Send + Sync` port; the PURE shared bits
//! (`indexer::collect_chunks` / `indexer::build_chunks`) are CALLED, never duplicated. Both
//! the CLI (`crate::app`) and the MCP `refresh` tool reach this service.

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;

use crate::domain::Plan;
use crate::domain::{CodeChunk, IndexProgress, IndexReport, RefreshReport, ReindexOutcome};
use crate::git::GitContext;
use crate::indexer;
use crate::repos::VectorStore;

/// Batch size for embed+upsert. Bounds the embedder POST size (Ollama) and lets us emit
/// a live progress update without one giant call. Mirrors `app::UPSERT_BATCH`.
const UPSERT_BATCH: usize = 64;

/// Write-side service: prepare storage, bulk-index a source tree, refresh changed files,
/// and flush — all over the shared [`VectorStore`] port.
// Some methods are reachable only under specific features; allow dead_code so a feature
// subset doesn't trip `clippy -D warnings`.
#[allow(dead_code)]
pub struct IndexingService {
    store: Arc<dyn VectorStore>,
    plan: Plan,
}

#[allow(dead_code)]
impl IndexingService {
    pub fn new(store: Arc<dyn VectorStore>, plan: Plan) -> Self {
        Self { store, plan }
    }

    /// Prepare storage (create collection/table + indexes) if missing.
    #[tracing::instrument(level = "info", skip(self), fields(recreate))]
    pub async fn ensure_ready(&self, recreate: bool) -> Result<()> {
        self.store.ensure_ready(recreate).await
    }

    /// Walk the root, collect chunks, stamp the git context, and upsert them in batches
    /// wrapped in a begin/end_bulk window, emitting one [`IndexProgress`] per batch.
    ///
    /// MIRRORS `app::index_sources`: same `UPSERT_BATCH` size, same per-batch file-crossing
    /// count (scan every chunk path, not just the first, so a multi-file batch reports each
    /// file once), and the same end_bulk-ALWAYS invariant (a failing upsert still rebuilds
    /// the index, but the body error wins). No TTY printing here — the CLI supplies that via
    /// the `progress` closure in US-004.
    #[tracing::instrument(level = "info", skip(self, ctx, progress))]
    pub async fn index_sources(
        &self,
        ctx: &GitContext,
        progress: &mut dyn FnMut(IndexProgress),
    ) -> Result<IndexReport> {
        let (mut chunks, files, skipped) = indexer::collect_chunks(&self.plan);
        for c in &mut chunks {
            c.commit_sha = ctx.sha.clone();
            c.dirty = ctx.dirty;
        }
        let chunks_total = chunks.len();

        self.store.begin_bulk().await?;
        // begin→body→end-always: run the upsert loop, then always end_bulk; the body error
        // (if any) wins over an end_bulk error.
        let body = self
            .index_batches(chunks, files, chunks_total, progress)
            .await;
        let end = self.store.end_bulk().await;
        body?;
        end?;
        Ok(IndexReport {
            chunks: chunks_total,
            files,
            skipped,
        })
    }

    /// The batch loop of [`index_sources`], factored out so the caller can run end_bulk
    /// unconditionally afterwards. Upserts in `UPSERT_BATCH` windows and emits one
    /// [`IndexProgress`] per batch.
    async fn index_batches(
        &self,
        chunks: Vec<CodeChunk>,
        files_total: usize,
        chunks_total: usize,
        progress: &mut dyn FnMut(IndexProgress),
    ) -> Result<()> {
        let mut done = 0usize;
        let mut files_done = 0usize;
        let mut last_path: Option<String> = None;
        let mut remaining = chunks.into_iter().peekable();
        while remaining.peek().is_some() {
            let batch: Vec<CodeChunk> = remaining.by_ref().take(UPSERT_BATCH).collect();
            // Announce every distinct file as we cross into its chunks. A single batch can
            // span many files, so scan all chunks — not just the first — or the counter
            // degenerates into a batch index (mirrors app::index_sources L387-399).
            let mut batch_path = String::new();
            for c in &batch {
                if last_path.as_deref() != Some(c.path.as_str()) {
                    files_done += 1;
                    last_path = Some(c.path.clone());
                }
                batch_path = c.path.clone();
            }
            let n = batch.len();
            self.store.upsert(&batch).await?;
            done += n;
            progress(IndexProgress {
                files_done,
                files_total,
                chunks_done: done,
                chunks_total,
                path: batch_path,
            });
        }
        Ok(())
    }

    /// Re-index a batch of changed paths in one bulk window: begin_bulk → per-path (fresh
    /// git capture + delete + re-chunk + re-embed + upsert) → end_bulk-ALWAYS, breaking on
    /// the first error and returning it after the index is rebuilt. Reuses [`ReindexOutcome`].
    #[tracing::instrument(level = "info", skip(self, paths), fields(paths = paths.len()))]
    pub async fn refresh(&self, paths: &[String]) -> Result<RefreshReport> {
        self.store.begin_bulk().await?;
        let mut entries: Vec<(String, ReindexOutcome)> = Vec::with_capacity(paths.len());
        let mut first_err = None;
        for rel in paths {
            // Fresh git ctx per path for an accurate dirty/commit stamp (a long-lived
            // server's startup capture would go stale).
            let ctx = crate::git::capture();
            match self.reindex_file(rel, &ctx).await {
                Ok(outcome) => entries.push((rel.trim_start_matches("./").to_string(), outcome)),
                Err(e) => {
                    first_err = Some(e);
                    break;
                }
            }
        }
        // Always rebuild the index before returning, even on error.
        let end = self.store.end_bulk().await;
        if let Some(e) = first_err {
            return Err(e);
        }
        end?;
        Ok(RefreshReport { entries })
    }

    /// Per-file re-index step over the store: delete the file's existing points, then —
    /// if the on-disk path is still indexable — re-chunk + stamp + upsert, dispatched over
    /// `&dyn VectorStore`. The inclusion decision flows through `indexer`'s single source of
    /// truth (`load_file_for_indexing` + `build_chunks`).
    async fn reindex_file(&self, rel: &str, ctx: &GitContext) -> Result<ReindexOutcome> {
        let key = rel.trim_start_matches("./");
        // Always remove the file's existing points first.
        self.store.delete_by_path(key).await?;

        let path = Path::new(rel);
        if !path.is_file() {
            return Ok(ReindexOutcome::Removed { reason: "removed" });
        }

        match indexer::load_file_for_indexing(&self.plan, path, key, None) {
            Some(raw) => {
                let mut file_chunks = indexer::build_chunks(&self.plan, path, key, &raw);
                if file_chunks.is_empty() {
                    return Ok(ReindexOutcome::Removed {
                        reason: "removed: no indexable content",
                    });
                }
                for c in &mut file_chunks {
                    c.commit_sha = ctx.sha.clone();
                    c.dirty = ctx.dirty;
                }
                let n = file_chunks.len();
                self.store.upsert(&file_chunks).await?;
                Ok(ReindexOutcome::Reindexed { chunks: n })
            }
            None => {
                if self.plan.skip_generated
                    && let Ok(raw) = std::fs::read_to_string(path)
                    && indexer::is_generated(&raw)
                {
                    return Ok(ReindexOutcome::Removed {
                        reason: "removed: autogenerated",
                    });
                }
                Ok(ReindexOutcome::Removed { reason: "removed" })
            }
        }
    }

    /// Drop all stored vectors (delete collection/table).
    pub async fn flush(&self) -> Result<()> {
        self.store.flush().await
    }
}

#[cfg(test)]
mod tests {
    //! In-process, NO-thread, NO-network: drive `IndexingService` over an `Arc<MockStore>`
    //! and assert the begin/upsert/end orchestration + emitted progress mirror the original
    //! `app::index_sources`.

    use std::fs;
    use std::sync::{Arc, Mutex};

    use tempfile::TempDir;

    use super::*;
    use crate::domain::IndexProgress;
    use crate::domain::Plan;
    use crate::domain::PrefixStyle;
    use crate::repos::mock::MockStore;
    use crate::vectordbs::mock::{MockBackend, MockCalls};

    /// A `Plan` rooted at `root` mirroring `app::tests::test_plan` (mock/ort, no globs,
    /// E5/e5-small knobs) so `collect_chunks` produces real chunks from the temp tree.
    fn test_plan(root: &str) -> Plan {
        Plan {
            root: root.to_string(),
            prefix_style: PrefixStyle::E5,
            max_chunk_chars: 1400,
            collection: "test_coll".to_string(),
            model: "intfloat/multilingual-e5-small".to_string(),
            vector_dim: 384,
            model_repo: "Xenova/multilingual-e5-small".to_string(),
            ..crate::config::test_support::minimal_plan()
        }
    }

    /// index_sources over a `MockStore`: exactly one begin_bulk + one end_bulk, upsert
    /// fires, the upserted chunk count matches `collect_chunks`, and the LAST emitted
    /// `IndexProgress` reports `chunks_done == chunks_total`.
    #[tokio::test]
    async fn index_sources_drives_bulk_window_and_emits_progress() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_string_lossy().to_string();
        fs::write(
            dir.path().join("alpha.ts"),
            "export function alpha() { return 1 }\nconst x = alpha()\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("beta.ts"),
            "export const beta = () => 2\nconsole.log(beta())\n",
        )
        .unwrap();

        let plan = test_plan(&root);
        let (expected_chunks, expected_files, _) = indexer::collect_chunks(&plan);
        let chunks_total = expected_chunks.len();
        assert!(chunks_total > 0, "fixture must produce chunks");

        // Keep a handle to the recorder before the backend moves into the Arc<dyn VectorStore>.
        let backend = MockBackend::new();
        let calls: Arc<Mutex<MockCalls>> = backend.calls.clone();
        let store = Arc::new(MockStore(backend));
        let svc = IndexingService::new(store, plan);

        let mut progress: Vec<IndexProgress> = Vec::new();
        let report = svc
            .index_sources(&GitContext::default(), &mut |p| progress.push(p))
            .await
            .unwrap();

        let calls = calls.lock().unwrap();
        assert_eq!(calls.begin_bulk, 1, "exactly one begin_bulk");
        assert_eq!(calls.end_bulk, 1, "exactly one end_bulk");
        assert!(!calls.upserts.is_empty(), "upsert was called");
        assert_eq!(
            calls.total_upserted_chunks(),
            chunks_total,
            "upserted count matches collect_chunks"
        );

        assert_eq!(report.chunks, chunks_total, "report chunk total");
        assert_eq!(report.files, expected_files, "report file count");

        let last = progress.last().expect("at least one progress emit");
        assert_eq!(
            last.chunks_done, chunks_total,
            "final progress reports all chunks done"
        );
        assert_eq!(last.chunks_total, chunks_total);
    }
}
