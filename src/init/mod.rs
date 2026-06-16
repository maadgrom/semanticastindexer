//! `init` subcommand: generate a starter `sai-cfg.yml`.
//!
//! The interview ([`interview`]) asks a few required questions (backend, embedder,
//! collection, model) and some optional ones (connection settings, extra excludes);
//! the template ([`template`]) renders the answers into the standard fully-commented
//! config. `--yes` (or closed stdin) accepts every default, producing the canonical
//! dummy config for the offline DuckDB + ONNX path.

pub mod interview;
pub mod template;

use anyhow::{Context, Result};
use std::path::Path;

use crate::cli::InitArgs;
use interview::Answers;

/// Run `init`: collect answers (pure defaults with `--yes`, interview otherwise),
/// render the config, sanity-check it parses, and write it to `args.output`.
// CLI-only command (dispatched before any backend/MCP path): the result lines are
// intentional data output, so the whole function opts out of the stdout lint.
#[allow(clippy::print_stdout)]
pub fn run(args: &InitArgs) -> Result<()> {
    let answers = if args.yes {
        Answers::default()
    } else {
        let stdin = std::io::stdin();
        let mut input = stdin.lock();
        let mut out = std::io::stdout().lock();
        interview::interview(&mut input, &mut out)?
    };

    let contents = template::render(&answers);
    // Guard the generator itself: what we write must be a loadable config.
    let _: crate::config::Config = serde_yaml_ng::from_str(&contents)
        .context("internal error: the generated config does not parse — please report this")?;

    write_config(Path::new(&args.output), &contents, args.force)?;
    println!("wrote {}", args.output);
    println!("next: semanticastindexer --dry-run   # preview what would be indexed");
    Ok(())
}

/// Write `contents` to `path`, creating parent directories as needed and refusing to
/// overwrite an existing file unless `force` is set.
pub fn write_config(path: &Path, contents: &str, force: bool) -> Result<()> {
    if path.exists() && !force {
        anyhow::bail!(
            "{} already exists — re-run with --force to overwrite",
            path.display()
        );
    }
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory: {}", parent.display()))?;
    }
    std::fs::write(path, contents)
        .with_context(|| format!("failed to write config: {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Overwrite guard: an existing file is refused without --force and replaced with it.
    #[test]
    fn write_config_refuses_then_forces_overwrite() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("sai-cfg.yml");
        write_config(&path, "first\n", false).unwrap();

        let err = write_config(&path, "second\n", false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("already exists"), "got: {err}");
        assert!(err.contains("--force"), "the fix is named: {err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first\n");

        write_config(&path, "second\n", true).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second\n");
    }

    /// Parent directories of a nested --output path are created.
    #[test]
    fn write_config_creates_parent_dirs() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nested/deeper/sai-cfg.yml");
        write_config(&path, "x: 1\n", false).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "x: 1\n");
    }
}
