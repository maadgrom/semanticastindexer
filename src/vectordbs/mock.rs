//! In-memory backend for happy-path flow tests (`#[cfg(test)]` only — never ships
//! in a release binary). It runs the REAL orchestration code (`index_sources`,
//! `sync`, `run_query`, `flush`) with NO network and NO real Qdrant/DuckDB, and
//! RECORDS every backend call so tests can assert ordering, balance, and args.

use std::sync::{Arc, Mutex};

use anyhow::Result;

use crate::domain::Hit;

/// Generate a trivial recording method on [`MockBackend`] that bumps the named [`MockCalls`]
/// counter and returns `Ok(())`. `begin_bulk`/`end_bulk`/`flush` differ only by which counter
/// they touch, so one definition keeps them DRY (and out of the near-duplicate index).
macro_rules! record_unit_call {
    ($method:ident, $counter:ident) => {
        pub async fn $method(&self) -> Result<()> {
            self.calls.lock().unwrap().$counter += 1;
            Ok(())
        }
    };
}

/// A stored row in the mock vector store: a `Hit` (without the score) plus its vector.
/// Used by the MCP-path methods (`query_by_vector`, `get_by_location`,
/// `all_chunks_with_vectors`) so tool logic can be tested with NO real backend.
#[derive(Clone)]
pub struct MockRow {
    pub id: u64,
    pub path: String,
    pub language: String,
    pub start_line: usize,
    pub end_line: usize,
    pub text: String,
    pub symbol: Option<String>,
    pub vector: Vec<f32>,
    pub commit_sha: Option<String>,
    pub dirty: bool,
    pub no_duplicate: bool,
}

impl MockRow {
    /// Convenience constructor for tests.
    pub fn new(id: u64, path: &str, start_line: usize, vector: Vec<f32>) -> Self {
        Self {
            id,
            path: path.to_string(),
            language: "ts".to_string(),
            start_line,
            end_line: start_line + 5,
            text: format!("chunk {id}"),
            symbol: None,
            vector,
            commit_sha: None,
            dirty: false,
            no_duplicate: false,
        }
    }

    fn to_hit(&self, score: f32) -> Hit {
        Hit {
            id: self.id,
            path: self.path.clone(),
            language: self.language.clone(),
            start_line: self.start_line,
            end_line: self.end_line,
            text: self.text.clone(),
            score,
            symbol: self.symbol.clone(),
            commit_sha: self.commit_sha.clone(),
            dirty: self.dirty,
            no_duplicate: self.no_duplicate,
        }
    }
}

/// Cosine similarity of two equal-length vectors. Returns 0 on length mismatch or a
/// zero-norm vector. Matches the `1 - array_cosine_distance` score the real backends use.
fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na * nb)
}

/// A single upserted batch: how many chunks and their `(path, start_line, symbol)` keys.
#[derive(Clone, Debug, PartialEq)]
pub struct UpsertBatch {
    pub count: usize,
    /// `(path, start_line, symbol)` for each chunk in the batch, in order.
    pub chunks: Vec<(String, usize, Option<String>)>,
}

/// Everything the Mock observed, for assertions.
#[derive(Default)]
pub struct MockCalls {
    pub ensure_ready: Vec<bool>,
    pub begin_bulk: usize,
    pub end_bulk: usize,
    pub upserts: Vec<UpsertBatch>,
    pub deletes: Vec<String>,
    pub flush: usize,
    pub queries: Vec<String>,
}

impl MockCalls {
    /// Total chunks across every upsert batch.
    pub fn total_upserted_chunks(&self) -> usize {
        self.upserts.iter().map(|b| b.count).sum()
    }
}

/// In-memory recording backend. Returns deterministic canned hits from `query`.
///
/// `calls` is behind an `Arc` so a test can keep a recorder handle while the backend
/// itself moves into the `Arc<dyn VectorStore>` (the flow tests drive the real
/// orchestration through [`crate::repos::mock::MockStore`]).
pub struct MockBackend {
    pub calls: Arc<Mutex<MockCalls>>,
    canned: Vec<Hit>,
    /// Optional stored rows-with-vectors for the MCP-path methods. Empty unless a test
    /// seeds them via [`MockBackend::with_rows`].
    rows: Vec<MockRow>,
}

impl MockBackend {
    /// Seed the mock's stored rows-with-vectors (for `query_by_vector` / `get_by_location`
    /// / `all_chunks_with_vectors` tests). Keeps the canned `query` hits unchanged.
    pub fn with_rows(rows: Vec<MockRow>) -> Self {
        let mut b = Self::new();
        b.rows = rows;
        b
    }

    /// New Mock whose `query` returns a fixed two-row result set.
    pub fn new() -> Self {
        Self {
            calls: Arc::new(Mutex::new(MockCalls::default())),
            rows: Vec::new(),
            canned: vec![
                Hit {
                    id: 1,
                    path: "src/alpha.ts".to_string(),
                    language: "ts".to_string(),
                    start_line: 1,
                    end_line: 10,
                    text: "alpha".to_string(),
                    score: 0.9000,
                    symbol: None,
                    commit_sha: None,
                    dirty: false,
                    no_duplicate: false,
                },
                Hit {
                    id: 2,
                    path: "src/beta.ts".to_string(),
                    language: "ts".to_string(),
                    start_line: 11,
                    end_line: 20,
                    text: "beta".to_string(),
                    score: 0.8000,
                    symbol: None,
                    commit_sha: None,
                    dirty: false,
                    no_duplicate: false,
                },
            ],
        }
    }

    pub async fn ensure_ready(&self, recreate: bool) -> Result<()> {
        self.calls.lock().unwrap().ensure_ready.push(recreate);
        Ok(())
    }

    record_unit_call!(begin_bulk, begin_bulk);
    record_unit_call!(end_bulk, end_bulk);

    pub async fn upsert(&self, chunks: &[crate::domain::CodeChunk]) -> Result<()> {
        let batch = UpsertBatch {
            count: chunks.len(),
            chunks: chunks
                .iter()
                .map(|c| (c.path.clone(), c.start_line, c.symbol.clone()))
                .collect(),
        };
        self.calls.lock().unwrap().upserts.push(batch);
        Ok(())
    }

    pub async fn delete_by_path(&self, path: &str) -> Result<()> {
        self.calls.lock().unwrap().deletes.push(path.to_string());
        Ok(())
    }

    pub async fn query(&self, q: &str, limit: u64) -> Result<Vec<Hit>> {
        self.calls.lock().unwrap().queries.push(q.to_string());
        Ok(self
            .canned
            .iter()
            .take(limit as usize)
            .map(|h| Hit {
                id: h.id,
                path: h.path.clone(),
                language: h.language.clone(),
                start_line: h.start_line,
                end_line: h.end_line,
                text: h.text.clone(),
                score: h.score,
                symbol: h.symbol.clone(),
                commit_sha: h.commit_sha.clone(),
                dirty: h.dirty,
                no_duplicate: h.no_duplicate,
            })
            .collect())
    }

    record_unit_call!(flush, flush);

    /// Rank seeded rows by cosine similarity to `vec`, excluding `exclude_id`, dedup by id
    /// (rows are already unique here), and truncate to `limit`. `score = cosine`.
    pub async fn query_by_vector(
        &self,
        vec: &[f32],
        limit: u64,
        exclude_id: Option<u64>,
    ) -> Result<Vec<Hit>> {
        let mut scored: Vec<(f32, &MockRow)> = self
            .rows
            .iter()
            .filter(|r| Some(r.id) != exclude_id)
            .map(|r| (cosine(vec, &r.vector), r))
            .collect();
        // Sort by score desc; ties broken by id asc for determinism.
        scored.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.1.id.cmp(&b.1.id))
        });
        let mut seen = std::collections::HashSet::new();
        let hits: Vec<Hit> = scored
            .into_iter()
            .filter(|(_, r)| seen.insert(r.id))
            .take(limit as usize)
            .map(|(s, r)| r.to_hit(s))
            .collect();
        Ok(hits)
    }

    /// Return the seeded row at `path`+`line` (and its vector), if any.
    pub async fn get_by_location(
        &self,
        path: &str,
        line: usize,
    ) -> Result<Option<(Hit, Vec<f32>)>> {
        Ok(self
            .rows
            .iter()
            .find(|r| r.path == path && r.start_line == line)
            .map(|r| (r.to_hit(1.0), r.vector.clone())))
    }

    /// Return every seeded row with its vector, optionally filtered by a path glob.
    pub async fn all_chunks_with_vectors(
        &self,
        path_glob: Option<&str>,
    ) -> Result<Vec<(Hit, Vec<f32>)>> {
        let matcher = match path_glob {
            None => None,
            Some(p) => Some(globset::Glob::new(p)?.compile_matcher()),
        };
        Ok(self
            .rows
            .iter()
            .filter(|r| matcher.as_ref().is_none_or(|m| m.is_match(&r.path)))
            .map(|r| (r.to_hit(1.0), r.vector.clone()))
            .collect())
    }

    /// Number of seeded rows.
    pub async fn chunk_count(&self) -> Result<u64> {
        Ok(self.rows.len() as u64)
    }

    pub async fn has_dirty(&self) -> Result<bool> {
        Ok(self.rows.iter().any(|r| r.dirty))
    }

    /// Deterministic canned query vector (length 4). Distinct text → distinct vector.
    pub async fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        Ok(canned_vector(text))
    }

    /// Deterministic canned passage vector (same scheme as `embed_query`).
    pub async fn embed_passage(&self, text: &str) -> Result<Vec<f32>> {
        Ok(canned_vector(text))
    }
}

/// Deterministic length-4 vector derived from the text bytes — enough to make distinct
/// inputs produce distinct vectors in tests without any real model.
fn canned_vector(text: &str) -> Vec<f32> {
    let mut v = [0f32; 4];
    for (i, b) in text.bytes().enumerate() {
        v[i % 4] += b as f32;
    }
    v.to_vec()
}
