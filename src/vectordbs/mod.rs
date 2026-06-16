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
// Transitional re-export shim (US-001): the embedding entities/value objects + the
// `CodeChunk`/`Hit` entities now live in `crate::domain`. Re-exported here so existing
// call sites that import them via `crate::vectordbs::…` keep resolving without churn.
// Removed in a later story when call sites import from `crate::domain` directly.
pub use crate::domain::{
    CodeChunk, Hit, PASSAGE_PREFIX, PrefixStyle, QUERY_PREFIX, QWEN_QUERY_INSTRUCT, format_passage,
    format_query,
};

/// Runtime guard shared by both backends: a locally-produced vector's length MUST equal
/// the configured `vector_dim`. A mismatch means the chosen model does not match the
/// config. Single source of truth for the dim-guard message used by the DuckDB table
/// column (`FLOAT[vector_dim]`) and the Qdrant raw-vector (local-embed) path.
#[cfg_attr(
    not(any(
        feature = "duckdb",
        all(feature = "qdrant", any(feature = "ort", feature = "ollama"))
    )),
    allow(dead_code)
)]
pub fn check_dim(produced: usize, vector_dim: u64) -> Result<()> {
    if produced as u64 != vector_dim {
        anyhow::bail!(
            "embedder produced {produced}-d vectors but vector_dim={vector_dim} — set vector_dim to match the model (e5-small=384, nomic-embed-text=768, mxbai-embed-large=1024)"
        );
    }
    Ok(())
}

/// The vector backend. Match-dispatched.
pub enum Backend {
    // Boxed UNCONDITIONALLY (mirrors `DuckDb`): in local-embed mode (`embedder: ort`/`ollama`) the
    // variant carries an optional `Embedder` (ONNX session for ort), so it is large. Boxing
    // both heavy variants keeps the enum small by construction and stops a clippy
    // `large_enum_variant` flip (CI is `-D warnings`) from silently churning the public shape.
    #[cfg(feature = "qdrant")]
    Qdrant(Box<qdrant::QdrantBackend>),
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
    /// DuckDB delegates to its owned local embedder; Qdrant delegates to its OPTIONAL local
    /// embedder (`embedder: ort`/`ollama`) and bails in server mode (the qdrant method is a
    /// cfg-pair, so the arm resolves in every build, including bare `--features qdrant`).
    #[cfg_attr(not(feature = "mcp"), allow(dead_code))]
    // Qdrant-only builds compile just the server-side-inference arm (which ignores `text`).
    #[cfg_attr(not(any(feature = "duckdb", test)), allow(unused_variables))]
    pub async fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        match self {
            #[cfg(feature = "qdrant")]
            Backend::Qdrant(b) => b.embed_query(text).await,
            #[cfg(feature = "duckdb")]
            Backend::DuckDb(b) => b.embed_query(text).await,
            #[cfg(test)]
            Backend::Mock(b) => b.embed_query(text).await,
        }
    }

    /// Embed code as a stored PASSAGE (asymmetric `passage:` side / code-vs-code space).
    /// Used by `find_similar { code }` (CLI `similar --code` and the MCP tool). Qdrant
    /// delegates to its OPTIONAL local embedder (cfg-pair; bails in server mode).
    #[cfg_attr(not(any(feature = "duckdb", feature = "qdrant")), allow(dead_code))]
    // Qdrant-only builds compile just the server-side-inference arm (which ignores `text`).
    #[cfg_attr(not(any(feature = "duckdb", test)), allow(unused_variables))]
    pub async fn embed_passage(&self, text: &str) -> Result<Vec<f32>> {
        match self {
            #[cfg(feature = "qdrant")]
            Backend::Qdrant(b) => b.embed_passage(text).await,
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
                // The embedder value drives where embedding happens: `qdrant` =
                // server-side inference (the Document API, no local embedder feature
                // needed); `ort`/`ollama` = embed locally and upsert raw vectors.
                let backend = match plan.embedder.as_str() {
                    "qdrant" => qdrant::QdrantBackend::connect(plan)?,
                    "ort" | "ollama" => {
                        #[cfg(any(feature = "ort", feature = "ollama"))]
                        {
                            qdrant::QdrantBackend::connect_local(plan, build_embedder(plan)?)?
                        }
                        #[cfg(not(any(feature = "ort", feature = "ollama")))]
                        {
                            anyhow::bail!(
                                "local embedding for qdrant requires a local embedder — rebuild with --features qdrant,ort (or qdrant,ollama)"
                            )
                        }
                    }
                    other => anyhow::bail!(
                        "unknown embedder '{other}' for the qdrant backend (expected 'qdrant', 'ort', or 'ollama')"
                    ),
                };
                Ok(Backend::Qdrant(Box::new(backend)))
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
                tracing::info!(
                    model_repo = %plan.model_repo,
                    "embedder=ort: downloading model via ONNX Runtime (ORT) from Hugging Face if not cached"
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
