//! Shared PURE near-duplicate clustering core, called by [`crate::service::QueryService`]
//! (which both the CLI `duplicates` subcommand and the MCP `find_duplicates` tool reach).
//!
//! This module is **not** gated behind the `mcp` feature: the `duplicates` / `similar`
//! CLI subcommands must work with just a vector backend + embedder (e.g.
//! `--features "ollama,ast"` or `--features ort`).
//!
//! What lives here (single source of truth — never duplicated):
//! - [`UnionFind`] — disjoint-set used to cluster near-duplicate chunks.
//! - [`DupMember`] / [`DupCluster`] — the cluster result shape.
//! - [`cluster_duplicates`] — the PURE clustering algorithm (union-find over
//!   per-chunk neighbour lists + edge bookkeeping + sort/truncate). NO I/O — the service
//!   gathers chunks + neighbours from the `VectorStore` and feeds them here.

#![cfg_attr(not(any(feature = "duckdb", feature = "qdrant")), allow(dead_code))]

use std::collections::{HashMap, HashSet};

use crate::domain::Hit;
use crate::domain::{DupCluster, DupMember};

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
    //! Tests for the PURE clustering core (`cluster_duplicates` + `UnionFind`) that the
    //! `QueryService::find_duplicates` path feeds. The store-backed scan + `find_similar`
    //! resolution are covered by the `crate::service::query` unit tests (over `MockStore`).

    use super::*;

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
