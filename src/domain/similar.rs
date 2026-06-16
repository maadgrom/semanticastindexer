//! The [`SimilarTarget`] value object: what a `find_similar` request searches for.

/// What `find_similar` searches for: a code snippet (embedded as a PASSAGE) or an
/// existing indexed chunk located by `path` + 1-based `line` (its stored vector is
/// reused — no re-embed — and the chunk itself is excluded from its own results).
///
/// Owned (no borrows) so it can be sent across the backend worker-thread boundary as
/// part of a request.
pub enum SimilarTarget {
    /// A code snippet to embed as a passage (code-vs-code space).
    Code(String),
    /// An existing indexed chunk's location.
    Location { path: String, line: usize },
}
