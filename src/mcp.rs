//! MCP server (`semanticastindexer mcp`, feature = "mcp").
//!
//! Exposes the indexer's semantic search to Claude over **stdio** via the official Rust
//! MCP SDK (`rmcp`). Search is read-only; the `sai_refresh`/`sai_sync` write tools are gated
//! behind `--allow-write`. Backend + Embedder are built ONCE at startup (offline defaults
//! backend=duckdb, embedder=ort, resolved as flag > `sai-cfg.yml` > default) and shared across
//! tool calls behind an `Arc<Mutex>` (DuckDB's connection is single-threaded).
//!
//! Tools (structured JSON output; all prefixed `sai_` to namespace them in the agent's
//! tool list):
//! - `sai_search_code`     — embed query → nearest hits (+language/path_glob post-filter).
//! - `sai_find_similar`    — neighbours of a snippet (`code`) or an existing chunk (`path`+`line`).
//! - `sai_find_duplicates` — codebase-wide near-duplicate clusters via NN + union-find.
//! - `sai_index_status`    — backend/collection/model/dim/chunk_count/chunker.
//! - `sai_refresh`         — re-index specific paths (write; `--allow-write`).
//! - `sai_sync`            — reconcile the index with the working tree, like CLI `sync` (write).
//!
//! Embedding semantics (correctness-critical):
//! - `sai_search_code`            → embed as QUERY.
//! - `sai_find_similar { code }`  → embed as PASSAGE (code-vs-code space).
//! - `sai_find_similar {path,line}` / `sai_find_duplicates` → STORED vectors, no re-embed.
//!
//! Concurrency: the DuckDB backend is `!Send`/`!Sync`, but rmcp's tool-handler futures
//! must be `Send`. The backend therefore lives on a dedicated worker thread INSIDE the
//! `DuckDbStore` (a closure-mailbox behind the `Send`+`Sync` [`crate::repos::VectorStore`]
//! port), so this server holds only `Arc<IndexingService>` + `Arc<QueryService>` — both
//! `Send`+`Sync` — and every handler future is `Send` as rmcp requires.

use std::sync::Arc;

use anyhow::Result;
use rmcp::{
    ErrorData as McpError, ServerHandler, ServiceExt,
    handler::server::router::tool::ToolRouter,
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
    transport::stdio,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::config::Plan;
use crate::domain::{Hit, SimilarTarget};
use crate::indexer::ReindexOutcome;
use crate::service::{IndexingService, QueryService};

/// Hard cap on any caller-supplied `limit`/`top_k` so a tool call can't ask for the world.
const MAX_LIMIT: u64 = 50;
/// Default result count for `search_code` / `find_similar`.
const DEFAULT_LIMIT: u64 = 8;
/// Snippet line cap (lines) when `include_text` is false.
const SNIPPET_LINES: usize = 8;
/// Snippet byte cap (chars) when `include_text` is false.
const SNIPPET_CHARS: usize = 800;
/// `find_duplicates` default cap on returned clusters (the cluster-size / top-k / min-score
/// defaults now come from the config-resolved `similarity:` block, stored on `ServerInner`).
const DEFAULT_DUP_MAX_CLUSTERS: usize = 50;
/// Hard cap on the number of paths a single `refresh` call may touch (bounds the batch).
const MAX_REFRESH_PATHS: usize = 200;

// ---------------------------------------------------------------------------
// Tool input schemas (serde + schemars → JSON Schema for the protocol).
// ---------------------------------------------------------------------------

/// `search_code` input.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchCodeArgs {
    /// Natural-language or code search query.
    pub query: String,
    /// Max results (clamped to 50). Default 8.
    #[serde(default)]
    pub limit: Option<u64>,
    /// Filter by stored language label (e.g. "ts").
    #[serde(default)]
    pub language: Option<String>,
    /// Filter results to paths matching this glob (e.g. "src/**").
    #[serde(default)]
    pub path_glob: Option<String>,
    /// Return the full chunk text instead of a capped snippet. Default false.
    #[serde(default)]
    pub include_text: bool,
}

/// `find_similar` input. Provide EITHER `code` OR (`path` AND `line`).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct FindSimilarArgs {
    /// A code snippet to find neighbours of (embedded as a passage).
    #[serde(default)]
    pub code: Option<String>,
    /// Path of an existing indexed chunk (use with `line`).
    #[serde(default)]
    pub path: Option<String>,
    /// 1-based start line of an existing indexed chunk (use with `path`).
    #[serde(default)]
    pub line: Option<usize>,
    /// Max results (clamped to 50). Default 8.
    #[serde(default)]
    pub limit: Option<u64>,
    /// Drop results scoring below this cosine similarity (omit to see raw scores).
    #[serde(default)]
    pub min_score: Option<f32>,
}

/// `find_duplicates` input.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct FindDuplicatesArgs {
    /// Minimum cosine similarity for an edge to count as a near-duplicate. When omitted,
    /// falls back to the configured `similarity.duplicate_min_score` (tune per model).
    #[serde(default)]
    pub min_score: Option<f32>,
    /// Minimum cluster size to report. When omitted, the configured default.
    #[serde(default)]
    pub min_cluster_size: Option<usize>,
    /// Restrict the scan to paths matching this glob.
    #[serde(default)]
    pub path_glob: Option<String>,
    /// Max clusters to return (largest first). Default 50.
    #[serde(default)]
    pub max_clusters: Option<usize>,
    /// Nearest-neighbour fan-out per chunk (clamped to 50). When omitted, the configured default.
    #[serde(default)]
    pub top_k: Option<u64>,
}

/// `index_status` takes no arguments.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct IndexStatusArgs {}

/// `refresh` input (write tool; requires `--allow-write`).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct RefreshArgs {
    /// File path(s) to re-index. Existing points for each path are deleted first; paths
    /// that still exist and pass the index filters are chunked, embedded, and re-upserted.
    pub paths: Vec<String>,
}

/// `sync` input (write tool; requires `--allow-write`). Reconciles the index with the
/// working tree, like the CLI `sync` command.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SyncArgs {
    /// Git revision to diff against (changed set = working tree vs `<since>`). Default "HEAD~1".
    #[serde(default)]
    pub since: Option<String>,
    /// Reconcile the staged set (`git diff --cached`) instead of `--since`. Default false.
    #[serde(default)]
    pub staged: bool,
    /// Explicit changed path(s) to reconcile; overrides git detection.
    #[serde(default)]
    pub paths: Vec<String>,
}

/// `prepare_mcp_setup` input.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct PrepareMcpSetupArgs {
    /// Target directory to set up semantic code search for (defaults to current working directory).
    #[serde(default)]
    pub target_directory: Option<String>,
    /// Vector backend to use. "duckdb" (recommended for agents) or "qdrant".
    #[serde(default)]
    pub backend: Option<String>,
    /// Embedder when using duckdb backend. "ollama" (lighter) or "ort" (fully offline).
    #[serde(default)]
    pub embedder: Option<String>,
    /// Whether to enable the tree-sitter AST chunker (requires the binary to have been built with --features ast).
    #[serde(default)]
    pub use_ast_chunker: bool,
    /// Whether to install the binary globally into ~/.local/bin (creates a `sai` wrapper).
    #[serde(default)]
    pub install_globally: bool,
    /// If true and the server was started with `--allow-setup`, actually execute the setup script.
    /// Otherwise only returns the exact commands the caller should run.
    #[serde(default)]
    pub execute: bool,
}

// ---------------------------------------------------------------------------
// Tool output rows (serialized into the structured result).
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct SearchHit {
    path: String,
    start_line: usize,
    end_line: usize,
    symbol: Option<String>,
    score: f32,
    snippet: String,
}

// `DupCluster` / `DupMember` (the near-duplicate cluster result shape) and the union-find
// clustering live in the shared `crate::search` module — used by BOTH this MCP tool and the
// CLI `duplicates` subcommand, so the algorithm exists in exactly one place.

// ---------------------------------------------------------------------------
// Server.
// ---------------------------------------------------------------------------

/// The MCP `sai` server. Holds the shared use-case services + plan metadata.
#[derive(Clone)]
pub struct SaiServer {
    inner: Arc<ServerInner>,
    // Consumed by the `#[tool_handler]` macro's generated dispatch; the dead-code lint
    // doesn't see that use (it reads the field via a trait method), so allow it here.
    #[allow(dead_code)]
    tool_router: ToolRouter<SaiServer>,
}

/// Shared state behind the `Arc`. The use-case services dispatch over the `Send`+`Sync`
/// [`crate::repos::VectorStore`] port (the `!Send` DuckDB backend is confined to a worker
/// thread INSIDE the store), so every tool-handler future is `Send` as rmcp requires. Plus
/// cached `Plan` metadata for the `index_status` tool + the gating flags/thresholds.
struct ServerInner {
    indexing: Arc<IndexingService>,
    query: Arc<QueryService>,
    backend_name: String,
    collection: String,
    model: String,
    vector_dim: u64,
    chunker: String,
    /// Whether the backend can embed locally (DuckDB) — Qdrant embeds server-side, so
    /// `search_code` falls back to its text `query()` path there. The worker honors this.
    can_embed_locally: bool,
    /// Whether the server was started with `--allow-write` (the `refresh` tool requires it).
    /// When false, the backend was opened read-only and `refresh` returns a clear error.
    allow_write: bool,
    /// Whether the server was started with `--allow-setup` (allows the `prepare_mcp_setup`
    /// tool to actually execute the mcp-setup script when requested).
    allow_setup: bool,
    /// Resolved similarity-threshold defaults (config value or built-in). MCP tool args
    /// override these per call: tool arg > config > built-in default.
    find_similar_min_score: f32,
    duplicate_min_score: f32,
    duplicate_min_cluster_size: usize,
    duplicate_top_k: u64,
}

#[tool_router]
impl SaiServer {
    /// Build the server from the two use-case services + the resolved plan. `allow_write`
    /// enables the `refresh`/`sync` write tools (the store must have been opened writable by
    /// the caller in that case).
    pub fn new(
        indexing: Arc<IndexingService>,
        query: Arc<QueryService>,
        plan: &Plan,
        allow_write: bool,
        allow_setup: bool,
    ) -> Self {
        let can_embed_locally = plan.can_embed_locally();
        Self {
            inner: Arc::new(ServerInner {
                indexing,
                query,
                backend_name: plan.backend.clone(),
                collection: plan.collection.clone(),
                model: model_label(plan),
                vector_dim: plan.vector_dim,
                chunker: plan.chunker.clone(),
                can_embed_locally,
                allow_write,
                allow_setup,
                find_similar_min_score: plan.find_similar_min_score(),
                duplicate_min_score: plan.duplicate_min_score(),
                duplicate_min_cluster_size: plan.duplicate_min_cluster_size(),
                duplicate_top_k: plan.top_k() as u64,
            }),
            tool_router: Self::tool_router(),
        }
    }

    /// General semantic search over the indexed code.
    #[tool(
        description = "Semantic code search. Embeds the query and returns the nearest indexed code chunks. Optional language/path_glob filters; snippet is capped unless include_text."
    )]
    async fn sai_search_code(
        &self,
        Parameters(args): Parameters<SearchCodeArgs>,
    ) -> Result<CallToolResult, McpError> {
        let limit = clamp_limit(args.limit.unwrap_or(DEFAULT_LIMIT));
        let glob = compile_glob_opt(args.path_glob.as_deref())?;

        // Over-fetch a little so post-filters (language/path_glob) still return `limit`.
        let fetch = clamp_limit(limit.saturating_mul(4).max(limit));
        // The store embeds the query locally (DuckDB) or runs the server-side text query
        // (Qdrant) and returns the NN hits; the service is `Send`+`Sync` so we only see Send
        // types here.
        let hits = self
            .inner
            .query
            .query(&args.query, fetch)
            .await
            .map_err(internal)?;

        let rows: Vec<SearchHit> = hits
            .into_iter()
            .filter(|h| language_ok(h, args.language.as_deref()))
            .filter(|h| glob.as_ref().is_none_or(|g| g.is_match(&h.path)))
            .take(limit as usize)
            .map(|h| to_search_hit(h, args.include_text))
            .collect();

        Ok(CallToolResult::structured(json!({ "hits": rows })))
    }

    /// Neighbours of one function/snippet, with an optional threshold.
    #[tool(
        description = "Find code similar to a snippet (code) or to an existing indexed chunk (path+line, exact stored vector, self-excluded). Provide either code OR path+line. Optional min_score threshold."
    )]
    async fn sai_find_similar(
        &self,
        Parameters(args): Parameters<FindSimilarArgs>,
    ) -> Result<CallToolResult, McpError> {
        let limit = clamp_limit(args.limit.unwrap_or(DEFAULT_LIMIT));

        // Map the args to a `SimilarTarget`, rejecting the no-valid-target shapes BEFORE
        // touching the store. The `code` branch needs a local embedder (Qdrant embeds
        // server-side, so there's no passage embed there) — guard it up front.
        let target = match (args.code.as_deref(), args.path.as_deref(), args.line) {
            (Some(code), _, _) => {
                if !self.inner.can_embed_locally {
                    return Err(McpError::invalid_params(
                        "sai_find_similar { code } requires a local embedder (duckdb backend)",
                        None,
                    ));
                }
                SimilarTarget::Code(code.to_string())
            }
            (None, Some(path), Some(line)) => SimilarTarget::Location {
                path: path.to_string(),
                line,
            },
            _ => {
                return Err(McpError::invalid_params(
                    "sai_find_similar requires either `code` or both `path` and `line`",
                    None,
                ));
            }
        };

        // Threshold resolution: tool arg > config default (stored at startup). find_similar
        // intentionally falls back to the configured min_score so omitting the arg still
        // applies the model-tuned cut; pass an explicit 0.0 to see the raw distribution. The
        // service applies the `min_score` filter internally (no manual post-filter here).
        let min_score = args.min_score.unwrap_or(self.inner.find_similar_min_score);
        // A missing chunk at `path:line` surfaces from the service as an anyhow error
        // "no indexed chunk at {path}:{line}"; preserve today's `invalid_params` shape for
        // that one case (it's a bad caller location, not a server fault). All other failures
        // map to an internal error.
        let hits = match self
            .inner
            .query
            .find_similar(target, limit, min_score)
            .await
        {
            Ok(hits) => hits,
            Err(e) => {
                let msg = e.to_string();
                if msg.starts_with("no indexed chunk at ") {
                    return Err(McpError::invalid_params(msg, None));
                }
                return Err(internal(e));
            }
        };
        let rows: Vec<SearchHit> = hits.into_iter().map(|h| to_search_hit(h, false)).collect();

        Ok(CallToolResult::structured(json!({ "hits": rows })))
    }

    /// Codebase-wide near-duplicate clusters via NN edges + union-find.
    #[tool(
        description = "Find near-duplicate code clusters across the index. For each chunk, takes its top_k neighbours, keeps edges with similarity >= min_score, and unions them into clusters. Returns clusters with size >= min_cluster_size, largest first."
    )]
    async fn sai_find_duplicates(
        &self,
        Parameters(args): Parameters<FindDuplicatesArgs>,
    ) -> Result<CallToolResult, McpError> {
        // Resolution per knob: tool arg > config value (stored at startup) > built-in default.
        let top_k = clamp_limit(args.top_k.unwrap_or(self.inner.duplicate_top_k));
        let min_cluster_size = args
            .min_cluster_size
            .unwrap_or(self.inner.duplicate_min_cluster_size)
            .max(1);
        let min_score = args.min_score.unwrap_or(self.inner.duplicate_min_score);
        let max_clusters = args.max_clusters.unwrap_or(DEFAULT_DUP_MAX_CLUSTERS);
        compile_glob_opt(args.path_glob.as_deref())?; // validate early

        // The service runs the SHARED `crate::search::cluster_duplicates` orchestration
        // (all chunks → per-chunk NN → union-find) over the store — the same code path the
        // CLI `duplicates` subcommand uses.
        let clusters = self
            .inner
            .query
            .find_duplicates(
                min_score,
                min_cluster_size,
                top_k,
                max_clusters,
                args.path_glob.clone(),
                // The MCP server is not git-aware; callers scope with `path_glob` instead
                // of a changed-file seed set (which the CLI `--since` provides).
                None,
            )
            .await
            .map_err(internal)?;

        Ok(CallToolResult::structured(json!({ "clusters": clusters })))
    }

    /// Index metadata for freshness/sanity checks.
    #[tool(
        description = "Report index status: backend, collection, embedding model, vector dimension, total chunk count, and chunker."
    )]
    async fn sai_index_status(
        &self,
        Parameters(_args): Parameters<IndexStatusArgs>,
    ) -> Result<CallToolResult, McpError> {
        let chunk_count = self.inner.query.chunk_count().await.map_err(internal)?;
        Ok(CallToolResult::structured(json!({
            "backend": self.inner.backend_name,
            "collection": self.inner.collection,
            "model": self.inner.model,
            "vector_dim": self.inner.vector_dim,
            "chunk_count": chunk_count,
            "chunker": self.inner.chunker,
        })))
    }

    /// Helps set up semantic code search as an MCP server for agentic tools.
    /// Returns precise commands, configuration snippets, and next steps.
    /// When `execute: true` and the server was started with `--allow-setup`, it will
    /// actually run the setup script (can take a long time due to compilation).
    #[tool(
        description = "Prepare or execute setup of semantic code search (MCP) for a project. Returns ready-to-run commands and MCP server configuration. Supports --allow-setup for actual execution."
    )]
    async fn sai_prepare_mcp_setup(
        &self,
        Parameters(args): Parameters<PrepareMcpSetupArgs>,
    ) -> Result<CallToolResult, McpError> {
        let target = args.target_directory.clone().unwrap_or_else(|| {
            std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| ".".to_string())
        });

        let backend = args.backend.unwrap_or_else(|| "duckdb".to_string());
        let embedder = args.embedder.unwrap_or_else(|| "ollama".to_string());

        // Build the correct feature list from actual inputs.
        let mut features = vec!["mcp".to_string()];
        features.push(backend.clone());
        if backend == "duckdb" {
            features.push(embedder.clone());
        }
        if args.use_ast_chunker {
            features.push("ast".to_string());
        }
        let features_str = features.join(",");

        // Locate mcp-setup/setup.sh relative to the current exe.
        // Try two candidate paths so both source-checkout and installed layouts are covered.
        let setup_script_path = std::env::current_exe().ok().and_then(|exe| {
            let parent = exe.parent()?;
            let candidates = [
                parent.join("../../mcp-setup/setup.sh"),
                parent.join("../mcp-setup/setup.sh"),
            ];
            candidates
                .into_iter()
                .find_map(|c| c.canonicalize().ok().filter(|p| p.exists()))
        });

        let from_source = setup_script_path.is_some();

        let recommended_command = if let Some(script) = &setup_script_path {
            let mut cmd = format!(
                "{} --non-interactive --backend {} --embedder {} --target-dir \"{}\" --features \"{}\"",
                script.to_string_lossy(),
                backend,
                embedder,
                target,
                features_str
            );
            if args.install_globally {
                cmd.push_str(" --install-global");
            }
            cmd
        } else {
            "curl -fsSL https://maadgrom.github.io/semanticastindexer/install.sh | bash".to_string()
        };

        let mcp_config = json!({
            "mcpServers": {
                "sai": {
                    "command": "<path-to-semanticastindexer>",
                    "args": ["mcp", "--config", "sai-cfg.yml"],
                    "cwd": target
                }
            }
        });

        let notes = if from_source {
            "For fully offline use, prefer embedder=ort (much longer first build). \
             The setup script was found next to this binary and the recommended_command \
             is ready to run."
        } else {
            "mcp-setup/setup.sh was not found next to this binary (release install?). \
             The recommended_command is a one-liner that downloads and installs a prebuilt \
             binary and wires up the MCP client. Building from source via mcp-setup/setup.sh \
             requires a source checkout of the repository."
        };

        let mut result = json!({
            "target_directory": target,
            "recommended_command": recommended_command,
            "mcp_server_config_example": mcp_config,
            "next_steps": [
                "1. Run the recommended_command in a terminal (it can take 5-20 minutes the first time).",
                "2. After it finishes, index your project: cd <your-project> && <binary> --dry-run",
                "3. Then run without --dry-run to actually build the index.",
                "4. Add the mcp_server_config_example to your agent's MCP settings.",
                "5. Restart your agentic tool."
            ],
            "notes": notes,
        });

        if args.execute {
            if !from_source {
                // Release install: setup.sh is not present; refuse to pipe curl | bash.
                result["execution_blocked"] = json!(true);
                result["execution_blocked_reason"] = json!(
                    "mcp-setup/setup.sh not found next to this binary (release install?); \
                     run the curl installer manually"
                );
            } else if self.inner.allow_setup {
                // Attempt to execute (this can be very long-running)
                let output = std::process::Command::new("bash")
                    .arg("-c")
                    .arg(&recommended_command)
                    .current_dir(&target)
                    .output();

                match output {
                    Ok(out) => {
                        result["execution_attempted"] = json!(true);
                        result["stdout"] = json!(String::from_utf8_lossy(&out.stdout).to_string());
                        result["stderr"] = json!(String::from_utf8_lossy(&out.stderr).to_string());
                        result["success"] = json!(out.status.success());
                    }
                    Err(e) => {
                        result["execution_attempted"] = json!(true);
                        result["error"] = json!(e.to_string());
                    }
                }
            } else {
                result["execution_blocked"] = json!(true);
                result["execution_blocked_reason"] = json!("Server not started with --allow-setup");
            }
        }

        Ok(CallToolResult::structured(result))
    }

    /// Re-index specific files in place (write tool; requires `--allow-write`).
    #[tool(
        description = "Re-index specific files: delete each path's existing points, then re-chunk + re-embed + upsert files that still exist and pass the index filters (ext, globs, not generated). Gone/excluded paths are just removed. Requires the server to be started with --allow-write."
    )]
    async fn sai_refresh(
        &self,
        Parameters(args): Parameters<RefreshArgs>,
    ) -> Result<CallToolResult, McpError> {
        if !self.inner.allow_write {
            return Err(McpError::invalid_params(
                "server is read-only; restart with --allow-write to enable refresh",
                None,
            ));
        }
        if args.paths.is_empty() {
            return Err(McpError::invalid_params(
                "refresh requires at least one path",
                None,
            ));
        }
        if args.paths.len() > MAX_REFRESH_PATHS {
            return Err(McpError::invalid_params(
                format!("too many paths (max {MAX_REFRESH_PATHS})"),
                None,
            ));
        }

        // The indexing service runs the whole batch in one bulk window (HNSW drop → per-path
        // delete + re-chunk + re-embed + upsert → rebuild), matching the `sync` command's
        // correctness requirement after per-path deletes.
        let report = self
            .inner
            .indexing
            .refresh(&args.paths)
            .await
            .map_err(internal)?;

        Ok(refresh_result(report))
    }

    /// Reconcile the index with the working tree (write tool; requires `--allow-write`).
    #[tool(
        description = "Reconcile the index with the working tree, like the CLI `sync`: re-index files changed since a git revision (default HEAD~1) — re-chunk/re-embed survivors and drop points for deleted or now-excluded paths. Pass `paths` to reconcile an explicit set instead of using git. Requires the server to be started with --allow-write."
    )]
    async fn sai_sync(
        &self,
        Parameters(args): Parameters<SyncArgs>,
    ) -> Result<CallToolResult, McpError> {
        if !self.inner.allow_write {
            return Err(McpError::invalid_params(
                "server is read-only; restart with --allow-write to enable sync",
                None,
            ));
        }
        let since = args.since.as_deref().unwrap_or("HEAD~1");
        let changed =
            crate::git::changed_files(Some(since), args.staged, &args.paths).map_err(internal)?;
        if changed.is_empty() {
            return Ok(CallToolResult::structured(
                json!({ "refreshed": [], "removed": [], "note": "no changed files" }),
            ));
        }
        if changed.len() > MAX_REFRESH_PATHS {
            return Err(McpError::invalid_params(
                format!(
                    "too many changed files ({}; max {MAX_REFRESH_PATHS}). Narrow `since` or pass explicit `paths`.",
                    changed.len()
                ),
                None,
            ));
        }

        // Same one-shot bulk reconcile as `sai_refresh`, over the git-changed set.
        let report = self
            .inner
            .indexing
            .refresh(&changed)
            .await
            .map_err(internal)?;
        Ok(refresh_result(report))
    }
}

#[tool_handler]
impl ServerHandler for SaiServer {
    fn get_info(&self) -> ServerInfo {
        // ServerInfo (= InitializeResult) is #[non_exhaustive], so a cross-crate struct
        // literal is impossible — build via Default + field assignment.
        #[allow(clippy::field_reassign_with_default)]
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.instructions = Some(
            "Semantic code search over the indexed repository. Tools: sai_search_code, \
             sai_find_similar, sai_find_duplicates, sai_index_status (read-only), sai_prepare_mcp_setup \
             (setup helper; execution requires --allow-setup), and the write tools sai_refresh \
             (re-index specific files) and sai_sync (reconcile the index with the working tree) — \
             both require the server to be started with --allow-write."
                .to_string(),
        );
        info
    }
}

/// Serve the MCP server over stdio until EOF. The two use-case services + plan are supplied
/// by `main`. `allow_write` gates the `refresh`/`sync` write tools. `allow_setup` gates
/// execution inside the `prepare_mcp_setup` tool.
pub async fn serve_inner(
    indexing: Arc<IndexingService>,
    query: Arc<QueryService>,
    plan: &Plan,
    allow_write: bool,
    allow_setup: bool,
) -> Result<()> {
    let server = SaiServer::new(indexing, query, plan, allow_write, allow_setup);
    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

/// Clamp a caller-supplied limit to `[1, MAX_LIMIT]`.
fn clamp_limit(n: u64) -> u64 {
    n.clamp(1, MAX_LIMIT)
}

/// Shape a [`RefreshReport`](crate::domain::RefreshReport) into the `{refreshed, removed}`
/// tool result shared by the `sai_refresh` and `sai_sync` write tools.
fn refresh_result(report: crate::domain::RefreshReport) -> CallToolResult {
    let mut refreshed: Vec<serde_json::Value> = Vec::new();
    let mut removed: Vec<String> = Vec::new();
    for (path, outcome) in report.entries {
        match outcome {
            ReindexOutcome::Reindexed { chunks } => {
                refreshed.push(json!({ "path": path, "chunks": chunks }));
            }
            ReindexOutcome::Removed { .. } => removed.push(path),
        }
    }
    CallToolResult::structured(json!({ "refreshed": refreshed, "removed": removed }))
}

/// Map an `anyhow::Error` to an MCP internal error.
fn internal(e: anyhow::Error) -> McpError {
    McpError::internal_error(e.to_string(), None)
}

/// Compile an optional path glob, surfacing a clear `invalid_params` on a bad pattern.
fn compile_glob_opt(pattern: Option<&str>) -> Result<Option<globset::GlobMatcher>, McpError> {
    match pattern {
        None => Ok(None),
        Some(p) => globset::Glob::new(p)
            .map(|g| Some(g.compile_matcher()))
            .map_err(|e| McpError::invalid_params(format!("invalid path_glob '{p}': {e}"), None)),
    }
}

/// Language filter: keep when no filter is set or the hit's language matches.
fn language_ok(hit: &Hit, want: Option<&str>) -> bool {
    match want {
        None => true,
        Some(l) => hit.language == l,
    }
}

/// Build a `SearchHit` row, capping the snippet unless `include_text`.
fn to_search_hit(hit: Hit, include_text: bool) -> SearchHit {
    let snippet = if include_text {
        hit.text.clone()
    } else {
        snippet_of(&hit.text)
    };
    SearchHit {
        path: hit.path,
        start_line: hit.start_line,
        end_line: hit.end_line,
        symbol: hit.symbol,
        score: hit.score,
        snippet,
    }
}

/// First ~8 lines of `text`, capped to ~800 chars (with a trailing ellipsis when cut).
fn snippet_of(text: &str) -> String {
    let head: String = text
        .lines()
        .take(SNIPPET_LINES)
        .collect::<Vec<_>>()
        .join("\n");
    if head.len() <= SNIPPET_CHARS {
        return head;
    }
    // Cap at a char boundary at or before SNIPPET_CHARS.
    let mut end = SNIPPET_CHARS;
    while end > 0 && !head.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &head[..end])
}

/// The human-readable model label for `index_status`: the Ollama model when that
/// embedder is selected (duckdb backend), else the configured `model`.
fn model_label(plan: &Plan) -> String {
    if plan.backend == "duckdb" && plan.embedder == "ollama" {
        plan.ollama_model
            .clone()
            .unwrap_or_else(|| plan.model.clone())
    } else {
        plan.model.clone()
    }
}

#[cfg(test)]
mod tests {
    //! Tool-logic tests against `Backend::Mock` (seeded rows-with-vectors). No real
    //! backend, no network: these prove the search/dedup/self-exclusion/union-find logic
    //! the MCP tools depend on. The macro-generated rmcp glue is exercised by the live
    //! `initialize`/`tools/list` smoke (see README); here we test the pure logic.

    use super::*;
    use crate::vectordbs::mock::{MockRow, seeded};

    /// query_by_vector ranks by cosine similarity (best first), dedups by id, truncates.
    #[tokio::test]
    async fn query_by_vector_orders_by_similarity_and_truncates() {
        let b = seeded(vec![
            MockRow::new(1, "src/a.ts", 1, vec![1.0, 0.0, 0.0, 0.0]),
            MockRow::new(2, "src/b.ts", 1, vec![0.9, 0.1, 0.0, 0.0]),
            MockRow::new(3, "src/c.ts", 1, vec![0.0, 1.0, 0.0, 0.0]),
        ]);
        let hits = b
            .query_by_vector(&[1.0, 0.0, 0.0, 0.0], 2, None)
            .await
            .unwrap();
        assert_eq!(hits.len(), 2, "truncated to limit");
        assert_eq!(hits[0].id, 1, "exact match ranks first");
        assert_eq!(hits[1].id, 2, "near match second");
        assert!(hits[0].score >= hits[1].score, "scores descending");
    }

    /// query_by_vector excludes the self id (find_similar by location / find_duplicates).
    #[tokio::test]
    async fn query_by_vector_excludes_self_id() {
        let b = seeded(vec![
            MockRow::new(1, "src/a.ts", 1, vec![1.0, 0.0, 0.0, 0.0]),
            MockRow::new(2, "src/b.ts", 1, vec![0.99, 0.01, 0.0, 0.0]),
        ]);
        let hits = b
            .query_by_vector(&[1.0, 0.0, 0.0, 0.0], 8, Some(1))
            .await
            .unwrap();
        assert!(hits.iter().all(|h| h.id != 1), "self id excluded");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, 2);
    }

    /// query_by_vector dedups by id (HNSW can surface a duplicate candidate). The mock's
    /// rows are unique, but the dedup contract is still asserted via repeated-id seeding
    /// being impossible — instead we assert no id appears twice in a larger result set.
    #[tokio::test]
    async fn query_by_vector_results_have_unique_ids() {
        let rows: Vec<MockRow> = (1..=10)
            .map(|i| MockRow::new(i, &format!("src/f{i}.ts"), 1, vec![i as f32, 0.0, 0.0, 0.0]))
            .collect();
        let b = seeded(rows);
        let hits = b
            .query_by_vector(&[5.0, 0.0, 0.0, 0.0], 50, None)
            .await
            .unwrap();
        let mut ids: Vec<u64> = hits.iter().map(|h| h.id).collect();
        let before = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(before, ids.len(), "no duplicate ids in results");
    }

    /// get_by_location returns the row + its exact stored vector, or None.
    #[tokio::test]
    async fn get_by_location_returns_row_and_vector() {
        let b = seeded(vec![
            MockRow::new(1, "src/a.ts", 10, vec![0.1, 0.2, 0.3, 0.4]),
            MockRow::new(2, "src/b.ts", 20, vec![0.5, 0.6, 0.7, 0.8]),
        ]);
        let got = b.get_by_location("src/b.ts", 20).await.unwrap();
        let (hit, vec) = got.expect("row present");
        assert_eq!(hit.id, 2);
        assert_eq!(vec, vec![0.5, 0.6, 0.7, 0.8], "exact stored vector");

        let missing = b.get_by_location("src/b.ts", 999).await.unwrap();
        assert!(missing.is_none(), "no chunk at that line");
    }

    // The find_duplicates clustering + union-find tests now live in `crate::search`
    // (the shared core that BOTH this tool and the CLI `duplicates` subcommand use), so
    // they are not duplicated here.

    /// snippet_of caps at ~8 lines / ~800 chars.
    #[test]
    fn snippet_caps_lines() {
        let text = (0..20)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let snip = snippet_of(&text);
        assert_eq!(snip.lines().count(), SNIPPET_LINES, "capped to 8 lines");
    }

    /// clamp_limit pins to [1, 50].
    #[test]
    fn clamp_limit_bounds() {
        assert_eq!(clamp_limit(0), 1);
        assert_eq!(clamp_limit(8), 8);
        assert_eq!(clamp_limit(9999), MAX_LIMIT);
    }
}
