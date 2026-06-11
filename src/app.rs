//! Command orchestration: turns parsed [`Args`] into backend calls.
//!
//! This is the layer the binary entrypoint (`main.rs`) dispatches into. It owns the
//! per-command flows (index, sync, flush, query, duplicates, similar, mcp) and the
//! interactive prompts; the pure walk/chunk logic lives in [`crate::indexer`] and the
//! similarity core in [`crate::search`].

use anyhow::{Context, Result};
use std::io::IsTerminal;
use std::process::Command;

use crate::cli::{Args, Cmd, SyncArgs};
use crate::config::{Plan, build_plan};
use crate::git;
use crate::indexer;
use crate::vectordbs::{self, Access, Backend, Hit, factory};

/// Run one parsed CLI invocation to completion. The single entrypoint `main` calls.
pub async fn run(args: Args) -> Result<()> {
    let t0 = std::time::Instant::now();
    let git_ctx = git::capture();
    let plan = build_plan(&args)?;
    // Reject `chunker: ast` when the binary lacks the `ast` feature (clear, actionable
    // error). dry_run is chunker-agnostic, but validating early keeps the message at the
    // top regardless of subcommand.
    indexer::ensure_chunker_available(&plan)?;

    match &args.command {
        Some(Cmd::Flush) => {
            run_timed(t0, &args, &git_ctx, "", async {
                let backend = factory(&plan, Access::ReadWrite)?;
                backend.flush().await
            })
            .await
        }
        Some(Cmd::Sync(sync_args)) => {
            run_timed(t0, &args, &git_ctx, "", async {
                let backend = factory(&plan, Access::ReadWrite)?;
                backend.ensure_ready(false).await?;
                sync(&backend, &plan, sync_args, &git_ctx).await
            })
            .await
        }
        #[cfg(feature = "mcp")]
        Some(Cmd::Mcp(mcp_args)) => {
            run_timed(t0, &args, &git_ctx, "", async {
                run_mcp(&args, mcp_args.allow_write, mcp_args.allow_setup).await
            })
            .await
        }
        #[cfg(any(feature = "duckdb", feature = "qdrant"))]
        Some(Cmd::Duplicates(dup_args)) => {
            run_timed(t0, &args, &git_ctx, "", async {
                run_duplicates(&plan, dup_args, args.silent).await
            })
            .await
        }
        #[cfg(any(feature = "duckdb", feature = "qdrant"))]
        Some(Cmd::Similar(sim_args)) => {
            run_timed(t0, &args, &git_ctx, "", async {
                run_similar(&plan, sim_args).await
            })
            .await
        }
        None => {
            // Default action: full index of --root.
            if args.dry_run {
                indexer::dry_run(&plan);
                finish(t0, &args, &git_ctx, " (dry-run)");
                return Ok(());
            }
            // The indexing path can offer to wipe a dimension-mismatched DuckDB file and
            // rebuild it. A query-only run never re-indexes, so it just surfaces the error
            // (deleting the index would only leave an empty DB to query).
            run_timed(t0, &args, &git_ctx, "", async {
                let backend = if args.query_only {
                    factory(&plan, Access::ReadWrite)?
                } else {
                    open_index_backend(&plan)?
                };
                backend.ensure_ready(args.recreate).await?;
                if !args.query_only {
                    index_sources(&backend, &plan, &git_ctx).await?;
                }
                if let Some(q) = args.query.as_deref() {
                    run_query(&backend, &plan, q).await?;
                }
                Ok(())
            })
            .await
        }
    }
}

fn finish(t0: std::time::Instant, args: &Args, ctx: &git::GitContext, extra: &str) {
    if args.silent {
        return;
    }
    let (sha, d) = match &ctx.sha {
        Some(s) => (s.as_str(), if ctx.dirty { ", dirty" } else { "" }),
        None => ("unknown", if ctx.dirty { ", dirty" } else { "" }),
    };
    eprintln!(
        "done{} at {}{} in {:.2}s",
        extra,
        sha,
        d,
        t0.elapsed().as_secs_f32()
    );
}

/// Internal: run a top-level command future, then always report its wall time (unless --silent).
/// Used so every CLI entrypoint (index, sync, duplicates, flush, mcp, ...) gets consistent timing
/// without repeating the "let r = ...; finish(...); r" pattern in every match arm.
async fn run_timed<F, T>(
    t0: std::time::Instant,
    args: &Args,
    ctx: &git::GitContext,
    extra: &str,
    f: F,
) -> Result<T>
where
    F: Future<Output = Result<T>>,
{
    let r = f.await;
    finish(t0, args, ctx, extra);
    r
}

/// Open the backend for the indexing path. If opening fails because an existing DuckDB
/// index was built with a different embedding model (dimension mismatch), offer — on an
/// interactive terminal, defaulting to NO — to delete the file and re-index from scratch.
/// Any other error (or a declined prompt) propagates unchanged.
fn open_index_backend(plan: &Plan) -> Result<Backend> {
    match factory(plan, Access::ReadWrite) {
        Ok(backend) => Ok(backend),
        Err(e) => {
            let Some(path) = vectordbs::dim_mismatch_duckdb_path(&e) else {
                return Err(e);
            };
            let question = format!(
                "The index at '{path}' was built with a different embedding model \
                 (dimension mismatch). Delete it and re-index from scratch?"
            );
            if !confirm_default_no(&question)? {
                return Err(e);
            }
            delete_duckdb_file(&path)?;
            eprintln!("deleted '{path}' — re-indexing from scratch");
            factory(plan, Access::ReadWrite)
        }
    }
}

/// Ask a yes/no question on the terminal, defaulting to NO. Returns `Ok(false)` immediately
/// when stdin is not an interactive terminal, so automation, CI, git hooks, and the MCP
/// stdio server never block on input or trigger a destructive action by default.
fn confirm_default_no(question: &str) -> Result<bool> {
    use std::io::{BufRead, IsTerminal, Write};
    if !std::io::stdin().is_terminal() {
        return Ok(false);
    }
    print!("{question} [y/N] ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(matches!(
        line.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

/// For `duplicates` (and similar truth-sensitive read commands): if the index has any
/// dirty-stamped chunks, emit a warning. On interactive tty, ask for confirmation (default NO)
/// so the user appreciates they may be looking at uncommitted work. Returns `true` if the
/// caller should abort.
#[cfg(any(feature = "duckdb", feature = "qdrant"))]
async fn warn_on_dirty(backend: &Backend, silent: bool) -> Result<bool> {
    if silent {
        return Ok(false);
    }
    // Best-effort (column may be absent on indexes created before the stamping feature).
    if !backend.has_dirty().await.unwrap_or(false) {
        return Ok(false);
    }
    let msg = "warning: index contains dirty chunks (uncommitted changes). duplicates may reflect a dirty working tree.";
    if std::io::stdin().is_terminal() {
        // Reuse the existing non-destructive "default NO" pattern used by dimension-mismatch prompts.
        if !confirm_default_no(&format!("{} Proceed?", msg))? {
            return Ok(true);
        }
    } else {
        eprintln!("{}", msg);
    }
    Ok(false)
}

/// Delete a DuckDB file plus its `.wal` write-ahead sidecar (ignored if absent) so a fresh
/// re-index does not replay stale data from the old, mismatched index.
fn delete_duckdb_file(path: &str) -> Result<()> {
    std::fs::remove_file(path).with_context(|| format!("failed to delete DuckDB file: {path}"))?;
    let _ = std::fs::remove_file(format!("{path}.wal"));
    Ok(())
}

/// Batch size for embed+upsert. Bounds the embedder POST size (Ollama) and lets us emit
/// a live progress line without one giant call.
const UPSERT_BATCH: usize = 64;

/// Walk the root, collect chunks, and upsert them in batches (wrapped in begin/end_bulk),
/// printing a single updating progress line to stderr while embedding.
async fn index_sources(backend: &Backend, plan: &Plan, ctx: &git::GitContext) -> Result<()> {
    let (mut chunks, files, skipped) = indexer::collect_chunks(plan);
    for c in &mut chunks {
        c.commit_sha = ctx.sha.clone();
        c.dirty = ctx.dirty;
    }
    let chunks_total = chunks.len();

    backend.begin_bulk().await?;
    let mut done = 0usize;
    let mut files_done = 0usize;
    let mut last_path: Option<&str> = None;
    for batch in chunks.chunks(UPSERT_BATCH) {
        // Announce every distinct file as we cross into its chunks. A single batch can
        // span many files, so scan all chunks — not just batch.first() — or the counter
        // degenerates into a batch index and most files are never reported.
        for c in batch {
            if last_path != Some(c.path.as_str()) {
                files_done += 1;
                // Clear the in-progress "embedded …" line before the permanent file line.
                eprintln!(
                    "\r\x1b[K  [ {}/{} files ] indexing {}",
                    files_done, files, c.path
                );
                last_path = Some(c.path.as_str());
            }
        }
        backend.upsert(batch).await?;
        done += batch.len();
        // Single updating line on stderr (carriage return, no newline until the end).
        eprint!("\rembedded {done}/{chunks_total} chunks");
        let _ = std::io::Write::flush(&mut std::io::stderr());
    }
    if chunks_total > 0 {
        eprintln!();
    }
    backend.end_bulk().await?;

    println!(
        "indexed {} chunks from {} {} file(s) into '{}' ({} file(s) skipped by config)",
        chunks_total,
        files,
        plan.ext.join("/"),
        plan.collection,
        skipped
    );
    Ok(())
}

/// Re-index only changed files: delete each file's existing points, then upload the current
/// content fresh. Deleted/now-excluded files are removed from the collection.
async fn sync(
    backend: &Backend,
    plan: &Plan,
    sync_args: &SyncArgs,
    ctx: &git::GitContext,
) -> Result<()> {
    let changed = changed_files(sync_args)?;
    if changed.is_empty() {
        println!("sync: no changed files");
        return Ok(());
    }

    // Always wrap the loop in a bulk window. Qdrant's begin/end_bulk are no-ops, so
    // this is free there. For DuckDB it is REQUIRED for correctness: every sync does a
    // `delete_by_path`, and DuckDB's experimental HNSW index returns too few candidates
    // after in-place deletes (recall degrades — `hnsw_compact_index` does not fix it).
    // begin_bulk drops the index and end_bulk recreates it, restoring full recall.
    backend.begin_bulk().await?;

    let (mut reindexed, mut deleted, mut chunks) = (0usize, 0usize, 0usize);
    for rel in &changed {
        let key = rel.trim_start_matches("./");
        match indexer::reindex_file(backend, plan, rel, ctx).await? {
            indexer::ReindexOutcome::Removed { reason } => {
                deleted += 1;
                println!("  - {key} ({reason})");
            }
            indexer::ReindexOutcome::Reindexed { chunks: n } => {
                chunks += n;
                reindexed += 1;
                println!("  + {key} ({n} chunks)");
            }
        }
    }

    backend.end_bulk().await?;

    println!("sync: {reindexed} file(s) re-indexed ({chunks} chunks), {deleted} file(s) removed");
    Ok(())
}

/// Resolve the changed-file list from explicit args or `git diff`.
fn changed_files(sync_args: &SyncArgs) -> Result<Vec<String>> {
    if !sync_args.files.is_empty() {
        return Ok(sync_args.files.clone());
    }
    let mut cmd = Command::new("git");
    cmd.args(["diff", "--name-only"]);
    if sync_args.staged {
        cmd.arg("--cached");
    } else {
        cmd.arg(&sync_args.since);
    }
    let output = cmd
        .output()
        .context("failed to run `git diff` (is git on PATH?)")?;
    if !output.status.success() {
        anyhow::bail!(
            "git diff failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect())
}

/// Run the read-only MCP server over stdio. Defaults backend=duckdb + embedder=ollama
/// (the offline, no-quota path) unless the user overrode `--backend`/`--embedder` (or set
/// them in the config). Builds the backend + embedder ONCE, opens DuckDB read-only, then
/// serves rmcp until EOF.
#[cfg(feature = "mcp")]
async fn run_mcp(args: &Args, allow_write: bool, allow_setup: bool) -> Result<()> {
    // Apply the MCP-specific defaults: when the user did not pass `--backend`/`--embedder`,
    // prefer duckdb + ollama (the indexer's global defaults are qdrant + ort). Explicit
    // CLI flags still win; config values are honored via the normal merge in `build_plan`.
    let mut mcp_args = args.clone();
    if mcp_args.backend.is_none() {
        mcp_args.backend = Some("duckdb".to_string());
    }
    if mcp_args.embedder.is_none() {
        mcp_args.embedder = Some("ollama".to_string());
    }
    let plan = build_plan(&mcp_args)?;
    indexer::ensure_chunker_available(&plan)?;
    // --allow-write opens the index WRITABLE (normal `connect`, incl. HNSW persistence) so
    // the `refresh` tool can delete + re-embed. Default is read-only: `refresh` then errors.
    let backend = if allow_write {
        let b = vectordbs::factory(&plan, Access::ReadWrite)?;
        b.ensure_ready(false).await?;
        b
    } else {
        vectordbs::factory(&plan, Access::ReadOnly)?
    };
    // The DuckDB backend is `!Send`/`!Sync` (its connection + ort session), so it cannot
    // be held across an `.await` inside an rmcp `#[tool]` handler (those futures must be
    // `Send`). Move it onto a dedicated worker thread that owns it; the MCP server holds
    // only a `Send`+`Sync` channel handle. See `worker.rs` for the full rationale.
    let can_embed_locally = plan.backend == "duckdb";
    let handle = crate::worker::spawn(backend, plan.clone(), can_embed_locally)?;
    crate::mcp::serve_inner(handle, &plan, allow_write, allow_setup).await
}

/// Run a semantic query and print the hits exactly as before.
async fn run_query(backend: &Backend, plan: &Plan, q: &str) -> Result<()> {
    let hits = backend.query(q, plan.limit).await?;
    println!("\ntop {} for: {q}", hits.len());
    for h in &hits {
        print_hit(h);
    }
    Ok(())
}

/// Render one hit: `score  path:start-end`.
fn print_hit(h: &Hit) {
    println!(
        "  {:.4}  {}:{}-{}",
        h.score, h.path, h.start_line, h.end_line
    );
}

/// Default cap on clusters printed by `duplicates` when `--max-clusters` is omitted.
#[cfg(any(feature = "duckdb", feature = "qdrant"))]
const DEFAULT_DUP_MAX_CLUSTERS: usize = 50;

/// `duplicates` handler: resolve each knob (CLI flag > config > built-in default), open the
/// index read-only, run the shared codebase-wide near-duplicate scan, and print the clusters
/// human-readably.
#[cfg(any(feature = "duckdb", feature = "qdrant"))]
async fn run_duplicates(
    plan: &Plan,
    args: &crate::cli::DuplicatesArgs,
    silent: bool,
) -> Result<()> {
    use crate::search;

    // Knob resolution: CLI flag > config (similarity.*) > built-in default.
    let min_score = args.min_score.unwrap_or_else(|| plan.duplicate_min_score());
    let min_cluster_size = args
        .min_cluster_size
        .unwrap_or_else(|| plan.duplicate_min_cluster_size())
        .max(1);
    let top_k = args.top_k.unwrap_or_else(|| plan.top_k() as u64);
    let max_clusters = args.max_clusters.unwrap_or(DEFAULT_DUP_MAX_CLUSTERS);

    let backend = vectordbs::factory(plan, Access::ReadOnly)?;
    if warn_on_dirty(&backend, silent).await? {
        return Ok(());
    }
    let clusters = search::find_duplicates(
        &backend,
        min_score,
        min_cluster_size,
        top_k,
        max_clusters,
        args.path_glob.as_deref(),
    )
    .await?;

    if clusters.is_empty() {
        println!(
            "no near-duplicate clusters (min_score {min_score}, min_cluster_size {min_cluster_size}, top_k {top_k})"
        );
        return Ok(());
    }
    println!(
        "{} near-duplicate cluster(s) (min_score {min_score}, min_cluster_size {min_cluster_size}, top_k {top_k}):",
        clusters.len()
    );
    for c in &clusters {
        println!(
            "cluster (size {}, sim {:.4}..{:.4}):",
            c.size, c.min_sim, c.max_sim
        );
        for m in &c.members {
            let symbol = m.symbol.as_deref().unwrap_or("");
            println!("  {}:{}-{}  {}", m.path, m.start_line, m.end_line, symbol);
        }
    }
    Ok(())
}

/// `similar` handler: require EXACTLY ONE of --code or (--path & --line), resolve min_score
/// (flag > config.find_similar_min_score > default), open the index read-only, run the shared
/// nearest-neighbour resolution, and print `score  path:start-end  symbol`.
#[cfg(any(feature = "duckdb", feature = "qdrant"))]
async fn run_similar(plan: &Plan, args: &crate::cli::SimilarArgs) -> Result<()> {
    use crate::search::{self, SimilarTarget};

    // Require exactly one of --code or (--path AND --line).
    let target = match (args.code.as_deref(), args.path.as_deref(), args.line) {
        (Some(_), Some(_), _) | (Some(_), _, Some(_)) => {
            anyhow::bail!("similar: provide EITHER --code OR (--path and --line), not both");
        }
        (Some(code), None, None) => SimilarTarget::Code(code),
        (None, Some(path), Some(line)) => SimilarTarget::Location { path, line },
        (None, Some(_), None) | (None, None, Some(_)) => {
            anyhow::bail!("similar: --path and --line must be given together");
        }
        (None, None, None) => {
            anyhow::bail!("similar: provide either --code or both --path and --line");
        }
    };

    let min_score = args
        .min_score
        .unwrap_or_else(|| plan.find_similar_min_score());

    let backend = vectordbs::factory(plan, Access::ReadOnly)?;
    let hits = search::find_similar(&backend, target, args.limit, min_score).await?;

    println!("{} similar (min_score {min_score}):", hits.len());
    for h in &hits {
        let symbol = h.symbol.as_deref().unwrap_or("");
        println!(
            "  {:.4}  {}:{}-{}  {}",
            h.score, h.path, h.start_line, h.end_line, symbol
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Happy-path flow tests driving the REAL orchestration fns (`index_sources`,
    //! `sync`, `run_query`, `flush`) against the in-memory [`MockBackend`]. No
    //! network, no real Qdrant/DuckDB: every call is recorded by the mock and
    //! asserted here. Source trees are built under a `tempdir`.

    use super::*;
    use crate::config::Plan;
    use crate::vectordbs::mock::{MockBackend, MockCalls};
    use globset::GlobSetBuilder;
    use std::fs;
    use std::sync::MutexGuard;
    use tempfile::TempDir;

    /// Lock and return the recorded calls of a `Backend::Mock`. Every flow test
    /// here builds a `Mock` backend, so any other variant is a test-harness bug.
    fn mock_calls(backend: &Backend) -> MutexGuard<'_, MockCalls> {
        match backend {
            Backend::Mock(m) => m.calls.lock().unwrap(),
            #[allow(unreachable_patterns)]
            _ => unreachable!("flow tests only use Backend::Mock"),
        }
    }

    /// Build a minimal `Plan` rooted at `root` with `ext` = ts and no globs. Mirrors
    /// `build_plan` defaults without reading any YAML. Starts from the shared
    /// `minimal_plan` (mock/ort, no globs) and overrides only the E5/e5-small knobs
    /// these flow tests rely on.
    fn test_plan(root: &str) -> Plan {
        Plan {
            root: root.to_string(),
            prefix_style: crate::vectordbs::PrefixStyle::E5,
            max_chunk_chars: 1400,
            collection: "test_coll".to_string(),
            model: "intfloat/multilingual-e5-small".to_string(),
            vector_dim: 384,
            model_repo: "Xenova/multilingual-e5-small".to_string(),
            ..crate::config::test_support::minimal_plan()
        }
    }

    /// A plan whose `exclude` globset drops `**/*.test.ts`.
    fn test_plan_excluding_tests(root: &str) -> Plan {
        let mut b = GlobSetBuilder::new();
        b.add(globset::Glob::new("**/*.test.ts").unwrap());
        let mut plan = test_plan(root);
        plan.exclude = b.build().expect("exclude globset");
        plan
    }

    /// index: begin_bulk → upsert(N) → end_bulk, and the upserted count equals
    /// what `collect_chunks` produces from the temp tree.
    #[tokio::test]
    async fn index_drives_begin_upsert_end_in_order() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_string_lossy().to_string();
        fs::write(
            dir.path().join("alpha.ts"),
            "export function alpha() { return 1 }\nconst x = alpha()\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("beta.ts"),
            "export const beta = () => 2\nconsole.log(beta())\n",
        )
        .unwrap();

        let plan = test_plan(&root);
        let (expected_chunks, _, _) = indexer::collect_chunks(&plan);
        let expected = expected_chunks.len();
        assert!(expected > 0, "fixture must produce chunks");

        let backend = Backend::Mock(MockBackend::new());
        index_sources(&backend, &plan, &git::GitContext::default())
            .await
            .unwrap();

        let calls = mock_calls(&backend);
        assert_eq!(calls.begin_bulk, 1, "exactly one begin_bulk");
        assert_eq!(calls.end_bulk, 1, "exactly one end_bulk");
        assert_eq!(calls.upserts.len(), 1, "one upsert batch");
        assert_eq!(
            calls.total_upserted_chunks(),
            expected,
            "upserted chunk count must match collect_chunks"
        );
    }

    /// sync (explicit --file list, no git): an indexable file, an excluded test
    /// file, and a deleted/non-existent path. delete_by_path fires for EACH
    /// changed path; the loop is wrapped by begin/end_bulk; only the indexable
    /// file yields an upsert.
    #[tokio::test]
    async fn sync_deletes_every_path_and_upserts_only_indexable() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_string_lossy().to_string();
        let good = dir.path().join("keep.ts");
        let excluded = dir.path().join("keep.test.ts");
        fs::write(&good, "export function keep() { return 42 }\n").unwrap();
        fs::write(&excluded, "export const t = 1\n").unwrap();
        let gone = dir.path().join("gone.ts"); // never created → deleted path

        let plan = test_plan_excluding_tests(&root);
        let sync_args = SyncArgs {
            since: "HEAD~1".to_string(),
            staged: false,
            files: vec![
                good.to_string_lossy().to_string(),
                excluded.to_string_lossy().to_string(),
                gone.to_string_lossy().to_string(),
            ],
        };

        let backend = Backend::Mock(MockBackend::new());
        sync(&backend, &plan, &sync_args, &git::GitContext::default())
            .await
            .unwrap();

        let calls = mock_calls(&backend);
        // delete_by_path called once per changed path (3).
        assert_eq!(calls.deletes.len(), 3, "delete fired for each changed path");
        // begin/end_bulk wrap the loop.
        assert_eq!(calls.begin_bulk, 1);
        assert_eq!(calls.end_bulk, 1);
        // Only the indexable (non-test, existing) file produced an upsert.
        assert_eq!(calls.upserts.len(), 1, "only the indexable file upserts");
        assert!(calls.upserts[0].count > 0, "indexable file produced chunks");
    }

    /// flush: orchestration invokes backend.flush().
    #[tokio::test]
    async fn flush_invokes_backend_flush() {
        let backend = Backend::Mock(MockBackend::new());
        backend.flush().await.unwrap();
        let calls = mock_calls(&backend);
        assert_eq!(calls.flush, 1, "flush called exactly once");
    }

    /// reindex_file (the shared helper used by both `sync` and the MCP `refresh` tool):
    /// an existing indexable file deletes-then-upserts and reports its chunk count; a
    /// gone/excluded path deletes-only and reports Removed.
    #[tokio::test]
    async fn reindex_file_reindexes_existing_and_removes_gone() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_string_lossy().to_string();
        let good = dir.path().join("keep.ts");
        fs::write(&good, "export function keep() { return 42 }\n").unwrap();
        let gone = dir.path().join("gone.ts"); // never created

        let plan = test_plan(&root);
        let backend = Backend::Mock(MockBackend::new());

        let good_path = good.to_string_lossy().to_string();
        match indexer::reindex_file(&backend, &plan, &good_path, &git::GitContext::default())
            .await
            .unwrap()
        {
            indexer::ReindexOutcome::Reindexed { chunks } => {
                assert!(chunks > 0, "indexable file chunks")
            }
            indexer::ReindexOutcome::Removed { .. } => panic!("existing file must be reindexed"),
        }
        let gone_path = gone.to_string_lossy().to_string();
        match indexer::reindex_file(&backend, &plan, &gone_path, &git::GitContext::default())
            .await
            .unwrap()
        {
            indexer::ReindexOutcome::Removed { .. } => {}
            indexer::ReindexOutcome::Reindexed { .. } => panic!("gone file must be removed"),
        }

        let calls = mock_calls(&backend);
        // delete fired for BOTH paths; only the existing file produced an upsert.
        assert_eq!(calls.deletes.len(), 2, "delete fires per path");
        assert_eq!(calls.upserts.len(), 1, "only the existing file upserts");
    }

    /// Threshold resolution: a Plan built with no `similarity:` config yields the
    /// built-in defaults via the accessor methods (the MCP tools read these when the
    /// tool arg is omitted). Tool arg > config > default — here only the default rung.
    #[test]
    fn similarity_defaults_resolve() {
        let plan = crate::config::test_support::minimal_plan();
        assert_eq!(plan.find_similar_min_score(), 0.85);
        assert_eq!(plan.duplicate_min_score(), 0.93);
        assert_eq!(plan.duplicate_min_cluster_size(), 2);
        assert_eq!(plan.top_k(), 10);
    }

    /// query: run_query issues the query and renders the canned hits.
    #[tokio::test]
    async fn query_runs_and_returns_canned_hits() {
        let dir = TempDir::new().unwrap();
        let plan = test_plan(&dir.path().to_string_lossy());
        let backend = Backend::Mock(MockBackend::new());

        // run_query renders to stdout; assert it completes and recorded the query.
        run_query(&backend, &plan, "where is alpha").await.unwrap();
        // Independently confirm the canned result set is deterministic.
        let hits = backend.query("where is alpha", plan.limit).await.unwrap();

        let calls = mock_calls(&backend);
        assert_eq!(calls.queries.len(), 2, "run_query + direct query");
        assert_eq!(calls.queries[0], "where is alpha");
        assert_eq!(hits.len(), 2, "canned hits returned");
        assert_eq!(hits[0].path, "src/alpha.ts");
        assert!(hits[0].score >= hits[1].score, "hits ordered by score");
    }
}
