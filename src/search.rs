//! Shared similarity-search core used by BOTH the CLI (`similar` / `duplicates`
//! subcommands) AND the MCP server (`mcp.rs`).
//!
//! This module is **not** gated behind the `mcp` feature: the `duplicates` / `similar`
//! CLI subcommands must work with just a vector backend + embedder (e.g.
//! `--features "ollama,ast"` or `--features ort`).
//!
//! What lives here (single source of truth — never duplicated):
//! - [`UnionFind`] — disjoint-set used to cluster near-duplicate chunks.
//! - [`DupMember`] / [`DupCluster`] — the cluster result shape.
//! - [`cluster_duplicates`] — the PURE clustering algorithm (union-find over
//!   per-chunk neighbour lists + edge bookkeeping + sort/truncate).
//! - [`find_duplicates`] / [`find_similar`] — the orchestration over a [`Backend`]
//!   (fetch chunks/vectors, gather neighbours, resolve a [`SimilarTarget`]).
//!
//! These functions take `&Backend` and therefore run ON the backend worker thread
//! ([`crate::worker`]): both the CLI subcommands and the MCP tools send a
//! `FindDuplicates` / `FindSimilar` request through the `Send`+`Sync`
//! [`crate::worker::BackendHandle`], and the worker calls into this module. The
//! backend's sync DuckDB I/O thus never blocks the main runtime, and the
//! orchestration exists in exactly one place.

#![cfg_attr(not(any(feature = "duckdb", feature = "qdrant")), allow(dead_code))]

use std::collections::{HashMap, HashSet};

use anyhow::Result;

use crate::vectordbs::{Backend, Hit};
// Transitional re-export shim (US-001): the cluster result shapes + the `find_similar`
// target now live in `crate::domain`. Re-exported so existing call sites importing them
// via `crate::search::…` keep resolving without churn. Removed in a later story.
pub use crate::domain::{DupCluster, DupMember, SimilarTarget};

/// Cluster near-duplicate chunks from per-chunk nearest-neighbour lists (PURE — no I/O).
///
/// `chunks` is every stored chunk (paired with its vector — the vector is unused here
/// but kept so callers can pass the exact `all_chunks_with_vectors` shape). `neighbours`
/// is parallel to `chunks`: `neighbours[i]` is the nearest-neighbour hits of `chunks[i]`
/// (self already excluded by the backend). An edge is kept when its similarity
/// `>= min_score`; kept edges union the two chunks. Clusters with `>= min_cluster_size`
/// members are returned, largest first (tie-break: higher `max_sim`), truncated to
/// `max_clusters`.
///
/// Both the CLI handlers and the MCP `find_duplicates` tool call this with the chunks +
/// neighbours they each gathered, so the union-find lives in exactly one place.
///
/// `seed_paths`, when `Some`, restricts which chunks may SEED a cluster to those whose
/// path is in the set (e.g. the files a PR changed). Neighbour search still spans the
/// whole DB, so a seeded chunk still clusters with the EXISTING code it duplicates — the
/// restriction only stops untouched chunks from seeding. This turns the scan into "does
/// the changed code duplicate anything already indexed?", which a count-delta gate misses
/// when new slop joins an already-existing cluster. `None` = the whole-DB scan.
pub fn cluster_duplicates(
    chunks: &[(Hit, Vec<f32>)],
    neighbours: &[Vec<Hit>],
    min_score: f32,
    min_cluster_size: usize,
    max_clusters: usize,
    seed_paths: Option<&HashSet<String>>,
) -> Vec<DupCluster> {
    let n = chunks.len();
    // Stable index per chunk id for union-find.
    let mut id_to_idx = HashMap::with_capacity(n);
    for (i, (hit, _)) in chunks.iter().enumerate() {
        id_to_idx.insert(hit.id, i);
    }

    let mut uf = UnionFind::new(n);
    // edges keyed by ordered (a,b) idx pair → best similarity, deduped.
    let mut edges: HashMap<(usize, usize), f32> = HashMap::new();

    for (i, nbrs) in neighbours.iter().enumerate() {
        // A chunk that opted out of clustering forms no edges as a seed.
        if chunks[i].0.no_duplicate {
            continue;
        }
        // With a seed set, only the listed (e.g. PR-changed) chunks may seed a cluster.
        // This is applied to the SEED only, never the neighbour `j` below, so a seeded
        // chunk still pulls in the untouched code it duplicates.
        if let Some(seeds) = seed_paths
            && !seeds.contains(&chunks[i].0.path)
        {
            continue;
        }
        for nb in nbrs {
            if nb.score < min_score {
                continue;
            }
            let Some(&j) = id_to_idx.get(&nb.id) else {
                continue;
            };
            // …and is never picked up as a neighbour either.
            if chunks[j].0.no_duplicate {
                continue;
            }
            if i == j {
                continue;
            }
            let key = if i < j { (i, j) } else { (j, i) };
            let e = edges.entry(key).or_insert(nb.score);
            if nb.score > *e {
                *e = nb.score;
            }
            uf.union(i, j);
        }
    }

    // Group chunk indices by union-find root.
    let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..n {
        groups.entry(uf.find(i)).or_default().push(i);
    }

    let mut clusters: Vec<DupCluster> = groups
        .into_values()
        .filter(|members| members.len() >= min_cluster_size)
        .map(|members| build_cluster(chunks, &members, &edges))
        .collect();
    // Largest clusters first; tie-break by max_sim desc for determinism.
    // Use total_cmp for a total order (handles NaN / -0.0 safely; MSRV 1.85 supports it).
    clusters.sort_by(|a, b| b.size.cmp(&a.size).then(b.max_sim.total_cmp(&a.max_sim)));
    clusters.truncate(max_clusters);
    clusters
}

/// Build a cluster summary from member indices and the deduped edge map.
fn build_cluster(
    chunks: &[(Hit, Vec<f32>)],
    members: &[usize],
    edges: &HashMap<(usize, usize), f32>,
) -> DupCluster {
    let member_set: HashSet<usize> = members.iter().copied().collect();
    let mut min_sim = f32::MAX;
    let mut max_sim = f32::MIN;
    for (&(a, b), &s) in edges {
        if member_set.contains(&a) && member_set.contains(&b) {
            min_sim = min_sim.min(s);
            max_sim = max_sim.max(s);
        }
    }
    // A lone-edge cluster has at least one edge; guard the degenerate case anyway.
    if min_sim == f32::MAX {
        min_sim = 0.0;
    }
    if max_sim == f32::MIN {
        max_sim = 0.0;
    }
    let mut member_rows: Vec<DupMember> = members
        .iter()
        .map(|&i| {
            let h = &chunks[i].0;
            DupMember {
                path: h.path.clone(),
                start_line: h.start_line,
                end_line: h.end_line,
                symbol: h.symbol.clone(),
            }
        })
        .collect();
    // Deterministic member order.
    member_rows.sort_by(|a, b| a.path.cmp(&b.path).then(a.start_line.cmp(&b.start_line)));
    DupCluster {
        size: members.len(),
        members: member_rows,
        min_sim,
        max_sim,
    }
}

/// Run the full codebase-wide near-duplicate scan off the [`Backend`] enum. Fetches
/// every stored chunk (optionally path-glob filtered), gathers each chunk's `top_k`
/// nearest neighbours (self-excluded, stored vectors — no re-embed), then defers to
/// the shared [`cluster_duplicates`].
///
/// Runs on the backend worker thread: BOTH the CLI `duplicates` subcommand and the
/// MCP `sai_find_duplicates` tool reach this through a `FindDuplicates` request on
/// [`crate::worker::BackendHandle`].
///
/// `seed_paths` (see [`cluster_duplicates`]) restricts which chunks may seed a cluster.
/// As an optimisation it also skips the nearest-neighbour query for non-seed chunks —
/// they can still be PULLED IN as a seed's neighbour, but never seed a cluster themselves,
/// so their own neighbour list is never consulted. A PR gate thus does O(changed) vector
/// queries instead of O(whole index).
pub async fn find_duplicates(
    backend: &Backend,
    min_score: f32,
    min_cluster_size: usize,
    top_k: u64,
    max_clusters: usize,
    path_glob: Option<&str>,
    seed_paths: Option<&HashSet<String>>,
) -> Result<Vec<DupCluster>> {
    let chunks = backend.all_chunks_with_vectors(path_glob).await?;
    let mut neighbours: Vec<Vec<Hit>> = Vec::with_capacity(chunks.len());
    for (hit, vec) in &chunks {
        // Only seed chunks need their neighbours; skip the query for the rest.
        if seed_paths.is_some_and(|seeds| !seeds.contains(&hit.path)) {
            neighbours.push(Vec::new());
            continue;
        }
        let nbrs = backend.query_by_vector(vec, top_k, Some(hit.id)).await?;
        neighbours.push(nbrs);
    }
    Ok(cluster_duplicates(
        &chunks,
        &neighbours,
        min_score,
        min_cluster_size.max(1),
        max_clusters,
        seed_paths,
    ))
}

/// Resolve a `find_similar` request off the [`Backend`] enum into ranked neighbours,
/// applying `min_score`. Runs on the backend worker thread (the CLI `similar`
/// subcommand sends a `FindSimilar` request through [`crate::worker::BackendHandle`]).
///
/// - [`SimilarTarget::Code`] embeds the snippet as a PASSAGE then NN-searches by it.
/// - [`SimilarTarget::Location`] looks up the stored chunk + its exact vector and
///   NN-searches by that vector, excluding the chunk itself.
///
/// `min_score` drops neighbours below the cosine cut (pass `0.0` to see the raw
/// distribution). Returns the ranked, filtered hits.
pub async fn find_similar(
    backend: &Backend,
    target: SimilarTarget,
    limit: u64,
    min_score: f32,
) -> Result<Vec<Hit>> {
    let hits = match target {
        SimilarTarget::Code(code) => {
            let vec = backend.embed_passage(&code).await?;
            backend.query_by_vector(&vec, limit, None).await?
        }
        SimilarTarget::Location { path, line } => {
            let located = backend.get_by_location(&path, line).await?;
            let (hit, vec) =
                located.ok_or_else(|| anyhow::anyhow!("no indexed chunk at {path}:{line}"))?;
            backend.query_by_vector(&vec, limit, Some(hit.id)).await?
        }
    };
    Ok(hits.into_iter().filter(|h| h.score >= min_score).collect())
}

/// Classic union-find (disjoint set) with path compression + union by size. Used to
/// cluster near-duplicate chunks from the pairwise NN edges. Single source of truth
/// shared by the CLI and MCP find_duplicates paths.
pub struct UnionFind {
    parent: Vec<usize>,
    size: Vec<usize>,
}

impl UnionFind {
    pub fn new(n: usize) -> Self {
        Self {
            parent: (0..n).collect(),
            size: vec![1; n],
        }
    }

    pub fn find(&mut self, x: usize) -> usize {
        let mut root = x;
        while self.parent[root] != root {
            root = self.parent[root];
        }
        // Path compression.
        let mut cur = x;
        while self.parent[cur] != root {
            let next = self.parent[cur];
            self.parent[cur] = root;
            cur = next;
        }
        root
    }

    pub fn union(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra == rb {
            return;
        }
        let (big, small) = if self.size[ra] >= self.size[rb] {
            (ra, rb)
        } else {
            (rb, ra)
        };
        self.parent[small] = big;
        self.size[big] += self.size[small];
    }
}

#[cfg(test)]
mod tests {
    //! Tests against `Backend::Mock` (seeded rows-with-vectors) for the shared core that
    //! BOTH the CLI subcommands and the MCP tools depend on: union-find clustering, the
    //! duplicates scan, and find_similar resolution (code + location, self-exclusion).

    use super::*;
    use crate::vectordbs::mock::{MockRow, seeded};

    /// find_duplicates: a tight cluster of near-identical vectors collapses into ONE
    /// component; a distinct vector stays separate; min_cluster_size filters it.
    #[tokio::test]
    async fn find_duplicates_clusters_near_identical_and_separates_distinct() {
        let b = seeded(vec![
            MockRow::new(1, "src/dup1.ts", 1, vec![1.0, 0.0, 0.0, 0.0]),
            MockRow::new(2, "src/dup2.ts", 1, vec![0.999, 0.01, 0.0, 0.0]),
            MockRow::new(3, "src/dup3.ts", 1, vec![0.998, 0.0, 0.02, 0.0]),
            MockRow::new(4, "src/other.ts", 1, vec![0.0, 0.0, 0.0, 1.0]),
        ]);
        let clusters = find_duplicates(&b, 0.95, 2, 10, 50, None, None)
            .await
            .unwrap();
        assert_eq!(clusters.len(), 1, "exactly one near-duplicate cluster");
        assert_eq!(clusters[0].size, 3, "the three near-identical chunks");
        let paths: Vec<&str> = clusters[0]
            .members
            .iter()
            .map(|m| m.path.as_str())
            .collect();
        assert!(paths.contains(&"src/dup1.ts"));
        assert!(paths.contains(&"src/dup2.ts"));
        assert!(paths.contains(&"src/dup3.ts"));
        assert!(!paths.contains(&"src/other.ts"), "outlier excluded");
        assert!(clusters[0].min_sim >= 0.95, "edge sims above threshold");
    }

    /// min_score filtering: raising the threshold above all edge similarities yields no
    /// clusters even though the vectors are somewhat close.
    #[tokio::test]
    async fn find_duplicates_min_score_filters_out_weak_edges() {
        let b = seeded(vec![
            MockRow::new(1, "src/a.ts", 1, vec![1.0, 0.0, 0.0, 0.0]),
            MockRow::new(2, "src/b.ts", 1, vec![0.7, 0.7, 0.0, 0.0]),
        ]);
        // cosine ~0.707 < 0.99 → no edges kept.
        let clusters = find_duplicates(&b, 0.99, 2, 10, 50, None, None)
            .await
            .unwrap();
        assert!(clusters.is_empty(), "no edges survive a high threshold");
    }

    /// find_similar by code: embeds the snippet (mock canned vector) and ranks neighbours.
    #[tokio::test]
    async fn find_similar_by_code_ranks_neighbours() {
        let b = seeded(vec![
            MockRow::new(1, "src/a.ts", 1, vec![1.0, 0.0, 0.0, 0.0]),
            MockRow::new(2, "src/b.ts", 1, vec![0.0, 1.0, 0.0, 0.0]),
        ]);
        // min_score 0.0 → no filtering; just assert it resolves + ranks.
        let hits = find_similar(&b, SimilarTarget::Code("anything".to_string()), 8, 0.0)
            .await
            .unwrap();
        assert!(!hits.is_empty(), "code path returns neighbours");
        assert!(
            hits.windows(2).all(|w| w[0].score >= w[1].score),
            "ranked by score desc"
        );
    }

    /// find_similar by location: reuses the stored vector and EXCLUDES the chunk itself.
    #[tokio::test]
    async fn find_similar_by_location_excludes_self() {
        let b = seeded(vec![
            MockRow::new(1, "src/a.ts", 10, vec![1.0, 0.0, 0.0, 0.0]),
            MockRow::new(2, "src/b.ts", 1, vec![0.99, 0.01, 0.0, 0.0]),
        ]);
        let hits = find_similar(
            &b,
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

    /// find_similar by location: a missing chunk is a clear error.
    #[tokio::test]
    async fn find_similar_missing_location_errors() {
        let b = seeded(vec![MockRow::new(
            1,
            "src/a.ts",
            10,
            vec![1.0, 0.0, 0.0, 0.0],
        )]);
        // `Hit` is not `Debug`, so avoid `unwrap_err()` — match the Result directly.
        let res = find_similar(
            &b,
            SimilarTarget::Location {
                path: "src/a.ts".to_string(),
                line: 999,
            },
            8,
            0.0,
        )
        .await;
        match res {
            Err(e) => assert!(e.to_string().contains("no indexed chunk"), "clear error"),
            Ok(_) => panic!("missing location must error"),
        }
    }

    /// min_score filters low-scoring neighbours out of find_similar results.
    #[tokio::test]
    async fn find_similar_applies_min_score() {
        let b = seeded(vec![
            MockRow::new(1, "src/a.ts", 1, vec![1.0, 0.0, 0.0, 0.0]),
            MockRow::new(2, "src/b.ts", 1, vec![0.0, 1.0, 0.0, 0.0]),
        ]);
        let hits = find_similar(
            &b,
            SimilarTarget::Location {
                path: "src/a.ts".to_string(),
                line: 1,
            },
            8,
            0.5,
        )
        .await
        .unwrap();
        // The only other vector is orthogonal (cosine 0 < 0.5) → filtered out.
        assert!(hits.is_empty(), "orthogonal neighbour dropped by min_score");
    }

    /// Build a minimal `Hit` for the pure `cluster_duplicates` tests.
    fn hit(id: u64, path: &str, score: f32, no_duplicate: bool) -> Hit {
        Hit {
            id,
            path: path.to_string(),
            language: "ts".to_string(),
            start_line: 1,
            end_line: 1,
            text: String::new(),
            score,
            symbol: None,
            commit_sha: None,
            dirty: false,
            no_duplicate,
        }
    }

    /// `cluster_duplicates` excludes a `no_duplicate` chunk both as a cluster seed and as a
    /// neighbour: two near-identical chunks would cluster, but flagging one drops it to a
    /// singleton, leaving no cluster of the required size.
    #[test]
    fn cluster_duplicates_excludes_no_duplicate_chunks() {
        // Sanity: WITHOUT the flag the two chunks cluster.
        let chunks = vec![
            (hit(1, "src/a.ts", 0.0, false), vec![1.0, 0.0]),
            (hit(2, "src/b.ts", 0.0, false), vec![1.0, 0.0]),
        ];
        let neighbours = vec![
            vec![hit(2, "src/b.ts", 0.99, false)],
            vec![hit(1, "src/a.ts", 0.99, false)],
        ];
        let clusters = cluster_duplicates(&chunks, &neighbours, 0.95, 2, 50, None);
        assert_eq!(clusters.len(), 1, "unflagged near-identical chunks cluster");

        // WITH chunk 2 flagged no_duplicate: it forms no edge as a seed and is skipped as a
        // neighbour of chunk 1 → no cluster of size >= 2 survives.
        let chunks = vec![
            (hit(1, "src/a.ts", 0.0, false), vec![1.0, 0.0]),
            (hit(2, "src/b.ts", 0.0, true), vec![1.0, 0.0]),
        ];
        let neighbours = vec![
            vec![hit(2, "src/b.ts", 0.99, false)],
            vec![hit(1, "src/a.ts", 0.99, false)],
        ];
        let clusters = cluster_duplicates(&chunks, &neighbours, 0.95, 2, 50, None);
        assert!(
            clusters.is_empty(),
            "a no_duplicate chunk is excluded from clustering (as seed and neighbour)"
        );
    }

    /// `seed_paths` restricts which chunks may SEED a cluster: a changed chunk still
    /// clusters with the untouched code it duplicates, but a pre-existing duplicate pair
    /// among untouched files (neither chunk seeded) never surfaces.
    #[test]
    fn cluster_duplicates_seeds_only_changed_paths() {
        // a.ts (changed) duplicates b.ts (untouched); c.ts/d.ts are an untouched
        // pre-existing duplicate pair that must NOT be reported by a seeded scan.
        let chunks = vec![
            (hit(1, "src/a.ts", 0.0, false), vec![1.0, 0.0]),
            (hit(2, "src/b.ts", 0.0, false), vec![1.0, 0.0]),
            (hit(3, "src/c.ts", 0.0, false), vec![0.0, 1.0]),
            (hit(4, "src/d.ts", 0.0, false), vec![0.0, 1.0]),
        ];
        let neighbours = vec![
            vec![hit(2, "src/b.ts", 0.99, false)],
            vec![hit(1, "src/a.ts", 0.99, false)],
            vec![hit(4, "src/d.ts", 0.99, false)],
            vec![hit(3, "src/c.ts", 0.99, false)],
        ];

        let seeds: HashSet<String> = [String::from("src/a.ts")].into_iter().collect();
        let clusters = cluster_duplicates(&chunks, &neighbours, 0.95, 2, 50, Some(&seeds));
        assert_eq!(
            clusters.len(),
            1,
            "only the changed file's duplicate surfaces"
        );
        let paths: Vec<&str> = clusters[0]
            .members
            .iter()
            .map(|m| m.path.as_str())
            .collect();
        assert!(paths.contains(&"src/a.ts"), "the changed seed is present");
        assert!(
            paths.contains(&"src/b.ts"),
            "the untouched code it duplicates is pulled in as a neighbour"
        );
        assert!(
            !paths.contains(&"src/c.ts") && !paths.contains(&"src/d.ts"),
            "the untouched pre-existing pair is not reported"
        );

        // Sanity: with no seed set the whole-DB scan still reports BOTH pairs.
        let clusters = cluster_duplicates(&chunks, &neighbours, 0.95, 2, 50, None);
        assert_eq!(
            clusters.len(),
            2,
            "whole-DB scan reports both duplicate pairs"
        );
    }

    /// End-to-end over the Mock backend: a `seed_paths` set scopes the scan to changed
    /// files while the neighbour search still spans the whole index.
    #[tokio::test]
    async fn find_duplicates_seed_paths_restricts_to_changed() {
        let b = seeded(vec![
            // "changed" file — duplicates the existing one below.
            MockRow::new(1, "src/new.ts", 1, vec![1.0, 0.0, 0.0, 0.0]),
            MockRow::new(2, "src/existing.ts", 1, vec![0.999, 0.01, 0.0, 0.0]),
            // untouched pre-existing duplicate pair.
            MockRow::new(3, "src/old_a.ts", 1, vec![0.0, 0.0, 1.0, 0.0]),
            MockRow::new(4, "src/old_b.ts", 1, vec![0.0, 0.0, 0.999, 0.01]),
        ]);
        let seeds: HashSet<String> = [String::from("src/new.ts")].into_iter().collect();
        let clusters = find_duplicates(&b, 0.95, 2, 10, 50, None, Some(&seeds))
            .await
            .unwrap();
        assert_eq!(clusters.len(), 1, "only the changed file's duplicate");
        let paths: Vec<&str> = clusters[0]
            .members
            .iter()
            .map(|m| m.path.as_str())
            .collect();
        assert!(paths.contains(&"src/new.ts"));
        assert!(paths.contains(&"src/existing.ts"));
        assert!(
            !paths.contains(&"src/old_a.ts"),
            "the untouched pre-existing pair is excluded"
        );
    }

    /// Union-find groups transitively connected indices and keeps disjoint sets apart.
    #[test]
    fn union_find_groups_transitively() {
        let mut uf = UnionFind::new(5);
        uf.union(0, 1);
        uf.union(1, 2);
        uf.union(3, 4);
        assert_eq!(uf.find(0), uf.find(2), "0-1-2 are one set");
        assert_eq!(uf.find(3), uf.find(4), "3-4 are one set");
        assert_ne!(uf.find(0), uf.find(3), "the two sets are disjoint");
    }
}
