//! The [`Hit`] value object: a search result row.

/// A search result row.
pub struct Hit {
    #[allow(dead_code)]
    pub id: u64,
    pub path: String,
    #[allow(dead_code)]
    pub language: String,
    pub start_line: usize,
    pub end_line: usize,
    #[allow(dead_code)]
    pub text: String,
    pub score: f32,
    /// Captured symbol name when available (AST chunker / stored column). `None` for
    /// line-window chunks or when the backend does not return it. Surfaced by the CLI
    /// `similar`/`duplicates` subcommands and the MCP tools (`search_code` /
    /// `find_duplicates`).
    #[cfg_attr(not(any(feature = "duckdb", feature = "qdrant")), allow(dead_code))]
    pub symbol: Option<String>,
    /// Commit at which this chunk was last indexed (if stamped).
    #[allow(dead_code)]
    pub commit_sha: Option<String>,
    #[allow(dead_code)]
    pub dirty: bool,
    /// True if the chunk carries a sai-noduplicate marker: indexed/searchable but excluded from near-duplicate clustering.
    pub no_duplicate: bool,
}
