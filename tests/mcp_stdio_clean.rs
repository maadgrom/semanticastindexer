//! Regression guard: `semanticastindexer mcp --allow-write` must produce a CLEAN stdout stream.
//!
//! The bug being guarded: before the tracing migration (Step 5 of `fix-mcp-stdout-via-tracing`),
//! several backend diagnostics — notably "using existing collection …" in `duckdb.rs` — were
//! `println!` calls that landed on stdout. Because MCP over stdio is a newline-delimited
//! JSON-RPC stream, any human-readable line on stdout corrupts the protocol and causes strict
//! clients (e.g. Grok) to reject the handshake entirely.
//!
//! This test spawns the real binary, sends a JSON-RPC `initialize` request, and asserts that
//! the FIRST line on stdout is a valid JSON-RPC 2.0 frame — not a diagnostic string.
//!
//! Framing assumption: rmcp's stdio transport uses newline-delimited JSON (one JSON object per
//! line, `\n`-terminated). If rmcp ever changes its framing this test will need updating.
//!
//! This test is gated on `feature = "mcp"` because the `mcp` subcommand only exists in that
//! build. CI runs `--features all` which includes `mcp`.

#[cfg(feature = "mcp")]
mod mcp_stdio_tests {
    use std::io::Write;
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::time::Duration;

    use tempfile::TempDir;

    /// Path to the compiled binary under test, injected by cargo's test harness.
    const BIN: &str = env!("CARGO_BIN_EXE_semanticastindexer");

    /// JSON-RPC 2.0 `initialize` request (MCP spec §2.1).
    /// This is the very first message a client must send; the server must echo back
    /// an `initialize` response — the first stdout line — before any tool calls.
    const INITIALIZE_REQUEST: &[u8] =
        b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{},\"clientInfo\":{\"name\":\"test\",\"version\":\"0.1\"}}}\n";

    /// Probe whether a stderr snippet looks like a DuckDB VSS / extension network error.
    ///
    /// In sandboxed or offline CI environments the DuckDB VSS extension cannot be downloaded
    /// from the internet.  When that happens the server exits before producing any stdout and
    /// we must SKIP rather than hard-fail — the intent of the test is to catch diagnostic
    /// pollution, not to gatekeep environment setup.
    fn looks_like_vss_load_failure(stderr: &str) -> bool {
        let lower = stderr.to_lowercase();
        lower.contains("vss")
            || lower.contains("extension")
            || lower.contains("http")
            || lower.contains("download")
            || lower.contains("network")
            || lower.contains("failed to load")
            || lower.contains("could not load")
    }

    /// Proves that the first byte emitted on stdout by `mcp --allow-write` is the opening
    /// brace of a JSON-RPC 2.0 `initialize` response — not a human-readable diagnostic.
    ///
    /// Setup:
    ///   - Temp dir with `src/hello.ts` so the root is non-empty.
    ///   - `sai-cfg.yml` pointing at `backend: duckdb` and `embedder: ollama`.
    ///     * DuckDB `ensure_ready` only creates tables — no embedding — so it works without
    ///       any running service.
    ///     * `OllamaEmbedder::new` only builds a `reqwest::Client` — also network-free.
    ///     * Together, the MCP handshake completes without a live ollama daemon or Qdrant.
    ///
    /// Failure modes and their treatment:
    ///   SKIP  — server exits before producing stdout with a VSS/extension error on stderr
    ///            (DuckDB can't download the extension in sandboxed/offline CI).
    ///   FAIL  — server started, produced stdout, but the first line is NOT valid JSON-RPC
    ///            (a diagnostic leaked onto stdout — the very bug this test guards against).
    #[test]
    fn mcp_allow_write_stdout_is_clean_jsonrpc() {
        // ── temp workspace ────────────────────────────────────────────────────
        let dir = TempDir::new().expect("tempdir must be created");
        let src_dir = dir.path().join("src");
        std::fs::create_dir_all(&src_dir).expect("src/ must be created");
        std::fs::write(src_dir.join("hello.ts"), "export const hello = () => 42;\n")
            .expect("dummy source file must be written");

        // Collection name uses a short unique suffix so parallel test runs don't share state.
        let collection = format!("test_mcp_stdio_{}", std::process::id());
        let cfg_content = format!("backend: duckdb\nembedder: ollama\ncollection: {collection}\n");
        let cfg_path = dir.path().join("sai-cfg.yml");
        std::fs::write(&cfg_path, &cfg_content).expect("sai-cfg.yml must be written");

        // ── spawn ─────────────────────────────────────────────────────────────
        let mut child = Command::new(BIN)
            .args([
                "mcp",
                "--allow-write",
                "--config",
                cfg_path.to_str().unwrap(),
            ])
            .current_dir(dir.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("binary must spawn");

        // ── send initialize ───────────────────────────────────────────────────
        {
            let stdin = child.stdin.as_mut().expect("stdin must be piped");
            stdin
                .write_all(INITIALIZE_REQUEST)
                .expect("initialize request must be written to stdin");
            // Flush is implicit when the MutexGuard is dropped; the write above is
            // complete.  We intentionally do NOT close stdin here — closing it signals
            // EOF and may cause the server to exit before we read the response.
        }

        // ── read first stdout line (with timeout) ─────────────────────────────
        // We use a reader thread + mpsc so the main thread can enforce the deadline
        // without blocking forever on a server that never writes.
        let mut stdout = child.stdout.take().expect("stdout must be piped");
        let (tx, rx) = mpsc::channel::<std::io::Result<String>>();

        std::thread::spawn(move || {
            use std::io::BufRead;
            let mut reader = std::io::BufReader::new(&mut stdout);
            let mut line = String::new();
            let result = reader.read_line(&mut line).map(|_| line);
            // Send even if the channel is gone (test timed out); we don't care about the error.
            let _ = tx.send(result);
        });

        let first_line = match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(line)) => line,
            Ok(Err(io_err)) => {
                // IO error reading stdout: check if the server exited early (env issue).
                let stderr = collect_stderr(&mut child);
                if looks_like_vss_load_failure(&stderr) {
                    eprintln!(
                        "[SKIP] mcp_allow_write_stdout_is_clean_jsonrpc: DuckDB VSS extension \
                         could not load (likely offline/sandboxed CI). \
                         stdout IO error: {io_err}. stderr:\n{stderr}"
                    );
                    let _ = child.kill();
                    return;
                }
                panic!("IO error reading child stdout: {io_err}\nstderr:\n{stderr}");
            }
            Err(_timeout) => {
                // The server didn't produce any stdout within the deadline.
                // Collect stderr for diagnostics, then decide skip vs. fail.
                let stderr = collect_stderr(&mut child);
                let _ = child.kill();

                if looks_like_vss_load_failure(&stderr) {
                    eprintln!(
                        "[SKIP] mcp_allow_write_stdout_is_clean_jsonrpc: timed out waiting for \
                         first stdout line and stderr suggests a VSS/extension load failure \
                         (offline/sandboxed CI). stderr:\n{stderr}"
                    );
                    return;
                }

                panic!(
                    "timed out (10s) waiting for first stdout line from `mcp --allow-write`.\n\
                     This likely means the server hung or exited before replying.\n\
                     stderr:\n{stderr}"
                );
            }
        };

        // ── clean up ──────────────────────────────────────────────────────────
        // Drop stdin to signal EOF so the server can exit cleanly, then kill best-effort.
        drop(child.stdin.take());
        let _ = child.kill();

        // ── assert: first line is valid JSON-RPC 2.0 ─────────────────────────
        let trimmed = first_line.trim();

        // If the line is empty the server wrote nothing useful before EOF; still a failure.
        assert!(
            !trimmed.is_empty(),
            "first stdout line must not be empty — server may have exited without responding"
        );

        // Parse as JSON.  A human-readable diagnostic like "using existing collection …"
        // will fail here, which is the primary regression this test guards against.
        let value: serde_json::Value = serde_json::from_str(trimmed).unwrap_or_else(|parse_err| {
            panic!(
                "first stdout line is NOT valid JSON — a diagnostic was emitted on stdout!\n\
                 Line: {trimmed:?}\n\
                 Parse error: {parse_err}\n\
                 This means a `println!` (or equivalent) in the startup path was not yet migrated \
                 to `tracing`. See Step 5 of `.omc/plans/fix-mcp-stdout-via-tracing.md`."
            )
        });

        // Verify the JSON-RPC version field.  A real response always has `"jsonrpc": "2.0"`.
        assert_eq!(
            value.get("jsonrpc").and_then(|v| v.as_str()),
            Some("2.0"),
            "first stdout line must be a JSON-RPC 2.0 frame (has \"jsonrpc\": \"2.0\"), got: {trimmed}"
        );
    }

    /// Drain `child.stderr` into a `String`.  Called after the reader thread returns or after
    /// a timeout — at that point the child has either exited or been killed, so reading
    /// stderr will complete quickly.
    fn collect_stderr(child: &mut std::process::Child) -> String {
        use std::io::Read;
        let mut buf = String::new();
        if let Some(mut stderr) = child.stderr.take() {
            let _ = stderr.read_to_string(&mut buf);
        }
        buf
    }
}
