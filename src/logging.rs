//! Logging facade: install a single `tracing` subscriber that writes every diagnostic
//! to **stderr**.
//!
//! stdout is a typed channel — it carries only JSON-RPC frames (MCP mode) and CLI
//! *data* output (query hits, `--json`, dry-run report, sync summary, `init` result).
//! Everything else (status, progress, warnings, timing, lifecycle) is a log and must
//! flow through here to stderr, so nothing human-readable can corrupt the stdout
//! channel. This is what makes the MCP `--allow-write` stdout fix structural.

use std::io::IsTerminal;

use anyhow::Result;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::format::FmtSpan;

use crate::cli::{Args, LogFormat};

/// Install the global `tracing` subscriber. Idempotent: calling this more than once
/// (e.g. across test cases) is treated as success rather than a panic, because we
/// install with `try_init()`.
///
/// Filter precedence: `RUST_LOG` > flags (`--silent`/`-v`) > default `info`. The
/// default directive is scoped to this crate so dependency noise stays low, with a
/// global `warn` fallback so important upstream warnings (e.g. from rmcp) still show.
pub fn init(args: &Args) -> Result<()> {
    let filter = build_filter(args);

    // ANSI auto-detect: MCP clients capture stderr to a *file* (not a tty); emitting
    // escape codes there would corrupt the captured logs. Only colorize a real tty.
    let ansi = std::io::stderr().is_terminal();

    // Emit a span-close event for each instrumented operation so its DURATION is
    // surfaced (`close time.busy=… time.idle=…`). The key-operation spans are INFO,
    // so per-op timing (index/embed/query/sync/ensure_ready) shows at the default
    // level — this is what makes the instrumentation actually useful for spotting
    // sync/embed performance, not just structured context.
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(ansi)
        .with_span_events(FmtSpan::CLOSE);

    // `try_init()` (not `init()`) so a second install — e.g. when tests call this
    // repeatedly — returns Err instead of panicking; we treat that as success.
    let result = match args.log_format {
        LogFormat::Json => builder.json().try_init(),
        LogFormat::Pretty => builder.try_init(),
    };

    // An already-installed subscriber is not an error for our purposes.
    let _ = result;
    Ok(())
}

/// Resolve the active [`EnvFilter`]: honor `RUST_LOG` if present and valid, otherwise
/// derive a crate-scoped directive from the CLI flags.
fn build_filter(args: &Args) -> EnvFilter {
    // Power-user knob first: `RUST_LOG` (parsed by `try_from_default_env`) wins.
    if let Ok(filter) = EnvFilter::try_from_default_env() {
        return filter;
    }

    // Flags next: `--silent` is the quietest; otherwise verbosity count picks the level.
    let level = if args.silent {
        "error"
    } else {
        match args.verbose {
            0 => "info",
            1 => "debug",
            _ => "trace",
        }
    };

    // Scope the chosen level to this crate; keep a global `warn` floor so dependency
    // noise stays down while upstream warnings still surface.
    let directive = format!("warn,{}={level}", env!("CARGO_CRATE_NAME"));
    EnvFilter::new(directive)
}
