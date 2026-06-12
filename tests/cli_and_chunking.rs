//! Integration tests against the PUBLIC library API (no network, no real backend):
//! CLI parsing → config/flag precedence in `build_plan`, and the walk/filter/chunk
//! pipeline (`collect_chunks`) over a temp source tree. These run as a separate crate,
//! so they prove the lib surface is actually usable by an external consumer.

use clap::Parser;
use std::fs;
use tempfile::TempDir;

use semanticastindexer::cli::Args;
use semanticastindexer::config::build_plan;
use semanticastindexer::indexer;

/// Parse CLI args from a vector (no process exit on error).
fn parse(argv: &[&str]) -> Args {
    Args::try_parse_from(argv).expect("argv must parse")
}

/// Unknown flags are rejected — guards against scripts passing phantom flags
/// (a `--language` flag was once passed by the Makefile and silently assumed to work).
#[test]
fn unknown_flags_are_rejected() {
    let res = Args::try_parse_from(["semanticastindexer", "--language", "ts"]);
    assert!(res.is_err(), "--language is not a real flag");
}

/// Precedence: CLI flag > YAML config > built-in default.
#[test]
fn build_plan_flag_overrides_config() {
    let dir = TempDir::new().unwrap();
    let cfg = dir.path().join("indexer.yaml");
    fs::write(&cfg, "collection: from_config\nbackend: duckdb\n").unwrap();
    let cfg_path = cfg.to_str().unwrap();

    // Config value applies when no flag is given.
    let plan = build_plan(&parse(&["semanticastindexer", "--config", cfg_path])).unwrap();
    assert_eq!(plan.collection, "from_config");
    assert_eq!(plan.backend, "duckdb");

    // Explicit flag wins over the config value.
    let plan = build_plan(&parse(&[
        "semanticastindexer",
        "--config",
        cfg_path,
        "--collection",
        "from_flag",
        "--backend",
        "qdrant",
    ]))
    .unwrap();
    assert_eq!(plan.collection, "from_flag");
    assert_eq!(plan.backend, "qdrant");
}

/// A missing explicit --config path is a hard error (only the default name may be absent).
#[test]
fn build_plan_errors_on_missing_explicit_config() {
    let res = build_plan(&parse(&[
        "semanticastindexer",
        "--config",
        "/nonexistent/indexer.yaml",
    ]));
    assert!(res.is_err(), "explicit missing config must error");
}

/// Build a plan rooted at `root` with config `yaml`, ext ts.
fn plan_for(root: &std::path::Path, yaml: &str) -> semanticastindexer::config::Plan {
    let cfg = root.join("indexer.yaml");
    fs::write(&cfg, yaml).unwrap();
    build_plan(&parse(&[
        "semanticastindexer",
        "--root",
        root.to_str().unwrap(),
        "--ext",
        "ts",
        "--config",
        cfg.to_str().unwrap(),
    ]))
    .unwrap()
}

/// collect_chunks walks the tree, honors exclude globs and the generated-marker skip,
/// and produces deterministic chunk ids across runs (the stable point-id contract).
#[test]
fn collect_chunks_filters_and_is_deterministic() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    fs::create_dir(&src).unwrap();
    fs::write(
        src.join("keep.ts"),
        "export function keep() { return 42 }\n",
    )
    .unwrap();
    fs::write(src.join("skip.test.ts"), "export const t = 1\n").unwrap();
    fs::write(
        src.join("gen.ts"),
        "// @generated\nexport function gen() { return 0 }\n",
    )
    .unwrap();

    let plan = plan_for(
        dir.path(),
        "exclude:\n  - \"**/*.test.ts\"\nskip_generated_marker: true\n",
    );
    // Point the plan's root at the src subdir (the walk root).
    let plan = semanticastindexer::config::Plan {
        root: src.to_str().unwrap().to_string(),
        ..plan
    };

    let (chunks, files, skipped) = indexer::collect_chunks(&plan);
    assert_eq!(files, 1, "only keep.ts is indexable");
    assert_eq!(skipped, 2, "excluded glob + generated marker are skipped");
    assert!(!chunks.is_empty(), "keep.ts produces chunks");
    assert!(
        chunks.iter().all(|c| c.path.ends_with("keep.ts")),
        "all chunks come from keep.ts"
    );

    // Determinism: a second walk yields identical ids (stable XxHash64 point ids).
    let (again, _, _) = indexer::collect_chunks(&plan);
    let ids: Vec<u64> = chunks.iter().map(|c| c.id).collect();
    let ids_again: Vec<u64> = again.iter().map(|c| c.id).collect();
    assert_eq!(ids, ids_again, "chunk ids are deterministic across runs");
}

/// The `sai-noindexing` marker drops a chunk; `sai-noduplicate` keeps it but flags it.
#[test]
fn collect_chunks_honors_opt_out_markers() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    fs::create_dir(&src).unwrap();
    fs::write(
        src.join("a.ts"),
        "export function a() {\n  // sai-noindexing\n  return 1\n}\n",
    )
    .unwrap();
    fs::write(
        src.join("b.ts"),
        "export function b() {\n  // sai-noduplicate\n  return 2\n}\n",
    )
    .unwrap();

    let plan = plan_for(dir.path(), "{}\n");
    let plan = semanticastindexer::config::Plan {
        root: src.to_str().unwrap().to_string(),
        ..plan
    };

    let (chunks, _, _) = indexer::collect_chunks(&plan);
    assert!(
        !chunks.iter().any(|c| c.path.ends_with("a.ts")),
        "sai-noindexing chunks are never stored"
    );
    let b: Vec<_> = chunks.iter().filter(|c| c.path.ends_with("b.ts")).collect();
    assert!(!b.is_empty(), "sai-noduplicate chunks are still indexed");
    assert!(
        b.iter().all(|c| c.no_duplicate),
        "sai-noduplicate chunks carry the flag"
    );
}
