//! Indexing/refresh report value objects: [`ReindexOutcome`] (per-file),
//! [`RefreshReport`] (a batch of them), and the indexing-side [`IndexProgress`]
//! (live per-batch progress) + [`IndexReport`] (the index_sources summary).

/// Outcome of re-indexing a single path: either it was removed (gone/excluded/empty,
/// with a reason) or re-indexed with N fresh chunks.
///
/// This type is intentionally small and owned so it can be returned across the
/// worker-thread boundary in the MCP server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReindexOutcome {
    /// File removed from the index. `reason` explains why (for logging only).
    Removed { reason: &'static str },
    /// File re-indexed with `chunks` fresh chunks upserted.
    Reindexed { chunks: usize },
}

/// Outcome of a `Refresh` batch: one [`ReindexOutcome`] per input path (leading `./`
/// stripped), in input order — so the CLI can render the exact per-file lines and the
/// MCP tool can split refreshed/removed.
pub struct RefreshReport {
    /// `(path, outcome)` for each requested path, in request order.
    pub entries: Vec<(String, ReindexOutcome)>,
}

/// Live indexing progress, emitted once per upsert batch by
/// [`crate::service::IndexingService::index_sources`].
///
/// `Send` + owned so it can later cross a channel to a UI/progress consumer (US-004 wires
/// the CLI's TTY progress closure to it). The fields are unread until that wiring lands, so
/// they carry `#[allow(dead_code)]` to satisfy `clippy -D warnings` in the lib target.
#[derive(Clone, Debug)]
pub struct IndexProgress {
    /// Distinct files crossed into so far (counts up to `files_total`).
    #[allow(dead_code)]
    pub files_done: usize,
    /// Total files that produced chunks for this run.
    #[allow(dead_code)]
    pub files_total: usize,
    /// Chunks upserted so far (counts up to `chunks_total`).
    #[allow(dead_code)]
    pub chunks_done: usize,
    /// Total chunks to upsert this run.
    #[allow(dead_code)]
    pub chunks_total: usize,
    /// Path of the most recently seen chunk in the batch that triggered this update.
    #[allow(dead_code)]
    pub path: String,
}

/// Summary returned by [`crate::service::IndexingService::index_sources`] — the data the
/// CLI prints. Fields are unread until US-004 wires the CLI summary line, so they carry
/// `#[allow(dead_code)]` for `clippy -D warnings` in the lib target.
pub struct IndexReport {
    /// Total chunks upserted.
    #[allow(dead_code)]
    pub chunks: usize,
    /// Files that produced chunks.
    #[allow(dead_code)]
    pub files: usize,
    /// Files skipped by config (globs / generated marker).
    #[allow(dead_code)]
    pub skipped: usize,
}
