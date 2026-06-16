//! Command orchestration: turns parsed [`Args`] into service calls.
//!
//! This is the layer the binary entrypoint (`main.rs`) dispatches into. It owns the
//! per-command flows (index, sync, flush, query, duplicates, similar, mcp) and the
//! interactive prompts. The CLI commands talk to the two use-case services
//! ([`IndexingService`]/[`QueryService`], built by [`crate::factory::build_services`]) over
//! the `Send + Sync` [`crate::repos::VectorStore`] port — the DuckDB backend's synchronous
//! I/O is confined to a worker thread INSIDE the store, so it never blocks the main Tokio
//! runtime. The pure walk/chunk logic lives in [`crate::indexer`].
//!
//! The MCP server (`run_mcp`) builds the SAME two services via [`crate::factory::build_services`]
//! and hands them to the rmcp `SaiServer` (see [`crate::mcp`]). The one difference from the CLI
//! paths: it does NOT join the DuckDB worker thread on shutdown (it detaches the `JoinHandle`),
//! so a leaked store-handle clone can't wedge server exit after stdio EOF — process exit reaps
//! the thread instead.

use anyhow::{Context, Result};
use std::collections::HashSet;
use std::io::IsTerminal;
use std::process::Command;
use std::thread::JoinHandle;

use crate::cli::{Args, Cmd, SyncArgs};
use crate::config::{Plan, build_plan};
use crate::factory::build_services;
use crate::git;
use crate::indexer;
use crate::service::{IndexingService, QueryService};
use crate::vectordbs::{self, Access, Hit};
// The MCP server (`run_mcp`) wraps each service in an `Arc` for the rmcp `SaiServer`.
#[cfg(feature = "mcp")]
use std::sync::Arc;

/// Run one parsed CLI invocation to completion. The single entrypoint `main` calls.
pub async fn run(args: Args) -> Result<()> {
    // `init` and `update` are config-independent (no plan, no backend) — handle them
    // before any config loading. `init` generates the config, so it must also work
    // when an existing one is broken; `update` works from any directory.
    if let Some(Cmd::Init(init_args)) = &args.command {
        return crate::init::run(init_args);
    }
    if matches!(args.command, Some(Cmd::Update)) {
        return run_update();
    }

    let t0 = std::time::Instant::now();
    let git_ctx = git::capture();
    let plan = build_plan(&args)?;
    // Reject `chunker: ast` when the binary lacks the `ast` feature (clear, actionable
    // error). dry_run is chunker-agnostic, but validating early keeps the message at the
    // top regardless of subcommand.
    indexer::ensure_chunker_available(&plan)?;

    match &args.command {
        // Both dispatched before config loading at the top of this function.
        Some(Cmd::Init(_)) => unreachable!("init is handled before config loading"),
        Some(Cmd::Update) => unreachable!("update is handled before config loading"),
        Some(Cmd::Flush) => {
            run_timed(t0, &git_ctx, "", async {
                let services = build_services(&plan, Access::ReadWrite)?;
                with_services(services, async |indexing, _query| indexing.flush().await).await
            })
            .await
        }
        Some(Cmd::Sync(sync_args)) => {
            run_timed(t0, &git_ctx, "", async {
                let services = build_services(&plan, Access::ReadWrite)?;
                with_services(services, async |indexing, _query| {
                    indexing.ensure_ready(false).await?;
                    sync(indexing, sync_args).await
                })
                .await
            })
            .await
        }
        #[cfg(feature = "mcp")]
        Some(Cmd::Mcp(mcp_args)) => {
            run_timed(t0, &git_ctx, "", async {
                run_mcp(
                    &args,
                    mcp_args.allow_write,
                    mcp_args.allow_setup,
                    mcp_args.http.as_deref(),
                )
                .await
            })
            .await
        }
        #[cfg(any(feature = "duckdb", feature = "qdrant"))]
        Some(Cmd::Duplicates(dup_args)) => {
            run_timed(t0, &git_ctx, "", async {
                let services = build_services(&plan, Access::ReadOnly)?;
                with_services(services, async |_indexing, query| {
                    run_duplicates(query, &plan, dup_args, args.silent).await
                })
                .await
            })
            .await
        }
        #[cfg(any(feature = "duckdb", feature = "qdrant"))]
        Some(Cmd::Similar(sim_args)) => {
            run_timed(t0, &git_ctx, "", async {
                let services = build_services(&plan, Access::ReadOnly)?;
                with_services(services, async |_indexing, query| {
                    run_similar(query, &plan, sim_args).await
                })
                .await
            })
            .await
        }
        None => {
            // Default action: full index of --root.
            if args.dry_run {
                indexer::dry_run(&plan);
                finish(t0, &git_ctx, " (dry-run)");
                return Ok(());
            }
            // The indexing path can offer to wipe a dimension-mismatched DuckDB file and
            // rebuild it. A query-only run never re-indexes, so it just surfaces the error
            // (deleting the index would only leave an empty DB to query).
            run_timed(t0, &git_ctx, "", async {
                let services = if args.query_only {
                    build_services(&plan, Access::ReadWrite)?
                } else {
                    open_index_services(&plan)?
                };
                with_services(services, async |indexing, query| {
                    indexing.ensure_ready(args.recreate).await?;
                    if !args.query_only {
                        index_sources(indexing, &plan, &git_ctx, args.silent).await?;
                    }
                    if let Some(q) = args.query.as_deref() {
                        run_query(query, &plan, q).await?;
                    }
                    Ok(())
                })
                .await
            })
            .await
        }
    }
}

/// Run `f` against the two services, then shut down CLEANLY: drop the services (closing the
/// DuckDB store's channel, which ends its worker loop) and join the worker thread so the
/// backend is dropped before we return — the DuckDB connection checkpoints its WAL on drop,
/// which a bare process exit would skip. Qdrant carries no thread, so the join is skipped.
async fn with_services<T, F>(
    services: (IndexingService, QueryService, Option<JoinHandle<()>>),
    f: F,
) -> Result<T>
where
    F: AsyncFnOnce(&IndexingService, &QueryService) -> Result<T>,
{
    let (indexing, query, thread) = services;
    let result = f(&indexing, &query).await;
    // Drop both services so the LAST `Arc<dyn VectorStore>` clone drops, closing the DuckDB
    // store's channel and ending its worker loop before we join.
    drop(indexing);
    drop(query);
    if let Some(thread) = thread
        && thread.join().is_err()
    {
        tracing::warn!("backend worker thread panicked during shutdown");
    }
    result
}

/// The official cargo-dist release installers — `update` reuses them so binary
/// replacement, PATH handling, and install location stay identical to first install.
#[cfg(not(windows))]
const UPDATE_INSTALLER_SH: &str = "https://github.com/maadgrom/semanticastindexer/releases/latest/download/semanticastindexer-installer.sh";
#[cfg(windows)]
const UPDATE_INSTALLER_PS1: &str = "https://github.com/maadgrom/semanticastindexer/releases/latest/download/semanticastindexer-installer.ps1";
/// GitHub API for the latest release — used by `update` to skip a no-op reinstall.
const RELEASES_API: &str =
    "https://api.github.com/repos/maadgrom/semanticastindexer/releases/latest";

/// Best-effort lookup of the latest published release version via the GitHub API
/// (using `curl`, which the installer already requires). Returns the tag with any
/// leading `v` stripped, or `None` on any failure (offline, rate-limited, unparseable)
/// so `update` falls back to running the installer rather than blocking on the check.
fn latest_release_version() -> Option<String> {
    let out = Command::new("curl")
        .args([
            "-fsSL",
            "-H",
            "Accept: application/vnd.github+json",
            RELEASES_API,
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let tag = json.get("tag_name")?.as_str()?;
    Some(tag.trim_start_matches('v').to_string())
}

/// `update` subcommand (unix): pipe the official release installer through `sh`.
/// POSIX allows replacing a running binary's file, so the new version simply takes
/// effect on the next invocation. Skips the reinstall when already on the latest tag.
// CLI-only `update` command (never on the MCP path); every `println!` here is
// intentional CLI status output, so the whole function opts out of the stdout lint.
#[cfg(not(windows))]
#[allow(clippy::print_stdout)]
fn run_update() -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    match latest_release_version() {
        // Already current: skip the network reinstall entirely.
        Some(latest) if latest == current => {
            println!(
                "{} {current} is already the latest release — nothing to do.",
                env!("CARGO_PKG_NAME")
            );
            return Ok(());
        }
        Some(latest) => println!(
            "{} {current} → {latest} — updating…",
            env!("CARGO_PKG_NAME")
        ),
        // Version check failed (offline / rate-limited): fall back to a plain reinstall.
        None => println!(
            "{} {current} — updating to the latest release…",
            env!("CARGO_PKG_NAME")
        ),
    }
    let status = Command::new("sh")
        .arg("-c")
        .arg(format!("curl -fsSL {UPDATE_INSTALLER_SH} | sh"))
        .status()
        .context("failed to run the release installer (are sh and curl on PATH?)")?;
    anyhow::ensure!(status.success(), "release installer exited with {status}");
    println!("update complete — restart any running MCP servers to pick up the new binary");
    Ok(())
}

/// `update` subcommand (windows): a running executable cannot overwrite itself, so
/// print the exact PowerShell one-liner to run after this process exits.
#[cfg(windows)]
#[allow(clippy::print_stdout)] // intentional CLI status output (never on the MCP path)
fn run_update() -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    if latest_release_version().as_deref() == Some(current) {
        println!(
            "{} {current} is already the latest release — nothing to do.",
            env!("CARGO_PKG_NAME")
        );
        return Ok(());
    }
    println!(
        "{} {current} — a running executable cannot replace itself on Windows.\n\
         Run this in PowerShell to update:\n\n  \
         powershell -c \"irm {UPDATE_INSTALLER_PS1} | iex\"\n",
        env!("CARGO_PKG_NAME")
    );
    Ok(())
}

// Verbosity is governed entirely by the log filter (`--silent` maps to `error`,
// `RUST_LOG` overrides), so this just emits the INFO event and lets the subscriber
// decide whether to show it — keeping one source of truth for log levels.
fn finish(t0: std::time::Instant, ctx: &git::GitContext, extra: &str) {
    let (sha, d) = match &ctx.sha {
        Some(s) => (s.as_str(), if ctx.dirty { ", dirty" } else { "" }),
        None => ("unknown", if ctx.dirty { ", dirty" } else { "" }),
    };
    tracing::info!(
        sha,
        elapsed_s = t0.elapsed().as_secs_f32(),
        "done{}{}",
        extra,
        d
    );
}

/// Internal: run a top-level command future, then always report its wall time (unless --silent).
/// Used so every CLI entrypoint (index, sync, duplicates, flush, mcp, ...) gets consistent timing
/// without repeating the "let r = ...; finish(...); r" pattern in every match arm.
async fn run_timed<F, T>(
    t0: std::time::Instant,
    ctx: &git::GitContext,
    extra: &str,
    f: F,
) -> Result<T>
where
    F: Future<Output = Result<T>>,
{
    let r = f.await;
    finish(t0, ctx, extra);
    r
}

/// Build the services for the indexing path. If opening fails because an existing DuckDB
/// index was built with a different embedding model (dimension mismatch), offer — on an
/// interactive terminal, defaulting to NO — to delete the file and re-index from scratch.
/// Any other error (or a declined prompt) propagates unchanged. Mirrors the old
/// `open_index_backend`, but builds the services (composition root) instead of the `Backend`.
fn open_index_services(
    plan: &Plan,
) -> Result<(IndexingService, QueryService, Option<JoinHandle<()>>)> {
    match build_services(plan, Access::ReadWrite) {
        Ok(services) => Ok(services),
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
            tracing::warn!(path = %path, "deleted mismatched index — re-indexing from scratch");
            build_services(plan, Access::ReadWrite)
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
    #[allow(clippy::print_stdout)]
    // intentional data output (interactive prompt, tty-guarded above)
    {
        print!("{question} [y/N] ");
    }
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
async fn warn_on_dirty(query: &QueryService, silent: bool) -> Result<bool> {
    if silent {
        return Ok(false);
    }
    // Best-effort (column may be absent on indexes created before the stamping feature).
    if !query.has_dirty().await.unwrap_or(false) {
        return Ok(false);
    }
    let msg = "warning: index contains dirty chunks (uncommitted changes). duplicates may reflect a dirty working tree.";
    if std::io::stdin().is_terminal() {
        // Reuse the existing non-destructive "default NO" pattern used by dimension-mismatch prompts.
        if !confirm_default_no(&format!("{} Proceed?", msg))? {
            return Ok(true);
        }
    } else {
        tracing::warn!("{}", msg);
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

/// Walk the root, collect+stamp+upsert chunks (the begin/end_bulk window + the chunk loop now
/// live in [`IndexingService::index_sources`]), printing a single updating progress line to
/// stderr while embedding. This CLI function only supplies the TTY progress CLOSURE and the
/// final summary line — the orchestration is the service's.
async fn index_sources(
    indexing: &IndexingService,
    plan: &Plan,
    ctx: &git::GitContext,
    silent: bool,
) -> Result<()> {
    // The `\r`-driven progress bars below are LIVE TTY output, not logs: they rely on
    // carriage-return overwrite and an ANSI clear-line, which `tracing` (discrete lines)
    // can't render. Gate them on a stderr TTY (so MCP clients capturing stderr to a file
    // never receive `\r`/escape garbage) and on `--silent`. In non-tty contexts the
    // Step-6 INFO spans provide the operation timing instead.
    let show_progress = std::io::stderr().is_terminal() && !silent;

    // One [`IndexProgress`] per upsert batch. Announce a file the first time the reported
    // path changes (the service counts every distinct file crossing into `files_done`; this
    // emits the permanent file line as we cross into a new one), then redraw the single
    // updating "embedded …" line. Mirrors the old in-line bar.
    let mut last_path: Option<String> = None;
    let mut emitted_any = false;
    let mut progress = |p: crate::domain::IndexProgress| {
        if !show_progress {
            return;
        }
        emitted_any = true;
        if last_path.as_deref() != Some(p.path.as_str()) {
            // Clear the in-progress "embedded …" line before the permanent file line.
            eprintln!(
                "\r\x1b[K  [ {}/{} files ] indexing {}",
                p.files_done, p.files_total, p.path
            );
            last_path = Some(p.path.clone());
        }
        // Single updating line on stderr (carriage return, no newline until the end).
        eprint!("\rembedded {}/{} chunks", p.chunks_done, p.chunks_total);
        let _ = std::io::Write::flush(&mut std::io::stderr());
    };

    let report = indexing.index_sources(ctx, &mut progress).await?;

    // Terminate the carriage-return progress bar with a final newline (only emitted when
    // the bar itself was shown — otherwise there is nothing to terminate).
    if emitted_any {
        eprintln!();
    }

    #[allow(clippy::print_stdout)] // intentional data output
    {
        println!(
            "indexed {} chunks from {} {} file(s) into '{}' ({} file(s) skipped by config)",
            report.chunks,
            report.files,
            plan.ext.join("/"),
            plan.collection,
            report.skipped
        );
    }
    Ok(())
}

/// Re-index only changed files: delete each file's existing points, then upload the current
/// content fresh. Deleted/now-excluded files are removed from the collection.
///
/// Delegates to [`IndexingService::refresh`] — one begin/end_bulk window around per-path
/// delete + re-chunk + re-embed + upsert, with the index rebuilt even when a path fails
/// mid-batch — then renders the reconcile report.
// CLI-only `sync` command (never on the MCP path); the whole body is its
// reconcile report, so it opts out of the stdout lint at the function level.
#[allow(clippy::print_stdout)]
async fn sync(indexing: &IndexingService, sync_args: &SyncArgs) -> Result<()> {
    let changed =
        crate::git::changed_files(Some(&sync_args.since), sync_args.staged, &sync_args.files)?;
    if changed.is_empty() {
        println!("sync: no changed files");
        return Ok(());
    }

    let report = indexing.refresh(&changed).await?;

    let (mut reindexed, mut deleted, mut chunks) = (0usize, 0usize, 0usize);
    for (path, outcome) in &report.entries {
        match outcome {
            indexer::ReindexOutcome::Removed { reason } => {
                deleted += 1;
                println!("  - {path} ({reason})");
            }
            indexer::ReindexOutcome::Reindexed { chunks: n } => {
                chunks += n;
                reindexed += 1;
                println!("  + {path} ({n} chunks)");
            }
        }
    }

    println!("sync: {reindexed} file(s) re-indexed ({chunks} chunks), {deleted} file(s) removed");
    Ok(())
}

/// Run the read-only MCP server over stdio. Backend/embedder resolve as `--flag > config >
/// duckdb/ort` (the fully-offline default), so `--config sai-cfg.yml` alone drives the server.
/// Builds the backend + embedder ONCE, opens DuckDB read-only, then serves rmcp until EOF.
#[cfg(feature = "mcp")]
async fn run_mcp(
    args: &Args,
    allow_write: bool,
    allow_setup: bool,
    http: Option<&str>,
) -> Result<()> {
    // The MCP offline defaults (duckdb + ort) sit BELOW the config, not above it: they apply
    // only when neither the flag nor `sai-cfg.yml` sets backend/embedder. See
    // `config::build_mcp_plan`.
    let plan = crate::config::build_mcp_plan(args)?;
    indexer::ensure_chunker_available(&plan)?;
    // --allow-write opens the index WRITABLE (normal `connect`, incl. HNSW persistence) so
    // the `refresh`/`sync` tools can delete + re-embed. Default is read-only: those tools
    // then error. `build_services` shares ONE `Arc<dyn VectorStore>` across both services;
    // the DuckDB worker thread is confined INSIDE that store.
    let (indexing, query, _thread) = build_services(
        &plan,
        if allow_write {
            Access::ReadWrite
        } else {
            Access::ReadOnly
        },
    )?;
    if allow_write {
        indexing.ensure_ready(false).await?;
    }
    // Unlike the CLI paths we DROP/detach the DuckDB worker `JoinHandle` (`_thread`) — we do
    // NOT join it on shutdown. The rmcp service owns the services (and thus the store's handle
    // clones), and a leaked clone must not be able to wedge server exit after stdio EOF; the
    // services drop on EOF (ending the worker) and process exit reaps the thread.
    // Transport: `--http <addr>` serves streamable-HTTP (needs `--features mcp-http`); the
    // default is stdio. Both keep the NON-join worker shutdown above.
    if let Some(addr) = http {
        #[cfg(feature = "mcp-http")]
        {
            return crate::mcp::serve_http(
                Arc::new(indexing),
                Arc::new(query),
                &plan,
                allow_write,
                allow_setup,
                addr,
            )
            .await;
        }
        #[cfg(not(feature = "mcp-http"))]
        {
            let _ = addr;
            anyhow::bail!(
                "`--http` requires a build with `--features mcp-http` (this binary has only the stdio MCP transport)"
            );
        }
    }
    crate::mcp::serve_inner(
        Arc::new(indexing),
        Arc::new(query),
        &plan,
        allow_write,
        allow_setup,
    )
    .await
}

/// Run a semantic query and print the hits exactly as before.
// CLI-only renderer (never on the MCP path): all output is intentional query DATA.
#[allow(clippy::print_stdout)]
async fn run_query(query: &QueryService, plan: &Plan, q: &str) -> Result<()> {
    let hits = query.query(q, plan.limit).await?;
    println!("\ntop {} for: {q}", hits.len());
    for h in &hits {
        print_hit(h);
    }
    Ok(())
}

/// Render one hit: `score  path:start-end`.
#[allow(clippy::print_stdout)] // CLI-only renderer: intentional query DATA
fn print_hit(h: &Hit) {
    println!(
        "  {:.4}  {}:{}-{}",
        h.score, h.path, h.start_line, h.end_line
    );
}

/// Default cap on clusters printed by `duplicates` when `--max-clusters` is omitted.
#[cfg(any(feature = "duckdb", feature = "qdrant"))]
const DEFAULT_DUP_MAX_CLUSTERS: usize = 50;

/// Resolved `duplicates` knobs (CLI flag > config > built-in default), shared by the
/// renderer and the GOLDEN test so the JSON envelope and the human header stay one source.
#[cfg(any(feature = "duckdb", feature = "qdrant"))]
struct DupKnobs {
    min_score: f32,
    min_cluster_size: usize,
    top_k: u64,
    /// `true` when a `--since`/`--staged`/`--file` seed set was derived (the CI-gate mode).
    seeded: bool,
}

/// Resolve the `duplicates` knobs, derive the changed-file `seed_paths` (B1 — the dedup
/// gate's exact code path: `--since`/`--staged`/`--file` → `git::changed_files` → the set
/// of files allowed to SEED a cluster), warn on a dirty tree, then run the scan via
/// [`QueryService::find_duplicates`]. Returns `Ok(None)` when a dirty-tree prompt aborts.
///
/// Extracted so the renderer ([`run_duplicates`]) and the GOLDEN test exercise the IDENTICAL
/// seed-derivation → service path (gate #6).
#[cfg(any(feature = "duckdb", feature = "qdrant"))]
async fn resolve_duplicate_clusters(
    query: &QueryService,
    plan: &Plan,
    args: &crate::cli::DuplicatesArgs,
    silent: bool,
) -> Result<Option<(DupKnobs, Vec<crate::domain::DupCluster>)>> {
    // Knob resolution: CLI flag > config (similarity.*) > built-in default.
    let min_score = args.min_score.unwrap_or_else(|| plan.duplicate_min_score());
    let min_cluster_size = args
        .min_cluster_size
        .unwrap_or_else(|| plan.duplicate_min_cluster_size())
        .max(1);
    let top_k = args.top_k.unwrap_or_else(|| plan.top_k() as u64);
    let max_clusters = args.max_clusters.unwrap_or(DEFAULT_DUP_MAX_CLUSTERS);

    // Changed-file seeding: when --since/--staged/--file is given, only chunks in those
    // files may seed a cluster (the neighbour search still spans the whole index). This is
    // the CI-gate mode — "does the changed code duplicate anything already indexed?" — and
    // it catches new slop that joins a pre-existing cluster, which a count-delta misses.
    let seed_paths = if args.since.is_some() || args.staged || !args.files.is_empty() {
        let changed = crate::git::changed_files(args.since.as_deref(), args.staged, &args.files)?;
        Some(changed.into_iter().collect::<HashSet<String>>())
    } else {
        None
    };
    let seeded = seed_paths.is_some();

    if warn_on_dirty(query, silent).await? {
        return Ok(None);
    }
    let clusters = query
        .find_duplicates(
            min_score,
            min_cluster_size,
            top_k,
            max_clusters,
            args.path_glob.clone(),
            seed_paths,
        )
        .await?;
    Ok(Some((
        DupKnobs {
            min_score,
            min_cluster_size,
            top_k,
            seeded,
        },
        clusters,
    )))
}

/// `duplicates` handler: resolve+scan via [`resolve_duplicate_clusters`], then print the
/// clusters human-readably (or as JSON for CI gates).
// CLI-only renderer (never on the MCP path): all output is intentional cluster DATA
// (human-readable or `--json`), so the whole function opts out of the stdout lint.
#[cfg(any(feature = "duckdb", feature = "qdrant"))]
#[allow(clippy::print_stdout)]
async fn run_duplicates(
    query: &QueryService,
    plan: &Plan,
    args: &crate::cli::DuplicatesArgs,
    silent: bool,
) -> Result<()> {
    let Some((knobs, clusters)) = resolve_duplicate_clusters(query, plan, args, silent).await?
    else {
        return Ok(());
    };
    let DupKnobs {
        min_score,
        min_cluster_size,
        top_k,
        seeded,
    } = knobs;

    // Machine-readable mode for CI gates: emit even when empty so callers branch on `count`.
    if args.json {
        let out = serde_json::json!({
            "min_score": min_score,
            "min_cluster_size": min_cluster_size,
            "top_k": top_k,
            "seeded": seeded,
            "count": clusters.len(),
            "clusters": clusters,
        });
        println!("{}", serde_json::to_string(&out)?);
        return Ok(());
    }

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
/// (flag > config.find_similar_min_score > default), run the nearest-neighbour resolution via
/// [`QueryService::find_similar`], and print `score  path:start-end  symbol`.
// CLI-only renderer (never on the MCP path): all output is intentional neighbour DATA.
#[cfg(any(feature = "duckdb", feature = "qdrant"))]
#[allow(clippy::print_stdout)]
async fn run_similar(
    query: &QueryService,
    plan: &Plan,
    args: &crate::cli::SimilarArgs,
) -> Result<()> {
    use crate::domain::SimilarTarget;

    // Require exactly one of --code or (--path AND --line).
    let target = match (args.code.as_deref(), args.path.as_deref(), args.line) {
        (Some(_), Some(_), _) | (Some(_), _, Some(_)) => {
            anyhow::bail!("similar: provide EITHER --code OR (--path and --line), not both");
        }
        (Some(code), None, None) => SimilarTarget::Code(code.to_string()),
        (None, Some(path), Some(line)) => SimilarTarget::Location {
            path: path.to_string(),
            line,
        },
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

    let hits = query.find_similar(target, args.limit, min_score).await?;

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
    //! Happy-path flow tests driving the REAL CLI orchestration fns (`index_sources`,
    //! `sync`, `run_query`, `run_duplicates`) against the in-memory [`MockStore`] — the
    //! same `Arc<dyn VectorStore>` path production uses, but in-process (NO worker thread,
    //! NO network). Every store call is recorded by the underlying [`MockBackend`] and
    //! asserted here. Source trees + git fixtures are built under a `tempdir`.

    use super::*;
    use crate::config::Plan;
    use crate::repos::mock::MockStore;
    use crate::vectordbs::mock::{MockBackend, MockCalls, MockRow};
    use globset::GlobSetBuilder;
    use std::fs;
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;

    /// Build the two services over a fresh `MockStore` for `plan`, returning them alongside
    /// the shared call recorder (kept before the backend moves into the `Arc<dyn …>`). The
    /// services share ONE store — exactly the `build_services` shape, minus the worker thread.
    fn mock_services(plan: &Plan) -> (IndexingService, QueryService, Arc<Mutex<MockCalls>>) {
        mock_services_from_backend(MockBackend::new(), plan)
    }

    /// As [`mock_services`], but over a pre-seeded backend (rows-with-vectors for the read
    /// side). Both services share the SAME `Arc<MockStore>`.
    fn mock_services_from_backend(
        backend: MockBackend,
        plan: &Plan,
    ) -> (IndexingService, QueryService, Arc<Mutex<MockCalls>>) {
        let calls = backend.calls.clone();
        let store: Arc<dyn crate::repos::VectorStore> = Arc::new(MockStore(backend));
        let indexing = IndexingService::new(store.clone(), plan.clone());
        let query = QueryService::new(store, plan.clone());
        (indexing, query, calls)
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

        let (indexing, _query, calls) = mock_services(&plan);
        index_sources(&indexing, &plan, &git::GitContext::default(), true)
            .await
            .unwrap();

        let calls = calls.lock().unwrap();
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

        let (indexing, _query, calls) = mock_services(&plan);
        sync(&indexing, &sync_args).await.unwrap();

        let calls = calls.lock().unwrap();
        // delete_by_path called once per changed path (3).
        assert_eq!(calls.deletes.len(), 3, "delete fired for each changed path");
        // begin/end_bulk wrap the loop.
        assert_eq!(calls.begin_bulk, 1);
        assert_eq!(calls.end_bulk, 1);
        // Only the indexable (non-test, existing) file produced an upsert.
        assert_eq!(calls.upserts.len(), 1, "only the indexable file upserts");
        assert!(calls.upserts[0].count > 0, "indexable file produced chunks");
    }

    /// flush: the service invokes `store.flush()` exactly once.
    #[tokio::test]
    async fn flush_invokes_backend_flush() {
        let plan = crate::config::test_support::minimal_plan();
        let (indexing, _query, calls) = mock_services(&plan);
        indexing.flush().await.unwrap();
        let calls = calls.lock().unwrap();
        assert_eq!(calls.flush, 1, "flush called exactly once");
    }

    /// refresh (the per-path reindex the CLI `sync` and MCP `refresh` share, now on
    /// [`IndexingService`]): one begin/end_bulk window over a batch of an existing indexable
    /// file (deletes-then-upserts, reported `Reindexed`) and a gone path (delete-only,
    /// reported `Removed`).
    #[tokio::test]
    async fn refresh_reindexes_existing_and_removes_gone() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_string_lossy().to_string();
        let good = dir.path().join("keep.ts");
        fs::write(&good, "export function keep() { return 42 }\n").unwrap();
        let gone = dir.path().join("gone.ts"); // never created

        let plan = test_plan(&root);
        let (indexing, _query, calls) = mock_services(&plan);

        let good_path = good.to_string_lossy().to_string();
        let gone_path = gone.to_string_lossy().to_string();
        let report = indexing
            .refresh(&[good_path.clone(), gone_path.clone()])
            .await
            .unwrap();

        match &report.entries[0].1 {
            indexer::ReindexOutcome::Reindexed { chunks } => {
                assert!(*chunks > 0, "indexable file chunks")
            }
            indexer::ReindexOutcome::Removed { .. } => panic!("existing file must be reindexed"),
        }
        match &report.entries[1].1 {
            indexer::ReindexOutcome::Removed { .. } => {}
            indexer::ReindexOutcome::Reindexed { .. } => panic!("gone file must be removed"),
        }

        let calls = calls.lock().unwrap();
        // one begin/end_bulk window wraps the batch.
        assert_eq!(calls.begin_bulk, 1);
        assert_eq!(calls.end_bulk, 1);
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

    /// query: run_query issues the query through the service and renders the canned hits.
    #[tokio::test]
    async fn query_runs_and_returns_canned_hits() {
        let dir = TempDir::new().unwrap();
        let plan = test_plan(&dir.path().to_string_lossy());
        let (_indexing, query, calls) = mock_services(&plan);

        // run_query renders to stdout; assert it completes and recorded the query.
        run_query(&query, &plan, "where is alpha").await.unwrap();
        // Independently confirm the canned result set is deterministic.
        let hits = query.query("where is alpha", plan.limit).await.unwrap();

        let calls = calls.lock().unwrap();
        assert_eq!(calls.queries.len(), 2, "run_query + direct query");
        assert_eq!(calls.queries[0], "where is alpha");
        assert_eq!(hits.len(), 2, "canned hits returned");
        assert_eq!(hits[0].path, "src/alpha.ts");
        assert!(hits[0].score >= hits[1].score, "hits ordered by score");
    }

    /// Serialises the cwd-mutating GOLDEN test (process-global cwd) so it can't race other
    /// tests that read `std::env::current_dir`.
    static CWD_LOCK: Mutex<()> = Mutex::new(());

    /// Run `git` in `dir`, asserting success (test fixture helper).
    fn git(dir: &std::path::Path, args: &[&str]) {
        let ok = Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .expect("git runs")
            .success();
        assert!(ok, "git {args:?} failed");
    }

    /// The four seed rows: `new.ts` ~ `existing.ts` (a near-identical pair) + an untouched
    /// `old_a.ts` ~ `old_b.ts` pair. Paths match what `git diff --name-only HEAD` emits.
    fn dup_fixture_rows() -> Vec<MockRow> {
        vec![
            MockRow::new(1, "new.ts", 1, vec![1.0, 0.0, 0.0, 0.0]),
            MockRow::new(2, "existing.ts", 1, vec![0.999, 0.01, 0.0, 0.0]),
            MockRow::new(3, "old_a.ts", 1, vec![0.0, 0.0, 1.0, 0.0]),
            MockRow::new(4, "old_b.ts", 1, vec![0.0, 0.0, 0.999, 0.01]),
        ]
    }

    /// GOLDEN (B1 — the dedup gate's exact code path): `duplicates --since` over a git fixture
    /// with a KNOWN near-identical pair (one file CHANGED since the baseline commit, the other
    /// untouched) seeds correctly THROUGH the new CLI → service path. The shared
    /// [`resolve_duplicate_clusters`] (which `run_duplicates` renders) derives `seed_paths`
    /// from `git diff --name-only HEAD` and passes it to `QueryService::find_duplicates`; the
    /// seeded pair's cluster surfaces, and an untouched-only pre-existing near-dup pair does
    /// NOT seed (it only surfaces in a full, unseeded scan).
    ///
    /// A plain `#[test]` (not `#[tokio::test]`): it sets the PROCESS cwd into the fixture so
    /// `git::changed_files` reads the right repo, serialised by [`CWD_LOCK`] and driven on a
    /// scoped current-thread runtime so cwd stays stable for the whole flow. The MockStore
    /// rows are keyed by the SAME relative paths git reports, so the seed restriction is
    /// exercised end-to-end with NO network and NO real embedder.
    #[test]
    fn duplicates_since_seeds_through_cli_service_path() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();

        // A baseline git repo: commit BOTH the "existing" duplicate target and the untouched
        // pre-existing pair, then MODIFY `new.ts` so it shows up in `git diff HEAD`.
        git(root, &["init", "-q"]);
        git(root, &["config", "user.email", "t@t"]);
        git(root, &["config", "user.name", "t"]);
        fs::write(root.join("new.ts"), "export const a = 1\n").unwrap();
        fs::write(root.join("existing.ts"), "export const b = 1\n").unwrap();
        fs::write(root.join("old_a.ts"), "export const c = 1\n").unwrap();
        fs::write(root.join("old_b.ts"), "export const d = 1\n").unwrap();
        git(root, &["add", "-A"]);
        git(root, &["commit", "-q", "-m", "baseline"]);
        // The change since HEAD: edit new.ts (its path is what `--since HEAD` will report).
        fs::write(root.join("new.ts"), "export const a = 2\n").unwrap();

        let plan = test_plan(&root.to_string_lossy());
        let dup_args = crate::cli::DuplicatesArgs {
            min_score: Some(0.95),
            min_cluster_size: Some(2),
            top_k: Some(10),
            path_glob: None,
            max_clusters: Some(50),
            since: Some("HEAD".to_string()),
            staged: false,
            files: Vec::new(),
            json: false,
        };
        let unseeded_args = crate::cli::DuplicatesArgs {
            since: None,
            ..dup_args.clone()
        };

        // Set cwd into the fixture for the whole synchronous flow (git::changed_files reads
        // it), serialised against other cwd-sensitive tests.
        let _guard = CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::current_dir().expect("cwd readable");
        std::env::set_current_dir(root).expect("chdir into fixture");

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("current-thread runtime");

        let result = rt.block_on(async {
            // Sanity: git reports the changed file the seeding keys off.
            let changed = crate::git::changed_files(Some("HEAD"), false, &[]).unwrap();
            assert!(
                changed.iter().any(|p| p == "new.ts"),
                "git diff HEAD reports the changed seed file, got {changed:?}"
            );

            // Seeded run via the SHARED resolve path: only new.ts seeds → its cluster with
            // existing.ts surfaces; the untouched old_a/old_b pair does NOT seed.
            let (_i, query, _c) =
                mock_services_from_backend(MockBackend::with_rows(dup_fixture_rows()), &plan);
            let (knobs, seeded_clusters) =
                resolve_duplicate_clusters(&query, &plan, &dup_args, true)
                    .await
                    .unwrap()
                    .expect("not aborted on dirty");
            assert!(knobs.seeded, "--since derives a seed set");

            // Control: a full UNSEEDED scan (no --since) reports BOTH pairs — proves the single
            // seeded cluster is the SEED restriction, not a missing pair.
            let (_i, query, _c) =
                mock_services_from_backend(MockBackend::with_rows(dup_fixture_rows()), &plan);
            let (_k, all) = resolve_duplicate_clusters(&query, &plan, &unseeded_args, true)
                .await
                .unwrap()
                .expect("not aborted on dirty");
            (seeded_clusters, all)
        });

        // Restore cwd before any assertion can unwind while the guard is held.
        std::env::set_current_dir(&prev).expect("restore cwd");
        drop(_guard);

        let (seeded_clusters, all) = result;
        assert_eq!(
            seeded_clusters.len(),
            1,
            "exactly the seeded file's cluster surfaces, got {seeded_clusters:?}"
        );
        let paths: Vec<&str> = seeded_clusters[0]
            .members
            .iter()
            .map(|m| m.path.as_str())
            .collect();
        assert!(paths.contains(&"new.ts"), "the changed seed is present");
        assert!(
            paths.contains(&"existing.ts"),
            "the untouched code the seed duplicates is pulled in"
        );
        assert!(
            !paths.contains(&"old_a.ts") && !paths.contains(&"old_b.ts"),
            "the untouched-only pre-existing pair does NOT seed"
        );
        assert_eq!(all.len(), 2, "the full unseeded scan reports both pairs");
    }

    /// dedup-gate smoke (B1/G6): the `duplicates --json` OUTPUT ENVELOPE is unchanged. The
    /// `dedup-gate.yml` consumer parses `{min_score, min_cluster_size, top_k, seeded, count,
    /// clusters}` from the `--json` stdout. Run the duplicates path over a MockStore (NO git
    /// seeding — a plain whole-DB scan), build the EXACT `serde_json::json!` envelope
    /// `run_duplicates` emits from the resolved knobs + clusters, and assert every key is
    /// present with the right type. Locks the gate's contract without a binary E2E (deferred
    /// to US-009).
    #[tokio::test]
    async fn duplicates_json_envelope_shape_over_mockstore() {
        let plan = test_plan(".");
        // No --since/--staged/--file → unseeded whole-DB scan (seeded == false).
        let args = crate::cli::DuplicatesArgs {
            min_score: Some(0.95),
            min_cluster_size: Some(2),
            top_k: Some(10),
            path_glob: None,
            max_clusters: Some(50),
            since: None,
            staged: false,
            files: Vec::new(),
            json: true,
        };

        let (_i, query, _c) =
            mock_services_from_backend(MockBackend::with_rows(dup_fixture_rows()), &plan);
        let (knobs, clusters) = resolve_duplicate_clusters(&query, &plan, &args, true)
            .await
            .unwrap()
            .expect("not aborted on dirty");
        let DupKnobs {
            min_score,
            min_cluster_size,
            top_k,
            seeded,
        } = knobs;

        // The IDENTICAL envelope `run_duplicates` serialises for `--json`.
        let out = serde_json::json!({
            "min_score": min_score,
            "min_cluster_size": min_cluster_size,
            "top_k": top_k,
            "seeded": seeded,
            "count": clusters.len(),
            "clusters": clusters,
        });

        let obj = out.as_object().expect("envelope is a JSON object");
        // Exactly the six keys the dedup-gate consumer parses — no more, no fewer.
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "clusters",
                "count",
                "min_cluster_size",
                "min_score",
                "seeded",
                "top_k"
            ],
            "the duplicates --json envelope keys must stay stable for dedup-gate.yml"
        );
        // Type contract per field.
        assert!(obj["min_score"].is_number(), "min_score is a number");
        assert!(
            obj["min_cluster_size"].is_u64(),
            "min_cluster_size is an integer"
        );
        assert!(obj["top_k"].is_u64(), "top_k is an integer");
        assert_eq!(obj["seeded"], serde_json::json!(false), "unseeded scan");
        assert!(obj["count"].is_u64(), "count is an integer");
        let arr = obj["clusters"].as_array().expect("clusters is an array");
        assert_eq!(
            arr.len() as u64,
            obj["count"].as_u64().unwrap(),
            "count equals clusters.len()"
        );
        // The whole-DB scan over the fixture surfaces both near-identical pairs, and each
        // cluster member carries the path the gate triages on.
        assert_eq!(obj["count"], serde_json::json!(2), "both fixture pairs");
        assert!(
            arr.iter().all(|c| c["members"]
                .as_array()
                .is_some_and(|m| m.iter().all(|mem| mem["path"].is_string()))),
            "every cluster member exposes a string path"
        );
    }
}
