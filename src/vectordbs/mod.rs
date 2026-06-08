//! Enum-dispatched vector backend layer. Native `async fn` per backend (no
//! async_trait / dyn). Only the Qdrant arm exists in P0.

pub mod embedder;

#[cfg(feature = "duckdb")]
pub mod duckdb;
#[cfg(test)]
pub(crate) mod mock;
#[cfg(feature = "qdrant")]
pub mod qdrant;

use anyhow::Result;

use crate::config::Plan;

/// E5 asymmetric prefix for stored passages.
pub const PASSAGE_PREFIX: &str = "passage: ";
/// E5 asymmetric prefix for queries.
pub const QUERY_PREFIX: &str = "query: ";
/// QwenInstruct query instruction. Qwen embedding models are instruction-tuned: the
/// query (not the passage) is wrapped with a task description. The stored passage is bare.
pub const QWEN_QUERY_INSTRUCT: &str =
    "Instruct: Given a code search query, retrieve relevant code\nQuery: ";

/// Model-aware embedding prefix policy. Resolved once in `build_plan` (explicit config
/// wins; else auto-detected from the model name) and applied by BOTH embedders and the
/// Qdrant `Document` path through the shared [`format_passage`]/[`format_query`] helpers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PrefixStyle {
    /// E5 asymmetric prefixes: `passage: <t>` / `query: <t>`.
    E5,
    /// Qwen instruct: bare passage; query wrapped with a task instruction.
    Qwen,
    /// No prefix on either side.
    None,
}

impl PrefixStyle {
    /// Parse an explicit `prefix_style` config value ("e5" | "qwen" | "none").
    pub fn parse(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "e5" => Ok(PrefixStyle::E5),
            "qwen" => Ok(PrefixStyle::Qwen),
            "none" => Ok(PrefixStyle::None),
            other => {
                anyhow::bail!("unknown prefix_style '{other}' (expected 'e5', 'qwen', or 'none')")
            }
        }
    }

    /// Auto-detect the prefix style from a model name: contains "e5" → E5,
    /// contains "qwen" → Qwen, otherwise None.
    pub fn detect(model: &str) -> Self {
        let m = model.to_ascii_lowercase();
        if m.contains("e5") {
            PrefixStyle::E5
        } else if m.contains("qwen") {
            PrefixStyle::Qwen
        } else {
            PrefixStyle::None
        }
    }
}

/// Format a stored passage under the resolved prefix policy. Single source of truth
/// shared by the Qdrant `Document` path and both DuckDB embedders.
#[cfg_attr(
    not(any(feature = "qdrant", feature = "ort", feature = "ollama")),
    allow(dead_code)
)]
pub fn format_passage(style: PrefixStyle, text: &str) -> String {
    match style {
        PrefixStyle::E5 => format!("{PASSAGE_PREFIX}{text}"),
        // Qwen: passages are bare (the instruction goes on the query side only).
        PrefixStyle::Qwen | PrefixStyle::None => text.to_string(),
    }
}

/// Format a query under the resolved prefix policy (shared, see [`format_passage`]).
#[cfg_attr(
    not(any(feature = "qdrant", feature = "ort", feature = "ollama")),
    allow(dead_code)
)]
pub fn format_query(style: PrefixStyle, text: &str) -> String {
    match style {
        PrefixStyle::E5 => format!("{QUERY_PREFIX}{text}"),
        PrefixStyle::Qwen => format!("{QWEN_QUERY_INSTRUCT}{text}"),
        PrefixStyle::None => text.to_string(),
    }
}

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

/// The vector backend. Match-dispatched.
pub enum Backend {
    #[cfg(feature = "qdrant")]
    Qdrant(qdrant::QdrantBackend),
    // Boxed: DuckDbBackend embeds an Embedder (ONNX session/tokenizer for ort) and is
    // far larger than the Qdrant variant (clippy::large_enum_variant).
    #[cfg(feature = "duckdb")]
    DuckDb(Box<duckdb::DuckDbBackend>),
    /// In-memory recording backend for happy-path flow tests (never ships).
    #[cfg(test)]
    Mock(mock::MockBackend),
}

impl Backend {
    /// Prepare storage (create collection/table + indexes) if missing.
    pub async fn ensure_ready(&self, recreate: bool) -> Result<()> {
        match self {
            #[cfg(feature = "qdrant")]
            Backend::Qdrant(b) => b.ensure_ready(recreate).await,
            #[cfg(feature = "duckdb")]
            Backend::DuckDb(b) => b.ensure_ready(recreate).await,
            #[cfg(test)]
            Backend::Mock(b) => b.ensure_ready(recreate).await,
        }
    }

    /// Begin a bulk insert window (e.g. drop index). No-op for Qdrant.
    pub async fn begin_bulk(&self) -> Result<()> {
        // sai-noduplicate: inverse of end_bulk; intentionally symmetric bookend (Backend dispatch)
        match self {
            #[cfg(feature = "qdrant")]
            Backend::Qdrant(b) => b.begin_bulk().await,
            #[cfg(feature = "duckdb")]
            Backend::DuckDb(b) => b.begin_bulk().await,
            #[cfg(test)]
            Backend::Mock(b) => b.begin_bulk().await,
        }
    }

    /// End a bulk insert window (e.g. recreate index). No-op for Qdrant.
    pub async fn end_bulk(&self) -> Result<()> {
        // sai-noduplicate: inverse of begin_bulk; intentionally symmetric bookend (Backend dispatch)
        match self {
            #[cfg(feature = "qdrant")]
            Backend::Qdrant(b) => b.end_bulk().await,
            #[cfg(feature = "duckdb")]
            Backend::DuckDb(b) => b.end_bulk().await,
            #[cfg(test)]
            Backend::Mock(b) => b.end_bulk().await,
        }
    }

    /// Upsert a batch of chunks.
    pub async fn upsert(&self, chunks: &[CodeChunk]) -> Result<()> {
        match self {
            #[cfg(feature = "qdrant")]
            Backend::Qdrant(b) => b.upsert(chunks).await,
            #[cfg(feature = "duckdb")]
            Backend::DuckDb(b) => b.upsert(chunks).await,
            #[cfg(test)]
            Backend::Mock(b) => b.upsert(chunks).await,
        }
    }

    /// Delete every stored chunk for a given file path.
    pub async fn delete_by_path(&self, path: &str) -> Result<()> {
        match self {
            #[cfg(feature = "qdrant")]
            Backend::Qdrant(b) => b.delete_by_path(path).await,
            #[cfg(feature = "duckdb")]
            Backend::DuckDb(b) => b.delete_by_path(path).await,
            #[cfg(test)]
            Backend::Mock(b) => b.delete_by_path(path).await,
        }
    }

    /// Nearest-neighbour search.
    pub async fn query(&self, q: &str, limit: u64) -> Result<Vec<Hit>> {
        match self {
            #[cfg(feature = "qdrant")]
            Backend::Qdrant(b) => b.query(q, limit).await,
            #[cfg(feature = "duckdb")]
            Backend::DuckDb(b) => b.query(q, limit).await,
            #[cfg(test)]
            Backend::Mock(b) => b.query(q, limit).await,
        }
    }

    /// Drop all stored vectors (delete collection/table).
    pub async fn flush(&self) -> Result<()> {
        match self {
            #[cfg(feature = "qdrant")]
            Backend::Qdrant(b) => b.flush().await,
            #[cfg(feature = "duckdb")]
            Backend::DuckDb(b) => b.flush().await,
            #[cfg(test)]
            Backend::Mock(b) => b.flush().await,
        }
    }

    /// Nearest-neighbour search by a RAW vector (no embedding). Over-fetches and dedups
    /// by id (HNSW can return the same id more than once), optionally excluding one id
    /// (self-exclusion for `find_similar` / `find_duplicates`). `score = 1 - distance`.
    /// Used by the shared similarity-search core (`crate::search`) behind both the CLI
    /// `similar`/`duplicates` subcommands and the MCP server's stored-vector search paths.
    #[cfg_attr(not(any(feature = "duckdb", feature = "qdrant")), allow(dead_code))]
    pub async fn query_by_vector(
        &self,
        vec: &[f32],
        limit: u64,
        exclude_id: Option<u64>,
    ) -> Result<Vec<Hit>> {
        match self {
            #[cfg(feature = "qdrant")]
            Backend::Qdrant(b) => b.query_by_vector(vec, limit, exclude_id).await,
            #[cfg(feature = "duckdb")]
            Backend::DuckDb(b) => b.query_by_vector(vec, limit, exclude_id).await,
            #[cfg(test)]
            Backend::Mock(b) => b.query_by_vector(vec, limit, exclude_id).await,
        }
    }

    /// Fetch a single stored chunk (and its vector) by file path + 1-based start line.
    /// Returns `None` when no chunk starts at that location. Used by `find_similar` to
    /// reuse the EXACT stored vector (no re-embed) of an existing function.
    #[cfg_attr(not(any(feature = "duckdb", feature = "qdrant")), allow(dead_code))]
    pub async fn get_by_location(
        &self,
        path: &str,
        line: usize,
    ) -> Result<Option<(Hit, Vec<f32>)>> {
        match self {
            #[cfg(feature = "qdrant")]
            Backend::Qdrant(b) => b.get_by_location(path, line).await,
            #[cfg(feature = "duckdb")]
            Backend::DuckDb(b) => b.get_by_location(path, line).await,
            #[cfg(test)]
            Backend::Mock(b) => b.get_by_location(path, line).await,
        }
    }

    /// Every stored chunk paired with its vector, optionally restricted to a path glob.
    /// Used by `find_duplicates` (codebase-wide near-duplicate clustering). Stored vectors
    /// only — never re-embeds.
    #[cfg_attr(not(any(feature = "duckdb", feature = "qdrant")), allow(dead_code))]
    pub async fn all_chunks_with_vectors(
        &self,
        path_glob: Option<&str>,
    ) -> Result<Vec<(Hit, Vec<f32>)>> {
        match self {
            #[cfg(feature = "qdrant")]
            Backend::Qdrant(b) => b.all_chunks_with_vectors(path_glob).await,
            #[cfg(feature = "duckdb")]
            Backend::DuckDb(b) => b.all_chunks_with_vectors(path_glob).await,
            #[cfg(test)]
            Backend::Mock(b) => b.all_chunks_with_vectors(path_glob).await,
        }
    }

    /// Total stored chunk count (for `index_status`).
    #[cfg_attr(not(feature = "mcp"), allow(dead_code))]
    pub async fn chunk_count(&self) -> Result<u64> {
        match self {
            #[cfg(feature = "qdrant")]
            Backend::Qdrant(b) => b.chunk_count().await,
            #[cfg(feature = "duckdb")]
            Backend::DuckDb(b) => b.chunk_count().await,
            #[cfg(test)]
            Backend::Mock(b) => b.chunk_count().await,
        }
    }

    /// Quick check for any dirty-stamped chunks (used for pre-`duplicates` warning).
    /// Best-effort; false on backends without the column or on error.
    #[cfg_attr(not(any(feature = "duckdb", feature = "qdrant")), allow(dead_code))]
    pub async fn has_dirty(&self) -> Result<bool> {
        match self {
            #[cfg(feature = "qdrant")]
            Backend::Qdrant(b) => b.has_dirty().await,
            #[cfg(feature = "duckdb")]
            Backend::DuckDb(b) => b.has_dirty().await,
            #[cfg(test)]
            Backend::Mock(b) => b.has_dirty().await,
        }
    }

    /// Embed a search query (asymmetric `query:` side) using the backend's embedder.
    /// DuckDB delegates to its owned local embedder; Qdrant has no local embedder
    /// (server-side inference), so the MCP server uses `query()` for it instead.
    #[cfg_attr(not(feature = "mcp"), allow(dead_code))]
    // Qdrant-only builds compile just the server-side-inference arm (which ignores `text`).
    #[cfg_attr(not(any(feature = "duckdb", test)), allow(unused_variables))]
    pub async fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        // sai-noduplicate: asymmetric query-side twin of embed_passage (Backend dispatch)
        match self {
            #[cfg(feature = "qdrant")]
            Backend::Qdrant(_) => {
                anyhow::bail!("qdrant backend embeds server-side; no local query embedding")
            }
            #[cfg(feature = "duckdb")]
            Backend::DuckDb(b) => b.embed_query(text).await,
            #[cfg(test)]
            Backend::Mock(b) => b.embed_query(text).await,
        }
    }

    /// Embed code as a stored PASSAGE (asymmetric `passage:` side / code-vs-code space).
    /// Used by `find_similar { code }` (CLI `similar --code` and the MCP tool).
    #[cfg_attr(not(any(feature = "duckdb", feature = "qdrant")), allow(dead_code))]
    // Qdrant-only builds compile just the server-side-inference arm (which ignores `text`).
    #[cfg_attr(not(any(feature = "duckdb", test)), allow(unused_variables))]
    pub async fn embed_passage(&self, text: &str) -> Result<Vec<f32>> {
        // sai-noduplicate: asymmetric passage-side twin of embed_query (Backend dispatch)
        match self {
            #[cfg(feature = "qdrant")]
            Backend::Qdrant(_) => {
                anyhow::bail!("qdrant backend embeds server-side; no local passage embedding")
            }
            #[cfg(feature = "duckdb")]
            Backend::DuckDb(b) => b.embed_passage(text).await,
            #[cfg(test)]
            Backend::Mock(b) => b.embed_passage(text).await,
        }
    }
}

/// How a backend should be opened. Only the DuckDB arm of [`factory`] distinguishes
/// these: `ReadOnly` opens the file without index maintenance or writes, so a search can
/// run while an index is open elsewhere. Qdrant is a remote path that is already
/// read-capable, so both modes behave identically there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Normal open: index maintenance + writes (indexing, `refresh`, HNSW persistence).
    ReadWrite,
    /// Read-only search path (the MCP server + the CLI `similar`/`duplicates` subcommands).
    ReadOnly,
}

/// Build the backend selected by `plan.backend`, opened per `access`. Arms are cfg-gated:
/// selecting a backend whose feature was not compiled in yields a clear, actionable error.
pub fn factory(plan: &Plan, access: Access) -> Result<Backend> {
    let _ = access; // only consulted by the duckdb arm
    match plan.backend.as_str() {
        "qdrant" => {
            #[cfg(feature = "qdrant")]
            {
                Ok(Backend::Qdrant(qdrant::QdrantBackend::connect(plan)?))
            }
            #[cfg(not(feature = "qdrant"))]
            {
                anyhow::bail!(
                    "backend 'qdrant' selected but this binary was built without the 'qdrant' feature (rebuild with --features qdrant)"
                )
            }
        }
        "duckdb" => {
            #[cfg(feature = "duckdb")]
            {
                let embedder = build_embedder(plan)?;
                let backend = match access {
                    Access::ReadOnly => duckdb::DuckDbBackend::connect_readonly(plan, embedder)?,
                    Access::ReadWrite => duckdb::DuckDbBackend::connect(plan, embedder)?,
                };
                Ok(Backend::DuckDb(Box::new(backend)))
            }
            #[cfg(not(feature = "duckdb"))]
            {
                anyhow::bail!(
                    "backend 'duckdb' selected but this binary was built without the 'duckdb' feature (rebuild with --features duckdb)"
                )
            }
        }
        other => anyhow::bail!("unknown backend '{other}' (expected 'qdrant' or 'duckdb')"),
    }
}

/// If `err` is a DuckDB embedding-dimension mismatch, return the path of the DuckDB file
/// that must be deleted to recover. Returns `None` for any other error — and always when
/// the `duckdb` feature is not compiled in. Lets the CLI offer an interactive
/// "delete & re-index?" without string-matching the error message.
pub fn dim_mismatch_duckdb_path(err: &anyhow::Error) -> Option<String> {
    #[cfg(feature = "duckdb")]
    {
        if let Some(m) = err.downcast_ref::<duckdb::DimMismatch>() {
            return Some(m.duckdb_path.clone());
        }
    }
    let _ = err;
    None
}

/// Build the embedder selected by `plan.embedder` for the DuckDB backend. Arms are
/// cfg-gated: selecting an embedder whose feature was not compiled in yields a clear
/// error. `ort` is the default; `ollama` is the remote HTTP option.
#[cfg(feature = "duckdb")]
fn build_embedder(plan: &Plan) -> Result<embedder::Embedder> {
    match plan.embedder.as_str() {
        "ort" => {
            #[cfg(feature = "ort")]
            {
                eprintln!(
                    "embedder=ort: downloading model via ONNX Runtime (ORT) from Hugging Face if not cached: {}",
                    plan.model_repo
                );
                Ok(embedder::Embedder::Ort(embedder::ort_embedder(plan)?))
            }
            #[cfg(not(feature = "ort"))]
            {
                anyhow::bail!(
                    "embedder 'ort' selected but this binary was built without the 'ort' feature (rebuild with --features ort)"
                )
            }
        }
        "ollama" => {
            #[cfg(feature = "ollama")]
            {
                Ok(embedder::Embedder::Ollama(embedder::ollama_embedder(plan)?))
            }
            #[cfg(not(feature = "ollama"))]
            {
                anyhow::bail!(
                    "embedder 'ollama' selected but this binary was built without the 'ollama' feature (rebuild with --features ollama)"
                )
            }
        }
        other => anyhow::bail!("unknown embedder '{other}' (expected 'ort' or 'ollama')"),
    }
}
