//! Indexing/refresh report value objects: [`ReindexOutcome`] (per-file) and
//! [`RefreshReport`] (a batch of them).

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
