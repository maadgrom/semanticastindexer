//! CLI argument types (clap derive). Shared by the binary entrypoint and the
//! orchestration layer in [`crate::app`], and part of the library surface so
//! integration tests can build an [`Args`] with `Args::try_parse_from`.

use clap::{Parser, Subcommand};

use crate::config::DEFAULT_CONFIG;

#[derive(Parser, Debug, Clone)]
#[command(
    about = "Near-duplicate detection and semantic code search: index source into Qdrant Cloud or local DuckDB, query in natural language, and serve over MCP",
    long_about = "Index source files into a vector backend (Qdrant Cloud server-side inference, or local DuckDB with on-device ONNX/Ollama embeddings) for near-duplicate detection and semantic code search. Run as a CLI or as an MCP server for AI coding agents. Configured by indexer.yaml; flags override config."
)]
pub struct Args {
    /// Subcommand. Omitted = full index of --root (the default).
    #[command(subcommand)]
    pub command: Option<Cmd>,

    /// Root directory to walk for source files.
    #[arg(long, default_value = "src")]
    pub root: String,

    /// File extensions to index (comma-separated, no dots). Each indexed chunk's
    /// `language` payload label is derived from its file's extension (e.g. `.ts` → "ts",
    /// `.tsx` → "tsx"), so `--ext ts,tsx` indexes both with the correct per-file label
    /// and AST grammar.
    #[arg(long, value_delimiter = ',', default_value = "ts,tsx")]
    pub ext: Vec<String>,

    /// Vector backend: "qdrant" or "duckdb" (overrides config). Global: accepted before
    /// or after a subcommand.
    #[arg(long, global = true)]
    pub backend: Option<String>,

    /// Embedder for the duckdb backend: "ort" or "ollama" (overrides config). Global:
    /// accepted before or after a subcommand.
    #[arg(long, global = true)]
    pub embedder: Option<String>,

    /// Chunker: "lines" or "ast" (tree-sitter). When omitted, we auto-select "ast" when any
    /// requested --ext has AST support (ts/tsx/rs/go) *if* the binary was built with --features ast.
    /// Explicit --chunker always wins.
    #[arg(long)]
    pub chunker: Option<String>,

    /// Path to the YAML config controlling exclusions. Global: accepted before or after
    /// a subcommand.
    #[arg(long, default_value = DEFAULT_CONFIG, global = true)]
    pub config: String,

    /// Target collection (overrides config). Global: accepted before or after a subcommand.
    #[arg(long, global = true)]
    pub collection: Option<String>,

    /// Inference model (overrides config).
    #[arg(long)]
    pub model: Option<String>,

    /// Optional semantic query to run after indexing (or alone with --query-only).
    #[arg(long)]
    pub query: Option<String>,

    /// Skip indexing; only run the --query against the existing collection.
    #[arg(long, default_value_t = false)]
    pub query_only: bool,

    /// Drop and recreate the collection before indexing.
    #[arg(long, default_value_t = false)]
    pub recreate: bool,

    /// Walk + report what would be indexed/skipped. No network, no upload.
    #[arg(long, default_value_t = false)]
    pub dry_run: bool,

    /// Number of nearest results to print for a query.
    #[arg(long, default_value_t = 5)]
    pub limit: u64,

    /// Suppress timing, progress, dirty warnings, and non-essential notes (ideal for hooks/CI).
    #[arg(long, global = true, default_value_t = false)]
    pub silent: bool,
}

#[derive(Subcommand, Debug, Clone)]
pub enum Cmd {
    /// Flush the vector storage: delete the entire collection.
    Flush,
    /// Re-index only changed files (for git hooks): delete each file's points, then upload fresh.
    Sync(SyncArgs),
    /// Run the MCP server (semantic code search for Claude) over stdio.
    /// Honors --backend/--embedder/--collection/--config; defaults to duckdb + ollama.
    /// READ-ONLY by default; pass `--allow-write` to enable the `refresh` write tool.
    #[cfg(feature = "mcp")]
    Mcp(McpArgs),
    /// Codebase-wide near-duplicate clusters (NN edges + union-find), printed
    /// human-readably. Uses stored vectors only (no re-embed). Needs a vector backend +
    /// embedder feature (e.g. --features ort / "ollama,ast").
    #[cfg(any(feature = "duckdb", feature = "qdrant"))]
    Duplicates(DuplicatesArgs),
    /// Nearest neighbours of a code snippet (--code, embedded as a passage) or an existing
    /// indexed chunk (--path --line, stored vector, self-excluded). Needs a vector backend
    /// + embedder feature.
    #[cfg(any(feature = "duckdb", feature = "qdrant"))]
    Similar(SimilarArgs),
}

/// `duplicates` subcommand args. Each threshold knob resolves CLI flag > config
/// (`similarity.*`) > built-in default.
#[cfg(any(feature = "duckdb", feature = "qdrant"))]
#[derive(clap::Args, Debug, Clone)]
pub struct DuplicatesArgs {
    /// Minimum cosine similarity for an edge to count as a near-duplicate.
    /// Default: config `similarity.duplicate_min_score` (else 0.93).
    #[arg(long)]
    pub min_score: Option<f32>,
    /// Minimum cluster size to report. Default: config `similarity.duplicate_min_cluster_size` (else 2).
    #[arg(long)]
    pub min_cluster_size: Option<usize>,
    /// Nearest-neighbour fan-out per chunk. Default: config `similarity.top_k` (else 10).
    #[arg(long)]
    pub top_k: Option<u64>,
    /// Restrict the scan to paths matching this glob (e.g. "src/utils/**").
    #[arg(long)]
    pub path_glob: Option<String>,
    /// Max clusters to print (largest first). Default 50.
    #[arg(long)]
    pub max_clusters: Option<usize>,
}

/// `similar` subcommand args. Provide EXACTLY ONE of --code OR (--path AND --line).
#[cfg(any(feature = "duckdb", feature = "qdrant"))]
#[derive(clap::Args, Debug, Clone)]
pub struct SimilarArgs {
    /// A code snippet to find neighbours of (embedded as a PASSAGE — code-vs-code space).
    #[arg(long)]
    pub code: Option<String>,
    /// Path of an existing indexed chunk (use with --line; reuses the stored vector).
    #[arg(long)]
    pub path: Option<String>,
    /// 1-based start line of an existing indexed chunk (use with --path).
    #[arg(long)]
    pub line: Option<usize>,
    /// Max results. Default 8.
    #[arg(long, default_value_t = 8)]
    pub limit: u64,
    /// Drop results scoring below this cosine similarity.
    /// Default: config `similarity.find_similar_min_score` (else 0.85). Pass 0 for raw scores.
    #[arg(long)]
    pub min_score: Option<f32>,
}

#[cfg(feature = "mcp")]
#[derive(clap::Args, Debug, Clone)]
pub struct McpArgs {
    /// Open the index WRITABLE and register the `sai_refresh` tool. Without this flag the
    /// server is read-only and `sai_refresh` returns a clear "restart with --allow-write" error.
    #[arg(long, default_value_t = false)]
    pub allow_write: bool,

    /// Allow the `sai_prepare_mcp_setup` tool to actually execute the mcp-setup script
    /// (can trigger long builds and file modifications). Use with caution.
    #[arg(long, default_value_t = false)]
    pub allow_setup: bool,
}

#[derive(clap::Args, Debug, Clone)]
pub struct SyncArgs {
    /// Git revision to diff against; changed set = `<since>..HEAD`.
    #[arg(long, default_value = "HEAD~1")]
    pub since: String,

    /// Use staged changes (`git diff --cached`) instead of `--since`.
    #[arg(long, default_value_t = false)]
    pub staged: bool,

    /// Explicit changed file path(s), repeatable. Overrides git detection.
    /// Files that exist are re-indexed; files that are gone are just deleted.
    #[arg(long = "file")]
    pub files: Vec<String>,
}
