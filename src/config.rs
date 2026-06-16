//! Config loading and plan resolution: YAML config, CLI-merged `Plan`, glob compilation.

use anyhow::{Context, Result};
use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::Deserialize;
use std::collections::HashSet;
use std::path::Path;

use crate::cli::Args;
use crate::vectordbs::PrefixStyle;

/// Standard config filename: what `init` generates and the first name sought when
/// `--config` is omitted.
pub const DEFAULT_CONFIG: &str = "sai-cfg.yml";
/// Default config filenames sought (in order) in the working directory when `--config`
/// is omitted: the standard name, its `.yaml` spelling, then the legacy pre-rename name.
pub const DEFAULT_CONFIG_LOOKUP: &[&str] = &["sai-cfg.yml", "sai-cfg.yaml", "indexer.yaml"];
pub(crate) const DEFAULT_COLLECTION: &str = "source_code";
/// Default model for the Qdrant (server-side inference) path. Crate-visible so the
/// `init` generator stays aligned with the runtime defaults by construction.
pub(crate) const DEFAULT_MODEL: &str = "intfloat/multilingual-e5-small";
/// Recommended default for the `ort` embedder (local ONNX + DuckDB). A code-trained
/// model that produces much better separation for near-duplicate detection than
/// general text models like e5-small. Crate-visible for the `init` generator.
pub(crate) const DEFAULT_ORT_MODEL: &str = "jinaai/jina-embeddings-v2-base-code";
const DEFAULT_VECTOR_DIM: u64 = 384;
pub(crate) const DEFAULT_ORT_VECTOR_DIM: u64 = 768;
const DEFAULT_BACKEND: &str = "qdrant";
/// Default embedder for any non-qdrant (i.e. duckdb) backend: local ONNX. The qdrant
/// backend instead defaults to the `qdrant` server-side-inference value (see
/// `build_plan_with_defaults`, which computes the embedder default from the resolved backend).
const DEFAULT_EMBEDDER: &str = "ort";
const DEFAULT_CHUNKER: &str = "lines";

/// File extensions for which the AST chunker is preferred by default (when the binary
/// was built with the `ast` feature and the user did not explicitly set a chunker
/// via CLI or config). For all other extensions we fall back to the reliable
/// line-based chunker.
///
/// MUST stay in sync with the grammar dispatch in `indexer`'s `ast::try_chunk_ast` —
/// an extension listed here without a grammar there would silently line-chunk.
/// Locked by `ast_dispatch_covers_all_preferred_extensions` in the indexer tests.
pub(crate) const AST_PREFERRED_EXTS: &[&str] = &["ts", "tsx", "rs", "go", "py"];

/// Returns whether we should prefer the AST chunker when no explicit chunker was
/// provided: true if ANY requested extension has a tree-sitter grammar. The chunker
/// itself still dispatches per-file, so a mixed `--ext ts,go` walk AST-parses both
/// and line-chunks anything without a grammar.
fn ast_preferred_for_exts(exts: &[String]) -> bool {
    exts.iter().any(|e| {
        AST_PREFERRED_EXTS
            .iter()
            .any(|&p| p.eq_ignore_ascii_case(e))
    })
}
pub(crate) const DEFAULT_DUCKDB_PATH: &str = ".index/code.duckdb";
pub(crate) const DEFAULT_MODEL_REPO: &str = "Xenova/multilingual-e5-small";
/// Matches `DEFAULT_ORT_MODEL`. The ort embedder downloads from this HF repo.
const DEFAULT_ORT_MODEL_REPO: &str = "jinaai/jina-embeddings-v2-base-code";
pub(crate) const DEFAULT_OLLAMA_URL: &str = "http://localhost:11434";
/// Default Ollama embedding model. mxbai-embed-large is 1024-d → use vector_dim: 1024.
/// Crate-visible for the `init` generator.
pub(crate) const DEFAULT_OLLAMA_MODEL: &str = "mxbai-embed-large";
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
    /// Who computes embeddings — scoped to THIS config's `backend`: `ort`/`ollama` (local) for
    /// any backend, or `qdrant` (server-side inference) for the qdrant backend. CLI `--embedder`
    /// overrides; when unset, the resolved backend's default applies (qdrant → `qdrant`, else `ort`).
    pub embedder: Option<String>,
    /// Chunker: "lines" or "ast" (tree-sitter, needs the `ast` feature).
    /// When not explicitly set, we auto-select "ast" for languages we have good
    /// grammars for (currently ts/tsx/rs/go/py) if the binary was built with --features ast.
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
    /// Honor the `sai-noindexing` marker: chunks carrying it are skipped entirely
    /// (never embedded/stored). Default true.
    pub honor_noindex_marker: Option<bool>,
    /// Honor the `sai-noduplicate` marker: chunks carrying it are still indexed and
    /// searchable but excluded from near-duplicate clustering. Default true.
    pub honor_noduplicate_marker: Option<bool>,
    /// DuckDB backend settings (path + ONNX model cache/repo).
    pub duckdb: DuckDbConfig,
    /// Ollama embedder settings (url + model).
    pub ollama: OllamaConfig,
    /// Qdrant-backend settings. Only the non-secret `url` is read from YAML; the API key
    /// comes from the `QDRANT_API_KEY` environment variable.
    pub qdrant: QdrantConfig,
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
    /// from. When using the default `ort` embedder this is now the Jina code model
    /// (see DEFAULT_ORT_MODEL_REPO).
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

/// Qdrant-backend YAML sub-section. The API key is a SECRET and is read ONLY from the
/// environment (`QDRANT_API_KEY`), never from YAML — only the non-secret `url` may live here.
#[derive(Deserialize, Default)]
#[serde(default)]
pub struct QdrantConfig {
    /// Qdrant cluster gRPC URL (non-secret). The `QDRANT_URL` env var, if set, overrides this.
    pub url: Option<String>,
}

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

/// Merge CLI args over the YAML config into a resolved Plan.
/// MCP server default backend: duckdb — the fully-offline local path (ONNX model is cached
/// on first run; no daemon, no API quota). Applied only when neither the CLI flag nor the
/// config sets the backend — i.e. `flag > config > this` — so the MCP server honors
/// `backend:` from `sai-cfg.yml`. The embedder default is then derived from the resolved
/// backend (duckdb → `ort`), so the MCP server still defaults to fully-offline `ort`.
const MCP_DEFAULT_BACKEND: &str = "duckdb";

/// Resolve the runtime [`Plan`]: CLI flags win, then `sai-cfg.yml`, then the global
/// defaults (`qdrant` backend → `qdrant` embedder, i.e. server-side inference).
pub fn build_plan(args: &Args) -> Result<Plan> {
    build_plan_with_defaults(args, DEFAULT_BACKEND)
}

/// Like [`build_plan`], but the MCP server's offline default backend (`duckdb`) applies only
/// when neither the CLI flag nor the config specifies the backend. This keeps the config
/// authoritative: `--config sai-cfg.yml` alone is enough to drive the server. The embedder
/// default follows the resolved backend (duckdb → `ort`).
pub fn build_mcp_plan(args: &Args) -> Result<Plan> {
    build_plan_with_defaults(args, MCP_DEFAULT_BACKEND)
}

fn build_plan_with_defaults(args: &Args, backend_default: &str) -> Result<Plan> {
    let config = load_config(args)?;

    let exclude = build_globset(&config.exclude, "exclude")?;
    let include = build_globset(&config.include, "include")?;
    let include_active = !config.include.is_empty();

    let mut exclude_dirs: HashSet<String> = HARD_PRUNE_DIRS.iter().map(|s| s.to_string()).collect();
    exclude_dirs.extend(config.exclude_dirs.iter().cloned());

    // Resolve the storage backend: CLI flag > config > the caller's default.
    let backend = args
        .backend
        .clone()
        .or_else(|| config.backend.clone())
        .unwrap_or_else(|| backend_default.to_string());

    // Embedder resolution is BACKEND-SCOPED. An `embedder:` value describes how the
    // CONFIG's backend embeds, so it must not leak across a `--backend` override: a duckdb
    // config's `ort` carried into a `--backend qdrant` run would silently flip qdrant into
    // local-embed mode (and demand a local embedder for ops that never embed, e.g. flush).
    // Resolution:
    //   1. an explicit CLI `--embedder` always wins;
    //   2. else the config's `embedder`, but ONLY when its backend matches the resolved
    //      backend (or the config names no backend) — otherwise it described a different
    //      backend and is dropped;
    //   3. else the resolved backend's natural default: qdrant → `qdrant` (server-side
    //      inference), every other backend → `ort` (local ONNX).
    let config_embedder_applies = match &config.backend {
        Some(b) => b == &backend,
        None => true,
    };
    let embedder = args
        .embedder
        .clone()
        .or_else(|| {
            if config_embedder_applies {
                config.embedder.clone()
            } else {
                None
            }
        })
        .unwrap_or_else(|| {
            if backend == "qdrant" {
                "qdrant".to_string()
            } else {
                DEFAULT_EMBEDDER.to_string()
            }
        });

    // `embedder: qdrant` is server-side inference and only makes sense for the qdrant
    // backend; a duckdb (or any non-qdrant) backend must embed locally via ort/ollama.
    if embedder == "qdrant" && backend != "qdrant" {
        anyhow::bail!(
            "embedder 'qdrant' (server-side inference) is only valid with backend: qdrant"
        );
    }

    // ort (local ONNX via DuckDB) now defaults to the code-specialized Jina model.
    // All other paths (Qdrant server inference, Ollama) keep the lightweight E5 default.
    let is_ort = embedder == "ort";
    let model = args
        .model
        .clone()
        .or(config.model.clone())
        .unwrap_or_else(|| {
            if is_ort {
                DEFAULT_ORT_MODEL.to_string()
            } else {
                DEFAULT_MODEL.to_string()
            }
        });

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

    // Normalize extensions once (strip any leading dot) so both the walk filter and the
    // chunker auto-selection see the same clean list.
    let ext: Vec<String> = args
        .ext
        .iter()
        .map(|e| e.trim_start_matches('.').to_string())
        .collect();

    Ok(Plan {
        root: args.root.clone(),
        // Chunker defaulting logic:
        // - CLI `--chunker` always wins
        // - Then config file `chunker:`
        // - Otherwise: if we have AST support compiled in *and* any requested extension
        //   has a tree-sitter grammar → "ast" (the chunker dispatches per-file)
        // - Else the safe line-based chunker.
        chunker: if let Some(c) = args.chunker.clone() {
            c
        } else if let Some(c) = config.chunker.clone() {
            c
        } else if cfg!(feature = "ast") && ast_preferred_for_exts(&ext) {
            "ast".to_string()
        } else {
            DEFAULT_CHUNKER.to_string()
        },
        ext,
        backend,
        embedder,
        max_chunk_chars,
        prefix_style,
        collection: args
            .collection
            .clone()
            .or(config.collection)
            .unwrap_or_else(|| DEFAULT_COLLECTION.to_string()),
        model,
        vector_dim: config.vector_dim.unwrap_or(if is_ort {
            DEFAULT_ORT_VECTOR_DIM
        } else {
            DEFAULT_VECTOR_DIM
        }),
        qdrant_url: config.qdrant.url,
        duckdb_path: config
            .duckdb
            .path
            .unwrap_or_else(|| DEFAULT_DUCKDB_PATH.to_string()),
        duckdb_model_cache: config.duckdb.model_cache,
        model_repo: config.duckdb.model_repo.unwrap_or_else(|| {
            if is_ort {
                DEFAULT_ORT_MODEL_REPO.to_string()
            } else {
                DEFAULT_MODEL_REPO.to_string()
            }
        }),
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
        honor_noindex_marker: config.honor_noindex_marker.unwrap_or(true),
        honor_noduplicate_marker: config.honor_noduplicate_marker.unwrap_or(true),
        limit: args.limit,
        find_similar_min_score,
        duplicate_min_score,
        duplicate_min_cluster_size,
        top_k,
    })
}

/// Model-aware default chunk-size cap (chars). The Qdrant/E5 path keeps the historical
/// 1400-char bound; large-context / code models get a much larger cap so a whole function
/// fits. Detection is by backend + embedder + model name.
fn default_cap(backend: &str, embedder: &str, model: &str) -> usize {
    let m = model.to_ascii_lowercase();
    // Qwen-style instruct embedders have an ~8K-token window → big cap.
    if m.contains("qwen") {
        return CAP_LARGE;
    }
    // E5 (the classic Qdrant / light ort default) → small 512-token window.
    if m.contains("e5") {
        return CAP_E5;
    }
    // Jina code models and other modern code embedders: treat as large-context so
    // whole functions are captured when using the ort + duckdb path.
    if m.contains("jina") {
        return CAP_LARGE;
    }
    // Otherwise: Ollama models (and future large models) are typically large-context.
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

/// First default config filename (see [`DEFAULT_CONFIG_LOOKUP`]) that exists in `dir`,
/// or `None` when the directory has no config. `sai-cfg.yml` is the standard and wins
/// over the `.yaml` spelling and the legacy `indexer.yaml`.
pub fn resolve_default_config(dir: &Path) -> Option<std::path::PathBuf> {
    DEFAULT_CONFIG_LOOKUP
        .iter()
        .map(|name| dir.join(name))
        .find(|p| p.is_file())
}

/// A bare default config filename (no directory part) that is simply absent is not an
/// error: `mcp --config sai-cfg.yml` must work in projects that never ran `init`.
fn is_bare_default_config_name(p: &str) -> bool {
    matches!(p, "sai-cfg.yml" | "sai-cfg.yaml")
}

/// Emit the "no config → built-in defaults" note and return the defaults. Shared by the
/// two no-config paths: no `--config` with no default file present, and an absent bare
/// default config name.
fn builtin_defaults_with_note() -> Config {
    tracing::warn!(
        config = DEFAULT_CONFIG,
        "no config found — using built-in defaults (only hard dirs pruned). \
         Run `semanticastindexer init` to generate one."
    );
    Config::default()
}

/// Load YAML config. No `--config` + no default file → built-in defaults; an explicit
/// `--config` path that does not exist → error.
fn load_config(args: &Args) -> Result<Config> {
    let path = match &args.config {
        Some(p) => {
            let path = std::path::PathBuf::from(p);
            if !path.exists() {
                // An absent bare default name is not an error — fall back to built-in
                // defaults like the `None` branch. Any other missing path (a typo, or one
                // with a directory component) still bails.
                if is_bare_default_config_name(p) {
                    return Ok(builtin_defaults_with_note());
                }
                anyhow::bail!("config file not found: {p}");
            }
            path
        }
        None => match resolve_default_config(Path::new(".")) {
            Some(path) => path,
            None => return Ok(builtin_defaults_with_note()),
        },
    };
    let display = path.display();
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read config: {display}"))?;
    serde_yaml_ng::from_str(&raw).with_context(|| format!("failed to parse config: {display}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use tempfile::TempDir;

    /// Resolution order: the standard `sai-cfg.yml` wins over `sai-cfg.yaml`, which
    /// wins over the legacy `indexer.yaml`; an empty directory resolves to None.
    #[test]
    fn default_config_resolution_order() {
        let dir = TempDir::new().unwrap();
        assert_eq!(resolve_default_config(dir.path()), None, "empty dir");

        std::fs::write(dir.path().join("indexer.yaml"), "{}\n").unwrap();
        assert_eq!(
            resolve_default_config(dir.path()).unwrap(),
            dir.path().join("indexer.yaml"),
            "legacy name is honored when alone"
        );

        std::fs::write(dir.path().join("sai-cfg.yaml"), "{}\n").unwrap();
        assert_eq!(
            resolve_default_config(dir.path()).unwrap(),
            dir.path().join("sai-cfg.yaml"),
            ".yaml spelling beats the legacy name"
        );

        std::fs::write(dir.path().join("sai-cfg.yml"), "{}\n").unwrap();
        assert_eq!(
            resolve_default_config(dir.path()).unwrap(),
            dir.path().join("sai-cfg.yml"),
            "the standard sai-cfg.yml wins over everything"
        );
    }

    /// A directory named like a config file must not satisfy the lookup.
    #[test]
    fn default_config_resolution_ignores_directories() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join("sai-cfg.yml")).unwrap();
        assert_eq!(resolve_default_config(dir.path()), None);
    }

    /// A bare default config name (no directory component) is the only case that soft-falls
    /// back to built-in defaults when the file is absent. The legacy name, any name with a
    /// directory component, and unrelated names all return false (those still bail).
    #[test]
    fn bare_default_config_name_recognition() {
        assert!(is_bare_default_config_name("sai-cfg.yml"));
        assert!(is_bare_default_config_name("sai-cfg.yaml"));
        // Legacy name does NOT soft-fallback.
        assert!(!is_bare_default_config_name("indexer.yaml"));
        // Has a directory component → not bare.
        assert!(!is_bare_default_config_name("configs/sai-cfg.yml"));
        assert!(!is_bare_default_config_name("other.yml"));
    }

    /// Build a Plan from inline YAML written to a temp config file (no network).
    fn plan_from_yaml(yaml: &str) -> Result<Plan> {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("sai-cfg.yml");
        std::fs::write(&path, yaml).unwrap();
        let args = Args::try_parse_from(["sai", "--config", path.to_str().unwrap()]).unwrap();
        build_plan(&args)
    }

    /// Backend-aware embedder default: a qdrant config WITHOUT an explicit `embedder`
    /// resolves to the `qdrant` (server-side inference) embedder and does NOT embed
    /// locally — today's exact Cloud behavior, now keyed on the embedder.
    #[test]
    fn qdrant_backend_defaults_to_qdrant_embedder() {
        let plan = plan_from_yaml("backend: qdrant\n").unwrap();
        assert_eq!(plan.embedder, "qdrant");
        assert!(
            !plan.can_embed_locally(),
            "the qdrant (server-side) embedder must keep the Document path"
        );
        // e5/384 preserved as the server default (is_ort false → DEFAULT_MODEL).
        assert_eq!(plan.model, DEFAULT_MODEL);
        assert_eq!(plan.vector_dim, DEFAULT_VECTOR_DIM);
    }

    /// `embedder: ort` on the qdrant backend → local-embed mode (jina/768).
    #[test]
    fn qdrant_with_ort_embedder_enables_local_embed() {
        let plan = plan_from_yaml("backend: qdrant\nembedder: ort\n").unwrap();
        assert_eq!(plan.embedder, "ort");
        assert!(plan.can_embed_locally());
        assert_eq!(plan.model, DEFAULT_ORT_MODEL);
        assert_eq!(plan.vector_dim, DEFAULT_ORT_VECTOR_DIM);
    }

    /// `embedder: ollama` on the qdrant backend → local-embed mode.
    #[test]
    fn qdrant_with_ollama_embedder_enables_local_embed() {
        let plan = plan_from_yaml("backend: qdrant\nembedder: ollama\n").unwrap();
        assert_eq!(plan.embedder, "ollama");
        assert!(plan.can_embed_locally());
    }

    /// Backend-scoped embedder resolution: a config's `embedder` describes its OWN backend,
    /// so a `--backend qdrant` override of a duckdb config must NOT leak that config's `ort`
    /// into the qdrant run — it resolves to qdrant's natural `qdrant` (server-side) default.
    /// This is the regression the dedup-gate's `flush` cleanup hit; it means flush (and any
    /// non-embedding op) needs no local embedder on a server-side qdrant override.
    #[test]
    fn config_embedder_does_not_leak_across_backend_override() {
        let args = Args::try_parse_from(["sai", "--backend", "qdrant"]).unwrap();
        // Stand in for the auto-discovered repo-root duckdb config.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("sai-cfg.yml");
        std::fs::write(&path, "backend: duckdb\nembedder: ort\n").unwrap();
        let args = Args {
            config: Some(path.to_string_lossy().to_string()),
            ..args
        };
        let plan = build_plan(&args).unwrap();
        assert_eq!(plan.backend, "qdrant");
        assert_eq!(
            plan.embedder, "qdrant",
            "the duckdb config's `ort` must not leak into a --backend qdrant run"
        );
        assert!(
            !plan.can_embed_locally(),
            "server-side qdrant — flush/index need no local embedder"
        );
    }

    /// But an `embedder` written in a config whose backend MATCHES the resolved backend is
    /// honored: a real qdrant config asking for local `ort` embedding still gets it.
    #[test]
    fn config_embedder_applies_when_backend_matches() {
        let plan = plan_from_yaml("backend: qdrant\nembedder: ort\n").unwrap();
        assert_eq!(plan.embedder, "ort");
        assert!(
            plan.can_embed_locally(),
            "qdrant + ort = local-embed, as written"
        );
    }

    /// `embedder: qdrant` with a non-qdrant backend is a hard validation error.
    #[test]
    fn qdrant_embedder_with_duckdb_backend_errors() {
        // `Plan` is not `Debug`, so match the `Err` directly instead of `unwrap_err()`.
        let err = match plan_from_yaml("backend: duckdb\nembedder: qdrant\n") {
            Ok(_) => panic!("expected a validation error for embedder qdrant + backend duckdb"),
            Err(e) => e.to_string(),
        };
        assert!(
            err.contains(
                "embedder 'qdrant' (server-side inference) is only valid with backend: qdrant"
            ),
            "got: {err}"
        );
    }

    /// The duckdb backend with no explicit embedder defaults to `ort` (local embed).
    #[test]
    fn duckdb_backend_defaults_to_ort_embedder() {
        let plan = plan_from_yaml("backend: duckdb\n").unwrap();
        assert_eq!(plan.embedder, "ort");
        assert!(plan.can_embed_locally());
        assert_eq!(plan.model, DEFAULT_ORT_MODEL);
        assert_eq!(plan.vector_dim, DEFAULT_ORT_VECTOR_DIM);
    }

    /// `can_embed_locally()` keys purely on the embedder value.
    #[test]
    fn can_embed_locally_by_embedder() {
        let mut plan = test_support::minimal_plan();

        plan.embedder = "ort".to_string();
        assert!(plan.can_embed_locally(), "ort embeds locally");

        plan.embedder = "ollama".to_string();
        assert!(plan.can_embed_locally(), "ollama embeds locally");

        plan.embedder = "qdrant".to_string();
        assert!(
            !plan.can_embed_locally(),
            "the qdrant embedder is server-side inference"
        );
    }
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
            backend: "mock".to_string(),
            qdrant_url: None,
            embedder: "ort".to_string(),
            chunker: "lines".to_string(),
            max_chunk_chars: CAP_LARGE, // Jina code model default for ort
            prefix_style: PrefixStyle::None, // Jina is symmetric
            collection: DEFAULT_COLLECTION.to_string(),
            model: DEFAULT_ORT_MODEL.to_string(),
            vector_dim: DEFAULT_ORT_VECTOR_DIM,
            duckdb_path: DEFAULT_DUCKDB_PATH.to_string(),
            duckdb_model_cache: None,
            model_repo: DEFAULT_ORT_MODEL_REPO.to_string(),
            ollama_url: DEFAULT_OLLAMA_URL.to_string(),
            ollama_model: None,
            exclude_dirs: HashSet::new(),
            include: GlobSetBuilder::new().build().expect("include globset"),
            include_active: false,
            exclude: empty,
            skip_generated: true,
            strip_comments: true,
            honor_noindex_marker: true,
            honor_noduplicate_marker: true,
            limit: 5,
            find_similar_min_score: DEFAULT_FIND_SIMILAR_MIN_SCORE,
            duplicate_min_score: DEFAULT_DUPLICATE_MIN_SCORE,
            duplicate_min_cluster_size: DEFAULT_DUPLICATE_MIN_CLUSTER_SIZE,
            top_k: DEFAULT_TOP_K,
        }
    }

    /// A `Plan` for the DuckDB backend with the Ollama embedder, pointed at
    /// `duckdb_path` with the chosen `dim`. Captures the fields the duckdb test
    /// builders share (backend "duckdb", embedder "ollama", ollama_model
    /// "nomic-embed-text", skip_generated false). For the fields that differ
    /// between callers (ext/prefix_style/collection/model/model_repo) this picks
    /// the validation-test defaults; callers override via struct-update syntax.
    pub fn duckdb_plan(duckdb_path: &std::path::Path, dim: u64) -> Plan {
        Plan {
            backend: "duckdb".to_string(),
            embedder: "ollama".to_string(),
            collection: "test_validation".to_string(),
            model: "intfloat/multilingual-e5-small".to_string(),
            vector_dim: dim,
            prefix_style: PrefixStyle::E5,
            model_repo: "Xenova/multilingual-e5-small".to_string(),
            duckdb_path: duckdb_path.to_string_lossy().to_string(),
            ollama_model: Some("nomic-embed-text".to_string()),
            skip_generated: false,
            max_chunk_chars: 1400,
            ..minimal_plan()
        }
    }
}
