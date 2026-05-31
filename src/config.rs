//! Config loading and plan resolution: YAML config, CLI-merged `Plan`, glob compilation.

use anyhow::{Context, Result};
use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::Deserialize;
use std::collections::HashSet;
use std::path::Path;

use crate::Args;
use crate::vectordbs::PrefixStyle;

pub const DEFAULT_CONFIG: &str = "indexer.yaml";
const DEFAULT_COLLECTION: &str = "source_code";
const DEFAULT_MODEL: &str = "intfloat/multilingual-e5-small";
const DEFAULT_VECTOR_DIM: u64 = 384;
const DEFAULT_BACKEND: &str = "qdrant";
const DEFAULT_EMBEDDER: &str = "ort";
const DEFAULT_CHUNKER: &str = "lines";

/// Languages for which the AST chunker is preferred by default (when the binary
/// was built with the `ast` feature and the user did not explicitly set a chunker
/// via CLI or config). For all other languages we fall back to the reliable
/// line-based chunker.
const AST_PREFERRED_LANGUAGES: &[&str] = &["ts", "tsx", "rs"];

/// Returns whether we should prefer the AST chunker for this language
/// when no explicit chunker was provided.
fn ast_preferred_for_language(language: &str) -> bool {
    AST_PREFERRED_LANGUAGES
        .iter()
        .any(|&l| l.eq_ignore_ascii_case(language))
}
const DEFAULT_DUCKDB_PATH: &str = ".index/code.duckdb";
const DEFAULT_MODEL_REPO: &str = "Xenova/multilingual-e5-small";
const DEFAULT_OLLAMA_URL: &str = "http://localhost:11434";
// Default Ollama embedding model. mxbai-embed-large is 1024-d → use vector_dim: 1024.
const DEFAULT_OLLAMA_MODEL: &str = "mxbai-embed-large";
// Similarity-threshold built-in defaults. These are MODEL-SPECIFIC cutoffs (Qwen ≠ e5);
// tune them in the YAML `similarity:` block per model. Resolution for every knob is
// MCP tool arg > config value > these defaults.
/// `find_similar` minimum cosine score (neighbors below this are dropped).
const DEFAULT_FIND_SIMILAR_MIN_SCORE: f32 = 0.85;
/// `find_duplicates` minimum cosine score for an edge between two chunks.
const DEFAULT_DUPLICATE_MIN_SCORE: f32 = 0.93;
/// `find_duplicates` minimum cluster size to report.
const DEFAULT_DUPLICATE_MIN_CLUSTER_SIZE: usize = 2;
/// `find_duplicates` per-chunk nearest-neighbor fetch (top-k).
const DEFAULT_TOP_K: usize = 10;
/// Chunk-size cap (chars) for the E5 / Qdrant path: E5's 512-token window ≈ 1400 chars.
const CAP_E5: usize = 1400;
/// Chunk-size cap (chars) for large-context models (qwen / ollama): ~8K tokens at
/// ~4 chars/token ≈ 32000 chars. No tokenizer needed — a char approximation suffices.
const CAP_LARGE: usize = 32000;
/// Dirs ALWAYS pruned, regardless of YAML config (the original SKIP_DIRS set, kept for sure).
const HARD_PRUNE_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    "dist",
    "build",
    "target",
    ".next",
    "coverage",
    ".turbo",
];

/// YAML config. All fields optional so a partial file still parses.
#[derive(Deserialize, Default)]
#[serde(default)]
pub struct Config {
    /// Vector backend: "qdrant" (default) or "duckdb". CLI `--backend` overrides.
    pub backend: Option<String>,
    /// Embedder for the duckdb backend: "ort" (default) or "ollama". CLI `--embedder` overrides.
    pub embedder: Option<String>,
    /// Chunker: "lines" or "ast" (tree-sitter, needs the `ast` feature).
    /// When not explicitly set, we auto-select "ast" for languages we have good
    /// grammars for (currently ts/tsx) if the binary was built with --features ast.
    /// CLI `--chunker` always takes precedence.
    pub chunker: Option<String>,
    /// Max chunk size in chars. When unset, defaulted by the embedder/model (E5≈1400, qwen/ollama≈32000).
    pub max_chunk_chars: Option<usize>,
    /// Embedding prefix policy: "e5" | "qwen" | "none". When unset, auto-detected from the model name.
    pub prefix_style: Option<String>,
    pub collection: Option<String>,
    pub model: Option<String>,
    pub vector_dim: Option<u64>,
    pub exclude_dirs: Vec<String>,
    /// Allow-list globs. When non-empty, ONLY matching files are considered (exclude still wins).
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub skip_generated_marker: bool,
    /// Strip comments from C-family source before embedding/storing. Default true.
    pub strip_comments: Option<bool>,
    /// DuckDB backend settings (path + ONNX model cache/repo).
    pub duckdb: DuckDbConfig,
    /// Ollama embedder settings (url + model).
    pub ollama: OllamaConfig,
    /// Similarity-threshold defaults for the MCP find_similar/find_duplicates tools.
    pub similarity: SimilarityConfig,
}

/// Similarity-threshold YAML sub-section. Every field is optional; missing ones fall
/// back to the built-in defaults. MCP tool args override these per call.
#[derive(Deserialize, Default)]
#[serde(default)]
pub struct SimilarityConfig {
    /// `find_similar` minimum cosine score (default 0.85).
    pub find_similar_min_score: Option<f32>,
    /// `find_duplicates` edge minimum cosine score (default 0.93).
    pub duplicate_min_score: Option<f32>,
    /// `find_duplicates` minimum cluster size to report (default 2).
    pub duplicate_min_cluster_size: Option<usize>,
    /// `find_duplicates` per-chunk nearest-neighbor fetch (default 10).
    pub top_k: Option<usize>,
}

/// DuckDB-backend YAML sub-section.
#[derive(Deserialize, Default)]
#[serde(default)]
pub struct DuckDbConfig {
    /// DuckDB file path. Default `.index/code.duckdb`.
    pub path: Option<String>,
    /// Optional ONNX model cache dir (HF cache) for offline reuse.
    pub model_cache: Option<String>,
    /// HuggingFace repo the ort embedder downloads `onnx/model.onnx` + `tokenizer.json`
    /// from. Default `Xenova/multilingual-e5-small`.
    pub model_repo: Option<String>,
}

/// Ollama-embedder YAML sub-section.
#[derive(Deserialize, Default)]
#[serde(default)]
pub struct OllamaConfig {
    /// Ollama server URL. Default `http://localhost:11434`.
    pub url: Option<String>,
    /// Embedding model name for the ollama embedder. Defaults to `mxbai-embed-large`
    /// (1024-d → set vector_dim: 1024) when unset.
    pub model: Option<String>,
}

/// Fully-resolved indexing plan (args + config merged).
#[derive(Clone)]
pub struct Plan {
    pub root: String,
    pub ext: Vec<String>,
    pub language: String,
    /// Selected vector backend: "qdrant" or "duckdb".
    pub backend: String,
    /// Selected embedder (duckdb backend only): "ort" or "ollama".
    #[cfg_attr(not(feature = "duckdb"), allow(dead_code))]
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

/// Merge CLI args over the YAML config into a resolved Plan.
pub fn build_plan(args: &Args) -> Result<Plan> {
    let config = load_config(args)?;

    let exclude = build_globset(&config.exclude, "exclude")?;
    let include = build_globset(&config.include, "include")?;
    let include_active = !config.include.is_empty();

    let mut exclude_dirs: HashSet<String> = HARD_PRUNE_DIRS.iter().map(|s| s.to_string()).collect();
    exclude_dirs.extend(config.exclude_dirs.iter().cloned());

    let backend = args
        .backend
        .clone()
        .or(config.backend)
        .unwrap_or_else(|| DEFAULT_BACKEND.to_string());
    let embedder = args
        .embedder
        .clone()
        .or(config.embedder)
        .unwrap_or_else(|| DEFAULT_EMBEDDER.to_string());
    let model = args
        .model
        .clone()
        .or(config.model)
        .unwrap_or_else(|| DEFAULT_MODEL.to_string());

    // Prefix policy: explicit config wins; else auto-detect from the model name.
    let prefix_style = match config.prefix_style {
        Some(s) => PrefixStyle::parse(&s)?,
        None => PrefixStyle::detect(&model),
    };

    // Chunk-size cap: explicit config wins; else model/embedder-aware default. The
    // E5 / Qdrant path keeps the historical 1400-char bound; large-context Ollama
    // models (qwen-style) get a much larger cap so whole functions fit.
    let max_chunk_chars = config
        .max_chunk_chars
        .unwrap_or_else(|| default_cap(&backend, &embedder, &model));

    // Similarity thresholds: config value > built-in default (MCP tool args override at
    // call time). These are model-specific — tune the YAML `similarity:` block per model.
    let sim = &config.similarity;
    let find_similar_min_score = sim
        .find_similar_min_score
        .unwrap_or(DEFAULT_FIND_SIMILAR_MIN_SCORE);
    let duplicate_min_score = sim
        .duplicate_min_score
        .unwrap_or(DEFAULT_DUPLICATE_MIN_SCORE);
    let duplicate_min_cluster_size = sim
        .duplicate_min_cluster_size
        .unwrap_or(DEFAULT_DUPLICATE_MIN_CLUSTER_SIZE);
    let top_k = sim.top_k.unwrap_or(DEFAULT_TOP_K);

    Ok(Plan {
        root: args.root.clone(),
        ext: args
            .ext
            .iter()
            .map(|e| e.trim_start_matches('.').to_string())
            .collect(),
        language: args.language.clone(),
        backend,
        embedder,
        // Chunker defaulting logic:
        // - CLI `--chunker` always wins
        // - Then config file `chunker:`
        // - Otherwise: if we have AST support compiled in *and* this language
        //   is one we have good tree-sitter grammars for → "ast"
        // - Else the safe line-based chunker.
        chunker: if let Some(c) = args.chunker.clone() {
            c
        } else if let Some(c) = config.chunker.clone() {
            c
        } else if cfg!(feature = "ast") && ast_preferred_for_language(&args.language) {
            "ast".to_string()
        } else {
            DEFAULT_CHUNKER.to_string()
        },
        max_chunk_chars,
        prefix_style,
        collection: args
            .collection
            .clone()
            .or(config.collection)
            .unwrap_or_else(|| DEFAULT_COLLECTION.to_string()),
        model,
        vector_dim: config.vector_dim.unwrap_or(DEFAULT_VECTOR_DIM),
        duckdb_path: config
            .duckdb
            .path
            .unwrap_or_else(|| DEFAULT_DUCKDB_PATH.to_string()),
        duckdb_model_cache: config.duckdb.model_cache,
        model_repo: config
            .duckdb
            .model_repo
            .unwrap_or_else(|| DEFAULT_MODEL_REPO.to_string()),
        ollama_url: config
            .ollama
            .url
            .unwrap_or_else(|| DEFAULT_OLLAMA_URL.to_string()),
        ollama_model: config
            .ollama
            .model
            .or_else(|| Some(DEFAULT_OLLAMA_MODEL.to_string())),
        exclude_dirs,
        include,
        include_active,
        exclude,
        skip_generated: config.skip_generated_marker,
        strip_comments: config.strip_comments.unwrap_or(true),
        limit: args.limit,
        find_similar_min_score,
        duplicate_min_score,
        duplicate_min_cluster_size,
        top_k,
    })
}

/// Model-aware default chunk-size cap (chars). The Qdrant/E5 path keeps the historical
/// 1400-char bound; large-context embedders (qwen / generic Ollama models) get the much
/// larger cap so a whole function fits. Detection is by backend + embedder + model name.
fn default_cap(backend: &str, embedder: &str, model: &str) -> usize {
    let m = model.to_ascii_lowercase();
    // Qwen-style instruct embedders have an ~8K-token window → big cap.
    if m.contains("qwen") {
        return CAP_LARGE;
    }
    // E5 (the Qdrant default and the ort default) → small 512-token window.
    if m.contains("e5") {
        return CAP_E5;
    }
    // Otherwise: Ollama models are typically large-context; Qdrant/ort default to E5.
    if backend == "duckdb" && embedder == "ollama" {
        CAP_LARGE
    } else {
        CAP_E5
    }
}

/// Compile a list of glob patterns into a GlobSet.
fn build_globset(patterns: &[String], label: &str) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(Glob::new(pattern).with_context(|| format!("bad {label} glob: {pattern}"))?);
    }
    builder
        .build()
        .with_context(|| format!("failed to compile {label} globs"))
}

/// Load YAML config. Missing default file → built-in defaults; missing explicit file → error.
fn load_config(args: &Args) -> Result<Config> {
    let path = Path::new(&args.config);
    if !path.exists() {
        if args.config == DEFAULT_CONFIG {
            eprintln!(
                "note: no config at {} — using built-in defaults (only hard dirs pruned)",
                DEFAULT_CONFIG
            );
            return Ok(Config::default());
        }
        anyhow::bail!("config file not found: {}", args.config);
    }
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read config: {}", args.config))?;
    serde_yaml::from_str(&raw).with_context(|| format!("failed to parse config: {}", args.config))
}

/// Test-only Plan builders shared across the crate's unit tests (e.g. the MCP helper
/// tests). Keeps a single source of truth for a minimal, network-free `Plan`.
#[cfg(test)]
pub mod test_support {
    use super::*;

    /// A minimal `Plan` with built-in defaults and no globs. Suitable for helper tests
    /// that only read a few fields (collection/model/dim/chunker + similarity knobs).
    pub fn minimal_plan() -> Plan {
        let empty = GlobSetBuilder::new().build().expect("empty globset");
        Plan {
            root: "src".to_string(),
            ext: vec!["ts".to_string()],
            language: "ts".to_string(),
            backend: "mock".to_string(),
            embedder: "ort".to_string(),
            chunker: "lines".to_string(),
            max_chunk_chars: CAP_E5,
            prefix_style: PrefixStyle::E5,
            collection: DEFAULT_COLLECTION.to_string(),
            model: DEFAULT_MODEL.to_string(),
            vector_dim: DEFAULT_VECTOR_DIM,
            duckdb_path: DEFAULT_DUCKDB_PATH.to_string(),
            duckdb_model_cache: None,
            model_repo: DEFAULT_MODEL_REPO.to_string(),
            ollama_url: DEFAULT_OLLAMA_URL.to_string(),
            ollama_model: None,
            exclude_dirs: HashSet::new(),
            include: GlobSetBuilder::new().build().expect("include globset"),
            include_active: false,
            exclude: empty,
            skip_generated: true,
            strip_comments: true,
            limit: 5,
            find_similar_min_score: DEFAULT_FIND_SIMILAR_MIN_SCORE,
            duplicate_min_score: DEFAULT_DUPLICATE_MIN_SCORE,
            duplicate_min_cluster_size: DEFAULT_DUPLICATE_MIN_CLUSTER_SIZE,
            top_k: DEFAULT_TOP_K,
        }
    }
}
