//! Best-effort git context for commit stamping and dirty detection.
//! Never blocks, never errors the caller. All ops are fast local reads.
//! Used for "per-commit picture" + pre-commit hook safety (dirty stage OK).

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
