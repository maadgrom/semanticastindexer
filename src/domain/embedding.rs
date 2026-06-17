//! Embedding prefix policy ([`PrefixStyle`]) and the shared passage/query formatting
//! helpers. Owned by the domain because [`crate::domain::Plan`] carries a
//! `prefix_style: PrefixStyle` field.

use anyhow::Result;

/// E5 asymmetric prefix for stored passages.
pub const PASSAGE_PREFIX: &str = "passage: ";
/// E5 asymmetric prefix for queries.
pub const QUERY_PREFIX: &str = "query: ";
/// QwenInstruct query instruction. Qwen embedding models are instruction-tuned: the
/// query (not the passage) is wrapped with a task description. The stored passage is bare.
pub const QWEN_QUERY_INSTRUCT: &str =
    "Instruct: Given a code search query, retrieve relevant code\nQuery: ";

/// Model-aware embedding prefix policy. Resolved once in `build_plan` (explicit config
/// wins; else auto-detected from the model name) and applied by BOTH embedders and the
/// Qdrant `Document` path through the shared [`format_passage`]/[`format_query`] helpers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PrefixStyle {
    /// E5 asymmetric prefixes: `passage: <t>` / `query: <t>`.
    E5,
    /// Qwen instruct: bare passage; query wrapped with a task instruction.
    Qwen,
    /// No prefix on either side.
    None,
}

impl PrefixStyle {
    /// Parse an explicit `prefix_style` config value ("e5" | "qwen" | "none").
    pub fn parse(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "e5" => Ok(PrefixStyle::E5),
            "qwen" => Ok(PrefixStyle::Qwen),
            "none" => Ok(PrefixStyle::None),
            other => {
                anyhow::bail!("unknown prefix_style '{other}' (expected 'e5', 'qwen', or 'none')")
            }
        }
    }

    /// Auto-detect the prefix style from a model name: contains "e5" → E5,
    /// contains "qwen" → Qwen, otherwise None.
    pub fn detect(model: &str) -> Self {
        let m = model.to_ascii_lowercase();
        if m.contains("e5") {
            PrefixStyle::E5
        } else if m.contains("qwen") {
            PrefixStyle::Qwen
        } else {
            PrefixStyle::None
        }
    }
}

/// Format a stored passage under the resolved prefix policy. Single source of truth
/// shared by the Qdrant `Document` path and both DuckDB embedders.
#[cfg_attr(
    not(any(feature = "qdrant", feature = "ort", feature = "ollama")),
    allow(dead_code)
)]
pub fn format_passage(style: PrefixStyle, text: &str) -> String {
    match style {
        PrefixStyle::E5 => format!("{PASSAGE_PREFIX}{text}"),
        // Qwen: passages are bare (the instruction goes on the query side only).
        PrefixStyle::Qwen | PrefixStyle::None => text.to_string(),
    }
}

/// Format a query under the resolved prefix policy (shared, see [`format_passage`]).
#[cfg_attr(
    not(any(feature = "qdrant", feature = "ort", feature = "ollama")),
    allow(dead_code)
)]
pub fn format_query(style: PrefixStyle, text: &str) -> String {
    match style {
        PrefixStyle::E5 => format!("{QUERY_PREFIX}{text}"),
        PrefixStyle::Qwen => format!("{QWEN_QUERY_INSTRUCT}{text}"),
        PrefixStyle::None => text.to_string(),
    }
}
