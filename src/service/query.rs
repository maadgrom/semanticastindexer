//! [`QueryService`]: the read-side service over the [`VectorStore`] port — text query,
//! find-similar, and the near-duplicate scan, dispatched over `Arc<dyn VectorStore>`.
//!
//! The orchestration loops run over the `Send + Sync` port; the PURE shared clustering core
//! (`search::cluster_duplicates`, union-find) is CALLED, never duplicated. Both the CLI
//! `duplicates`/`similar` subcommands and the MCP tools reach this service.

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Result;

use crate::domain::Plan;
use crate::domain::{DupCluster, Hit, SimilarTarget};
use crate::repos::VectorStore;
use crate::service::impl_service_new;

/// Read-side service: text query, find-similar, and the near-duplicate scan — all over the
/// shared [`VectorStore`] port. The B1 dedup gate depends on `find_duplicates` carrying
/// `seed_paths`.
// Some methods are reachable only under specific features (e.g. `chunk_count` on the MCP
// path); allow dead_code so a feature subset doesn't trip `clippy -D warnings`.
#[allow(dead_code)]
pub struct QueryService {
    store: Arc<dyn VectorStore>,
    plan: Plan,
}

impl_service_new!(QueryService);

#[allow(dead_code)]
impl QueryService {
    /// Nearest-neighbour search by query text. The store embeds-or-server-queries
    /// internally (matching the backends today).
    #[tracing::instrument(level = "info", skip(self, q), fields(q = %q.chars().take(80).collect::<String>(), limit))]
    pub async fn query(&self, q: &str, limit: u64) -> Result<Vec<Hit>> {
        self.store.query(q, limit).await
    }

    /// Total stored chunk count (drives the MCP `index_status` tool). Delegates to the store.
    #[tracing::instrument(level = "info", skip(self))]
    pub async fn chunk_count(&self) -> Result<u64> {
        self.store.chunk_count().await
    }

    /// Best-effort check for any dirty-stamped (uncommitted) chunks. Delegates to the store;
    /// backends without the column (or on error) report `false`. Drives the CLI `duplicates`
    /// dirty-tree warning (mirrors `app::warn_on_dirty`).
    pub async fn has_dirty(&self) -> Result<bool> {
        self.store.has_dirty().await
    }

    /// Resolve a `find_similar` request into ranked neighbours, applying `min_score`.
    /// MIRRORS `search::find_similar`:
    /// - [`SimilarTarget::Code`] embeds the snippet as a PASSAGE then NN-searches by it.
    /// - [`SimilarTarget::Location`] looks up the stored chunk + its exact vector and
    ///   NN-searches by that vector, EXCLUDING the chunk itself.
    #[tracing::instrument(level = "info", skip(self, target), fields(limit, min_score))]
    pub async fn find_similar(
        &self,
        target: SimilarTarget,
        limit: u64,
        min_score: f32,
    ) -> Result<Vec<Hit>> {
        let hits = match target {
            SimilarTarget::Code(code) => {
                let vec = self.store.embed_passage(&code).await?;
                self.store.query_by_vector(&vec, limit, None).await?
            }
            SimilarTarget::Location { path, line } => {
                let located = self.store.get_by_location(&path, line).await?;
                let (hit, vec) =
                    located.ok_or_else(|| anyhow::anyhow!("no indexed chunk at {path}:{line}"))?;
                self.store
                    .query_by_vector(&vec, limit, Some(hit.id))
                    .await?
            }
        };
        Ok(hits.into_iter().filter(|h| h.score >= min_score).collect())
    }

    /// Run the codebase-wide near-duplicate scan. MIRRORS `search::find_duplicates`: fetch
    /// every stored chunk (optionally path-glob filtered), gather each chunk's `top_k`
    /// nearest neighbours (self-excluded, stored vectors — no re-embed), then defer to the
    /// SHARED [`crate::search::cluster_duplicates`] (union-find — NOT duplicated here).
    ///
    /// `seed_paths` (B1 — the dedup gate depends on it) restricts which chunks may SEED a
    /// cluster, and is applied as the SAME optimisation as the original: a non-seed chunk
    /// skips its neighbour query (pushes an empty list) — it can still be pulled in as a
    /// seed's neighbour, but never seeds itself. The set is then passed through to
    /// `cluster_duplicates` so the seed restriction holds in the clustering too.
    #[tracing::instrument(
        level = "info",
        skip(self, seed_paths),
        fields(min_score, min_cluster_size, top_k, max_clusters, seeded = seed_paths.is_some())
    )]
    pub async fn find_duplicates(
        &self,
        min_score: f32,
        min_cluster_size: usize,
        top_k: u64,
        max_clusters: usize,
        path_glob: Option<String>,
        seed_paths: Option<HashSet<String>>,
    ) -> Result<Vec<DupCluster>> {
        let chunks = self
            .store
            .all_chunks_with_vectors(path_glob.as_deref())
            .await?;
        let mut neighbours: Vec<Vec<Hit>> = Vec::with_capacity(chunks.len());
        for (hit, vec) in &chunks {
            // Only seed chunks need their neighbours; skip the query for the rest.
            if seed_paths.as_ref().is_some_and(|s| !s.contains(&hit.path)) {
                neighbours.push(Vec::new());
                continue;
            }
            let nbrs = self.store.query_by_vector(vec, top_k, Some(hit.id)).await?;
            neighbours.push(nbrs);
        }
        Ok(crate::search::cluster_duplicates(
            &chunks,
            &neighbours,
            min_score,
            min_cluster_size.max(1),
            max_clusters,
            seed_paths.as_ref(),
        ))
    }
}

#[cfg(test)]
mod tests {
    //! In-process, NO-thread, NO-network: drive `QueryService` over an `Arc<MockStore>`
    //! seeded with rows-with-vectors, asserting the find_* orchestration mirrors
    //! `search::find_*` (cluster membership, seed restriction, location self-exclusion).

    use std::sync::Arc;

    use super::*;
    use crate::repos::mock::MockStore;
    use crate::vectordbs::mock::{MockBackend, MockRow};

    fn service(rows: Vec<MockRow>) -> QueryService {
        let store = Arc::new(MockStore(MockBackend::with_rows(rows)));
        QueryService::new(store, crate::config::test_support::minimal_plan())
    }

    /// find_duplicates over an obvious near-identical pair: a cluster containing BOTH paths.
    #[tokio::test]
    async fn find_duplicates_clusters_near_identical_pair() {
        let svc = service(vec![
            MockRow::new(1, "src/dup1.ts", 1, vec![1.0, 0.0, 0.0, 0.0]),
            MockRow::new(2, "src/dup2.ts", 1, vec![0.999, 0.01, 0.0, 0.0]),
            MockRow::new(3, "src/other.ts", 1, vec![0.0, 0.0, 0.0, 1.0]),
        ]);
        let clusters = svc
            .find_duplicates(0.95, 2, 10, 50, None, None)
            .await
            .unwrap();
        assert_eq!(clusters.len(), 1, "exactly one near-duplicate cluster");
        let paths: Vec<&str> = clusters[0]
            .members
            .iter()
            .map(|m| m.path.as_str())
            .collect();
        assert!(paths.contains(&"src/dup1.ts"));
        assert!(paths.contains(&"src/dup2.ts"));
        assert!(!paths.contains(&"src/other.ts"), "outlier excluded");
    }

    /// find_duplicates with `seed_paths` = {one path}: only that path seeds. The seed still
    /// clusters with the untouched code it duplicates, but an untouched-only pre-existing
    /// pair (neither seeded) does NOT surface.
    #[tokio::test]
    async fn find_duplicates_honors_seed_paths() {
        let rows = vec![
            // "changed" file — duplicates the untouched one below.
            MockRow::new(1, "src/new.ts", 1, vec![1.0, 0.0, 0.0, 0.0]),
            MockRow::new(2, "src/existing.ts", 1, vec![0.999, 0.01, 0.0, 0.0]),
            // untouched pre-existing duplicate pair (neither seeded).
            MockRow::new(3, "src/old_a.ts", 1, vec![0.0, 0.0, 1.0, 0.0]),
            MockRow::new(4, "src/old_b.ts", 1, vec![0.0, 0.0, 0.999, 0.01]),
        ];
        let seeds: HashSet<String> = [String::from("src/new.ts")].into_iter().collect();

        let svc = service(rows.clone());
        let clusters = svc
            .find_duplicates(0.95, 2, 10, 50, None, Some(seeds))
            .await
            .unwrap();
        assert_eq!(
            clusters.len(),
            1,
            "only the seeded file's duplicate surfaces"
        );
        let paths: Vec<&str> = clusters[0]
            .members
            .iter()
            .map(|m| m.path.as_str())
            .collect();
        assert!(paths.contains(&"src/new.ts"), "the seed is present");
        assert!(
            paths.contains(&"src/existing.ts"),
            "the untouched code it duplicates is pulled in as a neighbour"
        );
        assert!(
            !paths.contains(&"src/old_a.ts") && !paths.contains(&"src/old_b.ts"),
            "the untouched-only pre-existing pair does NOT seed"
        );

        // Sanity: the whole-DB scan (no seeds) reports BOTH pairs.
        let svc = service(rows);
        let clusters = svc
            .find_duplicates(0.95, 2, 10, 50, None, None)
            .await
            .unwrap();
        assert_eq!(clusters.len(), 2, "whole-DB scan reports both pairs");
    }

    /// find_similar by Location: the located chunk's own id is absent from results
    /// (self-exclusion via `query_by_vector(exclude_id)`).
    #[tokio::test]
    async fn find_similar_location_excludes_self() {
        let svc = service(vec![
            MockRow::new(1, "src/a.ts", 10, vec![1.0, 0.0, 0.0, 0.0]),
            MockRow::new(2, "src/b.ts", 1, vec![0.99, 0.01, 0.0, 0.0]),
        ]);
        let hits = svc
            .find_similar(
                SimilarTarget::Location {
                    path: "src/a.ts".to_string(),
                    line: 10,
                },
                8,
                0.0,
            )
            .await
            .unwrap();
        assert!(hits.iter().all(|h| h.id != 1), "self id excluded");
        assert_eq!(hits.len(), 1, "only the other chunk");
        assert_eq!(hits[0].id, 2);
    }
}
