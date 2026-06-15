//! End-to-end tests for `init` and the default-config resolution, driving the REAL
//! binary (via `CARGO_BIN_EXE_*`) in temp working directories — plus library-level
//! proof that a generated config actually drives `build_plan`.

use std::fs;
use std::io::Write;
use std::process::{Command, Output, Stdio};
use tempfile::TempDir;

use semanticastindexer::config::build_plan;
use semanticastindexer::init::{interview::Answers, template};

const BIN: &str = env!("CARGO_BIN_EXE_semanticastindexer");

/// Run the binary with `args` in `dir`, returning the completed output.
fn run_in(dir: &std::path::Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .output()
        .expect("binary must run")
}

/// `init --yes` writes the standard sai-cfg.yml; a second run refuses to clobber it
/// until --force is given.
#[test]
fn init_yes_creates_standard_config_and_guards_overwrite() {
    let dir = TempDir::new().unwrap();
    let cfg = dir.path().join("sai-cfg.yml");

    let out = run_in(dir.path(), &["init", "--yes"]);
    assert!(out.status.success(), "init --yes must succeed: {out:?}");
    let yaml = fs::read_to_string(&cfg).expect("sai-cfg.yml was written");
    assert!(yaml.contains("backend: duckdb"), "standard default backend");
    assert!(yaml.contains("model: jinaai/jina-embeddings-v2-base-code"));

    // Refuses to overwrite without --force…
    let out = run_in(dir.path(), &["init", "--yes"]);
    assert!(!out.status.success(), "must refuse to overwrite");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("already exists"), "got: {stderr}");

    // …and overwrites with it.
    fs::write(&cfg, "mangled").unwrap();
    let out = run_in(dir.path(), &["init", "--yes", "--force"]);
    assert!(out.status.success(), "init --force must succeed: {out:?}");
    assert!(
        fs::read_to_string(&cfg)
            .unwrap()
            .contains("backend: duckdb")
    );
}

/// The interactive interview over piped stdin: answers land in the generated file.
#[test]
fn init_interview_over_piped_stdin() {
    let dir = TempDir::new().unwrap();
    let mut child = Command::new(BIN)
        .arg("init")
        .current_dir(dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary must spawn");
    // backend=qdrant, embedder=qdrant (server-side), collection=my_code, model=default,
    // url=set, excludes=vendor.
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"qdrant\nqdrant\nmy_code\n\nhttps://c1.eu.cloud:6334\nvendor\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "interview must succeed: {out:?}");

    let yaml = fs::read_to_string(dir.path().join("sai-cfg.yml")).unwrap();
    assert!(yaml.contains("backend: qdrant"), "answer applied:\n{yaml}");
    assert!(yaml.contains("collection: my_code"));
    assert!(yaml.contains("model: intfloat/multilingual-e5-small"));
    assert!(
        yaml.contains("vector_dim: 384"),
        "dim auto-derived for e5-small"
    );
    assert!(yaml.contains("  url: https://c1.eu.cloud:6334"));
    assert!(yaml.contains("  - vendor"));
}

/// Closed stdin (`init < /dev/null`) accepts every default — non-interactive use in
/// scripts/CI works without --yes.
#[test]
fn init_with_closed_stdin_accepts_defaults() {
    let dir = TempDir::new().unwrap();
    let mut child = Command::new(BIN)
        .arg("init")
        .current_dir(dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("binary must spawn");
    drop(child.stdin.take()); // close immediately: EOF on the first question
    assert!(child.wait().unwrap().success());
    let yaml = fs::read_to_string(dir.path().join("sai-cfg.yml")).unwrap();
    assert!(yaml.contains("backend: duckdb"));
}

/// `--output` writes elsewhere, creating parent directories.
#[test]
fn init_honors_output_path() {
    let dir = TempDir::new().unwrap();
    let out = run_in(
        dir.path(),
        &["init", "--yes", "--output", "configs/deep/sai-cfg.yml"],
    );
    assert!(out.status.success(), "{out:?}");
    assert!(dir.path().join("configs/deep/sai-cfg.yml").is_file());
}

/// Default config resolution at the binary level: `sai-cfg.yml` is preferred over the
/// legacy `indexer.yaml`, which is still honored when it is the only config present.
/// A bad exclude glob makes the loaded file unambiguous: only the file that was
/// actually read can fail the run.
#[test]
fn default_seek_prefers_sai_cfg_and_still_reads_legacy() {
    let dir = TempDir::new().unwrap();
    fs::create_dir(dir.path().join("src")).unwrap();
    let bad = "exclude:\n  - \"[\"\n"; // unclosed glob class → build_plan error
    let good = "collection: fine\n";

    // Both present: sai-cfg.yml (good) wins over indexer.yaml (bad) → run succeeds.
    fs::write(dir.path().join("sai-cfg.yml"), good).unwrap();
    fs::write(dir.path().join("indexer.yaml"), bad).unwrap();
    let out = run_in(dir.path(), &["--dry-run", "--silent"]);
    assert!(
        out.status.success(),
        "sai-cfg.yml must win over legacy: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Only the legacy file: it is still read (and its bad glob fails the run).
    fs::remove_file(dir.path().join("sai-cfg.yml")).unwrap();
    let out = run_in(dir.path(), &["--dry-run", "--silent"]);
    assert!(!out.status.success(), "legacy config must still be loaded");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("bad exclude glob"), "got: {stderr}");
}

/// No config anywhere: the run proceeds on built-in defaults and says so.
#[test]
fn default_seek_falls_back_to_builtin_defaults() {
    let dir = TempDir::new().unwrap();
    fs::create_dir(dir.path().join("src")).unwrap();
    let out = run_in(dir.path(), &["--dry-run"]);
    assert!(out.status.success(), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no config at sai-cfg.yml"),
        "the note names the standard file: {stderr}"
    );
}

/// An explicit `--config` that does not exist stays a hard error (only the default
/// lookup may be absent).
#[test]
fn explicit_missing_config_is_still_an_error() {
    let dir = TempDir::new().unwrap();
    let out = run_in(
        dir.path(),
        &["--dry-run", "--config", "/nonexistent/sai-cfg.yml"],
    );
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("config file not found"), "got: {stderr}");
}

/// Library-level: a config generated from interview answers drives `build_plan` to
/// exactly those values — the generated file is a working config, not just valid YAML.
#[test]
fn generated_config_drives_build_plan() {
    use clap::Parser;
    use semanticastindexer::cli::Args;

    let dir = TempDir::new().unwrap();
    let answers = Answers {
        backend: "duckdb".to_string(),
        embedder: "ollama".to_string(),
        collection: "acme_code".to_string(),
        model: "nomic-embed-text".to_string(),
        vector_dim: 768,
        ollama_url: "http://gpu:11434".to_string(),
        extra_exclude_dirs: vec!["vendor".to_string()],
        ..Answers::default()
    };
    let cfg = dir.path().join("sai-cfg.yml");
    fs::write(&cfg, template::render(&answers)).unwrap();

    let args =
        Args::try_parse_from(["semanticastindexer", "--config", cfg.to_str().unwrap()]).unwrap();
    let plan = build_plan(&args).unwrap();
    assert_eq!(plan.backend, "duckdb");
    assert_eq!(plan.embedder, "ollama");
    assert_eq!(plan.collection, "acme_code");
    assert_eq!(plan.model, "nomic-embed-text");
    assert_eq!(plan.vector_dim, 768);
    assert_eq!(plan.ollama_url, "http://gpu:11434");
    assert_eq!(plan.ollama_model.as_deref(), Some("nomic-embed-text"));
    assert!(plan.exclude_dirs.contains("vendor"));
    assert!(plan.exclude_dirs.contains("node_modules"));
    assert!(
        plan.passes_globs("src/ok.ts") && !plan.passes_globs("src/ok.test.ts"),
        "standard exclude globs are active"
    );
}
