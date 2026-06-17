//! The fully-resolved indexing [`Plan`] (args + config merged) and its accessor methods.

use std::collections::HashSet;

use globset::GlobSet;

use crate::domain::PrefixStyle;

/// Fully-resolved indexing plan (args + config merged).
#[derive(Clone)]
pub struct Plan {
    pub root: String,
    /// File extensions to walk (no dots). Each chunk's `language` payload label is
    /// derived per-file from its extension (see `indexer::language_for_path`).
    pub ext: Vec<String>,
    /// Selected vector backend: "qdrant" or "duckdb".
    pub backend: String,
    /// Selected embedder. For the qdrant backend: "qdrant" (server-side inference,
    /// default), "ort", or "ollama" (local embed). For the duckdb backend: "ort"
    /// (default) or "ollama".
    pub embedder: String,
    /// Selected chunker. When not explicitly provided, this is auto-chosen based
    /// on language + whether the `ast` feature is available at compile time.
    /// See `build_plan` for the exact precedence and smart-default rules.
    pub chunker: String,
    /// Max chunk size in chars — the size bound honored by both chunkers.
    pub max_chunk_chars: usize,
    /// Resolved embedding prefix policy (model-aware), shared by the embedders + Qdrant.
    pub prefix_style: PrefixStyle,
    pub collection: String,
    pub model: String,
    pub vector_dim: u64,
    /// Qdrant cluster URL from YAML (`qdrant.url`); the `QDRANT_URL` env var overrides it.
    /// Only used by the qdrant backend; the API key is read separately from the environment.
    #[cfg_attr(not(feature = "qdrant"), allow(dead_code))]
    pub qdrant_url: Option<String>,
    /// DuckDB file path (only used by the duckdb backend).
    #[cfg_attr(not(feature = "duckdb"), allow(dead_code))]
    pub duckdb_path: String,
    /// Optional ONNX model cache dir / HF cache (only used by the ort embedder).
    #[cfg_attr(not(feature = "ort"), allow(dead_code))]
    pub duckdb_model_cache: Option<String>,
    /// HuggingFace repo for the ort embedder (only used by the ort embedder).
    #[cfg_attr(not(feature = "ort"), allow(dead_code))]
    pub model_repo: String,
    /// Ollama server URL (only used by the ollama embedder).
    #[cfg_attr(not(feature = "ollama"), allow(dead_code))]
    pub ollama_url: String,
    /// Ollama model (only used by the ollama embedder; defaults to mxbai-embed-large).
    #[cfg_attr(not(feature = "ollama"), allow(dead_code))]
    pub ollama_model: Option<String>,
    pub exclude_dirs: HashSet<String>,
    pub include: GlobSet,
    /// Whether any include patterns were configured (empty = include everything).
    pub include_active: bool,
    pub exclude: GlobSet,
    pub skip_generated: bool,
    pub strip_comments: bool,
    /// Honor the `sai-noindexing` marker (skip matching chunks entirely). Default true.
    pub honor_noindex_marker: bool,
    /// Honor the `sai-noduplicate` marker (index but exclude from clustering). Default true.
    pub honor_noduplicate_marker: bool,
    pub limit: u64,
    /// Resolved similarity-threshold defaults (config value or built-in). MCP tool args
    /// still override these per call.
    pub find_similar_min_score: f32,
    pub duplicate_min_score: f32,
    pub duplicate_min_cluster_size: usize,
    pub top_k: usize,
}

impl Plan {
    /// Glob gate shared by walk (`collect_chunks`/`dry_run`) and `sync`: a file
    /// passes when the include allow-list admits it (or is inactive) AND no exclude
    /// glob matches. `dry_run` still inspects the two halves separately to report a
    /// reason, but the pass/skip decision lives here so it can't drift between paths.
    pub fn passes_globs(&self, key: &str) -> bool {
        (!self.include_active || self.include.is_match(key)) && !self.exclude.is_match(key)
    }

    /// Whether this plan embeds locally (the worker can call `embed_query`/`embed_passage`
    /// → `query_by_vector` instead of the server text-query path). Single source of truth
    /// for the three call sites (`app.rs` index/MCP workers, `mcp.rs` server). True for the
    /// `ort`/`ollama` embedders (duckdb always; qdrant in local-embed mode); false for the
    /// `qdrant` embedder (Qdrant Cloud server-side inference).
    pub fn can_embed_locally(&self) -> bool {
        self.embedder == "ort" || self.embedder == "ollama"
    }

    /// Resolved `find_similar` minimum cosine score (config value or built-in default).
    pub fn find_similar_min_score(&self) -> f32 {
        self.find_similar_min_score
    }

    /// Resolved `find_duplicates` edge minimum cosine score.
    pub fn duplicate_min_score(&self) -> f32 {
        self.duplicate_min_score
    }

    /// Resolved `find_duplicates` minimum cluster size.
    pub fn duplicate_min_cluster_size(&self) -> usize {
        self.duplicate_min_cluster_size
    }

    /// Resolved `find_duplicates` per-chunk nearest-neighbor fetch (top-k).
    pub fn top_k(&self) -> usize {
        self.top_k
    }
}
