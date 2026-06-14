//! Best-effort git context for commit stamping and dirty detection.
//! Never blocks, never errors the caller. All ops are fast local reads.
//! Used for "per-commit picture" + pre-commit hook safety (dirty stage OK).

use anyhow::{Context, Result};
use std::process::Command;

/// Captured at command start (or per refresh). sha=None + dirty=true on any failure / no repo.
#[derive(Clone, Debug, Default)]
pub struct GitContext {
    /// Short SHA or full; None if not in a git repo or rev-parse failed.
    pub sha: Option<String>,
    /// True if worktree or index has uncommitted changes (or detection failed).
    pub dirty: bool,
}

/// Capture current HEAD + dirty state. Pure best-effort.
pub fn capture() -> GitContext {
    let sha = head_sha();
    let dirty = is_dirty();
    GitContext { sha, dirty }
}

fn head_sha() -> Option<String> {
    let out = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;
    if out.status.success() {
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if s.is_empty() { None } else { Some(s) }
    } else {
        None
    }
}

/// dirty = any change in index or worktree (or detection failed → assume dirty to be safe).
fn is_dirty() -> bool {
    // --quiet exits 0 only if no diff. We want "is there diff?" → invert.
    // Check both staged and unstaged. Any failure → dirty (conservative for hooks).
    let staged = Command::new("git")
        .args(["diff", "--quiet", "--cached"])
        .status()
        .map(|s| !s.success())
        .unwrap_or(true);
    if staged {
        return true;
    }
    Command::new("git")
        .args(["diff", "--quiet"])
        .status()
        .map(|s| !s.success())
        .unwrap_or(true)
}

/// Files changed according to git — shared by the CLI `sync`/`duplicates --since` commands
/// and the MCP `sai_sync` tool. A non-empty `explicit` list overrides git detection; otherwise
/// the working tree is diffed against `since` (or the staged set when `staged`; or plain
/// `git diff` when `since` is `None`). Returns repo-relative paths.
pub fn changed_files(since: Option<&str>, staged: bool, explicit: &[String]) -> Result<Vec<String>> {
    if !explicit.is_empty() {
        return Ok(explicit.to_vec());
    }
    let mut cmd = Command::new("git");
    cmd.args(["diff", "--name-only"]);
    if staged {
        cmd.arg("--cached");
    } else if let Some(since) = since {
        cmd.arg(since);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// An explicit path list bypasses git entirely and is returned verbatim (the path the
    /// CLI `--file` flag and the MCP `sai_sync { paths }` argument take).
    #[test]
    fn changed_files_returns_explicit_paths_verbatim() {
        let explicit = vec!["a.rs".to_string(), "b/c.ts".to_string()];
        let got = changed_files(Some("HEAD~1"), false, &explicit).unwrap();
        assert_eq!(got, explicit);
    }
}
