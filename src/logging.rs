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
use crate::config::LoggingConfig;

/// Install the global `tracing` subscriber. Idempotent: calling this more than once
/// (e.g. across test cases) is treated as success rather than a panic, because we
/// install with `try_init()`.
///
/// Every knob resolves in the same precedence order — `RUST_LOG`/CLI flags first, then
/// the project config's `logging:` block, then a built-in default:
///   * level  — `RUST_LOG` > `-v`/`--silent` > `logging.level`  > `info`
///   * format — `--log-format`              > `logging.format` > `pretty`
///   * timing — `--timing` (and `--silent` forces off) > `logging.timing` > off
///
/// The config tier is read silently here via [`crate::config::logging_config`] — see
/// that fn for why it must not log or touch stdout at this point in startup.
pub fn init(args: &Args) -> Result<()> {
    // Lowest-precedence tier: the project config's `logging:` block (silent, fault-tolerant).
    let file = crate::config::logging_config(args);

    let filter = build_filter(args, &file);
    let format = resolve_format(args, &file);
    let timing = resolve_timing(args, &file);

    // ANSI auto-detect: MCP clients capture stderr to a *file* (not a tty); emitting
    // escape codes there would corrupt the captured logs. Only colorize a real tty.
    let ansi = std::io::stderr().is_terminal();

    // Per-op timing is opt-in: when enabled, emit a span-close event per instrumented
    // operation (`close time.busy=… time.idle=…`) to profile sync/embed performance.
    let span_events = if timing {
        FmtSpan::CLOSE
    } else {
        FmtSpan::NONE
    };

    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(ansi)
        .with_span_events(span_events);

    // `try_init()` (not `init()`) so a second install — e.g. when tests call this
    // repeatedly — returns Err instead of panicking; we treat that as success.
    let result = match format {
        LogFormat::Json => builder.json().try_init(),
        LogFormat::Pretty => builder.try_init(),
    };

    // An already-installed subscriber is not an error for our purposes.
    let _ = result;
    Ok(())
}

/// Resolve the active [`EnvFilter`]: honor `RUST_LOG` if present and valid; otherwise
/// derive a crate-scoped directive from the flags, falling back to the config level and
/// finally `info`.
fn build_filter(args: &Args, file: &LoggingConfig) -> EnvFilter {
    // Power-user knob first: `RUST_LOG` (parsed by `try_from_default_env`) wins.
    if let Ok(filter) = EnvFilter::try_from_default_env() {
        return filter;
    }

    // Flags next: `--silent` is the quietest; otherwise an explicit `-v`/`-vv` count. When
    // no flag was passed (silent=false, verbose=0) the config `logging.level` applies, then
    // `info`. Unknown config levels fall back to `info` (we can't log the typo — no
    // subscriber yet — so we fail soft rather than bail).
    let level = if args.silent {
        "error"
    } else {
        match args.verbose {
            0 => normalize_level(file.level.as_deref()),
            1 => "debug",
            _ => "trace",
        }
    };

    // Scope the chosen level to this crate; keep a global `warn` floor so dependency
    // noise stays down while upstream warnings still surface.
    let directive = format!("warn,{}={level}", env!("CARGO_CRATE_NAME"));
    EnvFilter::new(directive)
}

/// Validate a config-supplied level string against the known set, defaulting to `info`
/// for an absent or unrecognized value.
fn normalize_level(level: Option<&str>) -> &'static str {
    let s = match level {
        Some(s) => s.trim(),
        None => return "info",
    };
    if s.eq_ignore_ascii_case("error") {
        "error"
    } else if s.eq_ignore_ascii_case("warn") {
        "warn"
    } else if s.eq_ignore_ascii_case("debug") {
        "debug"
    } else if s.eq_ignore_ascii_case("trace") {
        "trace"
    } else {
        // "info" or anything unrecognized → the safe default.
        "info"
    }
}

/// Resolve the log format: `--log-format` flag > config `logging.format` > `pretty`.
fn resolve_format(args: &Args, file: &LoggingConfig) -> LogFormat {
    if let Some(fmt) = args.log_format {
        return fmt;
    }
    match file.format.as_deref().map(str::trim) {
        Some(s) if s.eq_ignore_ascii_case("json") => LogFormat::Json,
        // "pretty", absent, or unrecognized → the human-readable default.
        _ => LogFormat::Pretty,
    }
}

/// Resolve whether per-operation timing spans are emitted: `--timing` > config
/// `logging.timing` > off. `--silent` forces it off — the quietest mode wins.
fn resolve_timing(args: &Args, file: &LoggingConfig) -> bool {
    !args.silent && (args.timing || file.timing.unwrap_or(false))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn args(argv: &[&str]) -> Args {
        Args::try_parse_from(argv).expect("args parse")
    }

    /// An empty config maps to the safe `info` default; recognized values pass through
    /// (case/whitespace-insensitive); anything else also falls back to `info`.
    #[test]
    fn normalize_level_validates_and_defaults() {
        assert_eq!(normalize_level(None), "info");
        assert_eq!(normalize_level(Some("info")), "info");
        assert_eq!(normalize_level(Some("  DEBUG ")), "debug");
        assert_eq!(normalize_level(Some("warn")), "warn");
        assert_eq!(normalize_level(Some("nonsense")), "info");
    }

    /// Format precedence: the `--log-format` flag wins; otherwise the config value (parsed
    /// case-insensitively); otherwise `pretty`.
    #[test]
    fn resolve_format_flag_beats_config_beats_default() {
        let json_cfg = LoggingConfig {
            format: Some("json".to_string()),
            ..LoggingConfig::default()
        };
        // No flag → config applies.
        assert_eq!(resolve_format(&args(&["sai"]), &json_cfg), LogFormat::Json);
        // Flag overrides config.
        assert_eq!(
            resolve_format(&args(&["sai", "--log-format", "pretty"]), &json_cfg),
            LogFormat::Pretty
        );
        // No flag, no/empty config → pretty.
        assert_eq!(
            resolve_format(&args(&["sai"]), &LoggingConfig::default()),
            LogFormat::Pretty
        );
    }

    /// Timing precedence: `--timing` or config enables it; `--silent` forces it off.
    #[test]
    fn resolve_timing_flag_config_and_silent() {
        let on = LoggingConfig {
            timing: Some(true),
            ..LoggingConfig::default()
        };
        assert!(!resolve_timing(&args(&["sai"]), &LoggingConfig::default()));
        assert!(resolve_timing(&args(&["sai"]), &on));
        assert!(resolve_timing(
            &args(&["sai", "--timing"]),
            &LoggingConfig::default()
        ));
        // --silent wins over both the flag and config.
        assert!(!resolve_timing(
            &args(&["sai", "--timing", "--silent"]),
            &on
        ));
    }
}
