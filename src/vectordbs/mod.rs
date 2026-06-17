//! Concrete vector backends (qdrant / duckdb / mock) and the shared embedder, embedding
//! helpers, and dim guard. The backends are wrapped by the `crate::repos::VectorStore`
//! adapters; this module no longer dispatches over them.

pub mod embedder;

#[cfg(feature = "duckdb")]
pub mod duckdb;
#[cfg(test)]
pub(crate) mod mock;
#[cfg(feature = "qdrant")]
pub mod qdrant;

use anyhow::{Context, Result};

// Only `build_embedder` (duckdb-gated) consumes `Plan` now that the enum `factory` is gone.
#[cfg(feature = "duckdb")]
use crate::domain::Plan;

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

/// Pop the single passage vector from an embedder batch result and dimension-check it.
/// The shared tail of both backends' `embed_passage` (DuckDB owns its embedder; Qdrant
/// borrows an optional one), so the "no vector returned" / dim-guard errors live in one
/// place. The embedder access differs per backend; only this finalising step is common.
#[cfg_attr(
    not(any(
        feature = "duckdb",
        all(feature = "qdrant", any(feature = "ort", feature = "ollama"))
    )),
    allow(dead_code)
)]
pub fn finalize_passage(mut vectors: Vec<Vec<f32>>, vector_dim: u64) -> Result<Vec<f32>> {
    let v = vectors
        .pop()
        .context("embedder returned no vector for the passage")?;
    check_dim(v.len(), vector_dim)?;
    Ok(v)
}

/// Compile an optional path glob into a matcher, with a consistent `invalid path_glob`
/// error. Single source of truth for the `all_chunks_with_vectors` path filter shared by
/// every backend (DuckDB/Qdrant/mock) and the MCP `find_duplicates`/`find_similar` tools
/// (the MCP layer maps the error to `invalid_params`).
#[cfg_attr(not(any(feature = "duckdb", feature = "qdrant")), allow(dead_code))]
pub fn compile_path_glob(pattern: Option<&str>) -> Result<Option<globset::GlobMatcher>> {
    match pattern {
        None => Ok(None),
        Some(p) => Ok(Some(
            globset::Glob::new(p)
                .with_context(|| format!("invalid path_glob: {p}"))?
                .compile_matcher(),
        )),
    }
}

/// How a backend should be opened. Only the DuckDB arm of [`crate::factory`] distinguishes
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
// `pub(crate)`: reused by the clean-arch composition root (`crate::factory`) to build the
// local embedder for the DuckDb store + qdrant local-embed mode. No logic change — the same
// fn the enum `factory` calls; just widened so the new factory does not duplicate it.
#[cfg(feature = "duckdb")]
pub(crate) fn build_embedder(plan: &Plan) -> Result<embedder::Embedder> {
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
