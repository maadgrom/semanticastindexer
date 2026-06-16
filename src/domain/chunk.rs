//! The [`CodeChunk`] entity: one embeddable slice of a source file, ready to upsert.

/// One embeddable slice of a source file, ready to upsert.
pub struct CodeChunk {
    pub id: u64,
    pub path: String,
    pub language: String,
    pub start_line: usize,
    pub end_line: usize,
    pub text: String,
    /// Captured symbol name (AST chunker). `None` for line-window chunks.
    pub symbol: Option<String>,
    /// Git commit this version was indexed at (for "changes per commit" picture).
    /// None for back-compat / pre-stamping runs.
    pub commit_sha: Option<String>,
    /// True if the source tree had uncommitted changes at index time.
    pub dirty: bool,
    /// True if the chunk carries a sai-noduplicate marker: indexed/searchable but excluded from near-duplicate clustering.
    pub no_duplicate: bool,
}
