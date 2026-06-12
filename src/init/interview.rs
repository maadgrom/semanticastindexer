//! The `init` interview: a few required questions (backend, embedder, collection,
//! model) plus optional backend-specific ones, read from any [`BufRead`] and written
//! to any [`Write`] so the whole flow is testable with scripted input. Every question
//! has a default; an empty answer (or EOF, for piped/non-interactive stdin) accepts it.

use anyhow::{Context, Result};
use std::io::{BufRead, Write};

// Defaults shared with runtime resolution (`config::build_plan`), so the generator can
// never drift from what the tool would resolve anyway. Model per path: `ort` gets the
// code-trained Jina model, `ollama` a model users commonly have pulled, Qdrant Cloud
// the lightweight server-side e5.
use crate::config::{
    DEFAULT_COLLECTION, DEFAULT_DUCKDB_PATH, DEFAULT_MODEL as DEFAULT_QDRANT_MODEL,
    DEFAULT_OLLAMA_MODEL, DEFAULT_OLLAMA_URL, DEFAULT_ORT_MODEL, DEFAULT_ORT_VECTOR_DIM,
};

/// Answers collected by the `init` interview. `Default` is the standard dummy config:
/// fully offline DuckDB + local ONNX embedder with the code-trained Jina model.
#[derive(Debug, Clone, PartialEq)]
pub struct Answers {
    /// Vector backend: "duckdb" or "qdrant".
    pub backend: String,
    /// Embedder (duckdb backend only): "ort" or "ollama".
    pub embedder: String,
    /// Target collection (Qdrant) / table (DuckDB).
    pub collection: String,
    /// Embedding model label.
    pub model: String,
    /// Output dimensionality of `model` (validated against the index at runtime).
    pub vector_dim: u64,
    /// Qdrant cluster gRPC URL (qdrant backend; None = rely on the QDRANT_URL env var).
    pub qdrant_url: Option<String>,
    /// DuckDB index file path (duckdb backend).
    pub duckdb_path: String,
    /// Ollama server URL (ollama embedder).
    pub ollama_url: String,
    /// Extra directory names to prune, on top of the standard list.
    pub extra_exclude_dirs: Vec<String>,
}

impl Default for Answers {
    fn default() -> Self {
        Answers {
            backend: "duckdb".to_string(),
            embedder: "ort".to_string(),
            collection: DEFAULT_COLLECTION.to_string(),
            model: DEFAULT_ORT_MODEL.to_string(),
            vector_dim: DEFAULT_ORT_VECTOR_DIM,
            qdrant_url: None,
            duckdb_path: DEFAULT_DUCKDB_PATH.to_string(),
            ollama_url: DEFAULT_OLLAMA_URL.to_string(),
            extra_exclude_dirs: Vec::new(),
        }
    }
}

/// Output dimensionality for models we recognize (case-insensitive substring match),
/// so the interview only asks for `vector_dim` when the model is unknown.
pub fn known_model_dim(model: &str) -> Option<u64> {
    let m = model.to_ascii_lowercase();
    const DIMS: &[(&str, u64)] = &[
        ("jina-embeddings-v2-base-code", 768),
        ("e5-small", 384),
        ("e5-base", 768),
        ("e5-large", 1024),
        ("mxbai-embed-large", 1024),
        ("nomic-embed-text", 768),
    ];
    DIMS.iter()
        .find(|(name, _)| m.contains(name))
        .map(|&(_, dim)| dim)
}

/// Run the interview: required questions first (backend → embedder → collection →
/// model, with `vector_dim` asked only for unrecognized models), then the optional
/// backend-specific connection settings and extra exclude dirs.
pub fn interview<R: BufRead, W: Write>(input: &mut R, out: &mut W) -> Result<Answers> {
    writeln!(
        out,
        "Generating a starter config — press Enter to accept the [default] for each question."
    )?;

    let backend = ask_choice(
        input,
        out,
        "Vector backend — duckdb (local file, fully offline) or qdrant (Qdrant Cloud)",
        &["duckdb", "qdrant"],
        "duckdb",
    )?;
    let embedder = if backend == "duckdb" {
        ask_choice(
            input,
            out,
            "Embedder — ort (local ONNX, zero setup) or ollama (running Ollama server)",
            &["ort", "ollama"],
            "ort",
        )?
    } else {
        // Qdrant embeds server-side; the embedder field is ignored there.
        "ort".to_string()
    };

    let collection = ask_string(
        input,
        out,
        "Collection (Qdrant) / table (DuckDB) name",
        DEFAULT_COLLECTION,
    )?;

    let default_model = match (backend.as_str(), embedder.as_str()) {
        ("qdrant", _) => DEFAULT_QDRANT_MODEL,
        (_, "ollama") => DEFAULT_OLLAMA_MODEL,
        _ => DEFAULT_ORT_MODEL,
    };
    let model = ask_string(input, out, "Embedding model", default_model)?;
    let vector_dim = match known_model_dim(&model) {
        Some(dim) => {
            writeln!(out, "  using {dim} dimensions for {model}")?;
            dim
        }
        None => ask_dim(input, out, &model)?,
    };

    // Optional, backend-specific connection settings.
    let mut answers = Answers {
        backend,
        embedder,
        collection,
        model,
        vector_dim,
        ..Answers::default()
    };
    if answers.backend == "qdrant" {
        answers.qdrant_url = ask_optional(
            input,
            out,
            "Qdrant cluster gRPC URL (Enter = use the QDRANT_URL env var)",
        )?;
    } else {
        answers.duckdb_path =
            ask_string(input, out, "DuckDB index file path", DEFAULT_DUCKDB_PATH)?;
    }
    if answers.embedder == "ollama" {
        answers.ollama_url = ask_string(input, out, "Ollama server URL", DEFAULT_OLLAMA_URL)?;
    }

    answers.extra_exclude_dirs = ask_list(
        input,
        out,
        "Extra directories to exclude, comma-separated (e.g. vendor,examples; Enter = none)",
    )?;

    Ok(answers)
}

/// Read one trimmed answer line. `Ok(None)` on EOF, so piped/closed stdin gracefully
/// accepts the default of every remaining question instead of hanging or erroring.
fn read_answer<R: BufRead>(input: &mut R) -> Result<Option<String>> {
    let mut line = String::new();
    let n = input
        .read_line(&mut line)
        .context("failed to read interview answer")?;
    if n == 0 {
        return Ok(None);
    }
    Ok(Some(line.trim().to_string()))
}

/// Free-text question with a default; empty answer or EOF accepts the default.
fn ask_string<R: BufRead, W: Write>(
    input: &mut R,
    out: &mut W,
    prompt: &str,
    default: &str,
) -> Result<String> {
    write!(out, "{prompt} [{default}]: ")?;
    out.flush()?;
    match read_answer(input)? {
        None => {
            writeln!(out)?;
            Ok(default.to_string())
        }
        Some(s) if s.is_empty() => Ok(default.to_string()),
        Some(s) => Ok(s),
    }
}

/// Closed-choice question; re-prompts on anything outside `choices` (case-insensitive).
fn ask_choice<R: BufRead, W: Write>(
    input: &mut R,
    out: &mut W,
    prompt: &str,
    choices: &[&str],
    default: &str,
) -> Result<String> {
    loop {
        write!(out, "{prompt} ({}) [{default}]: ", choices.join("/"))?;
        out.flush()?;
        match read_answer(input)? {
            None => {
                writeln!(out)?;
                return Ok(default.to_string());
            }
            Some(s) if s.is_empty() => return Ok(default.to_string()),
            Some(s) => {
                let s = s.to_ascii_lowercase();
                if choices.contains(&s.as_str()) {
                    return Ok(s);
                }
                writeln!(out, "  please answer one of: {}", choices.join(", "))?;
            }
        }
    }
}

/// Optional free-text question; empty answer or EOF means "not set".
fn ask_optional<R: BufRead, W: Write>(
    input: &mut R,
    out: &mut W,
    prompt: &str,
) -> Result<Option<String>> {
    write!(out, "{prompt}: ")?;
    out.flush()?;
    match read_answer(input)? {
        None => {
            writeln!(out)?;
            Ok(None)
        }
        Some(s) if s.is_empty() => Ok(None),
        Some(s) => Ok(Some(s)),
    }
}

/// Comma-separated list question; empty answer or EOF means an empty list.
fn ask_list<R: BufRead, W: Write>(input: &mut R, out: &mut W, prompt: &str) -> Result<Vec<String>> {
    Ok(ask_optional(input, out, prompt)?
        .map(|s| {
            s.split(',')
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default())
}

/// `vector_dim` for an unrecognized model: required, must be a positive integer.
/// EOF here is a hard error — we cannot guess the dimensionality of an unknown model.
fn ask_dim<R: BufRead, W: Write>(input: &mut R, out: &mut W, model: &str) -> Result<u64> {
    loop {
        write!(
            out,
            "Vector dimensionality of '{model}' (its embedding output size, e.g. 768): "
        )?;
        out.flush()?;
        match read_answer(input)? {
            None => anyhow::bail!(
                "vector_dim is required for unrecognized model '{model}' (input closed before an answer)"
            ),
            Some(s) => match s.parse::<u64>() {
                Ok(n) if n > 0 => return Ok(n),
                _ => writeln!(out, "  please enter a positive integer")?,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Run the interview over scripted input, returning the answers and the prompts.
    fn run(script: &str) -> (Result<Answers>, String) {
        let mut input = Cursor::new(script.as_bytes().to_vec());
        let mut out: Vec<u8> = Vec::new();
        let res = interview(&mut input, &mut out);
        (res, String::from_utf8(out).expect("prompts are utf-8"))
    }

    /// Immediate EOF (e.g. `init < /dev/null`) accepts every default: the result is
    /// exactly `Answers::default()` — the standard dummy config.
    #[test]
    fn eof_everywhere_yields_pure_defaults() {
        let (res, _) = run("");
        assert_eq!(res.unwrap(), Answers::default());
    }

    /// Blank lines (Enter on every question) also accept every default.
    #[test]
    fn blank_answers_yield_pure_defaults() {
        let (res, _) = run("\n\n\n\n\n\n");
        assert_eq!(res.unwrap(), Answers::default());
    }

    /// Qdrant path: no embedder question; the model default switches to e5-small
    /// (auto 384 dims) and the optional URL question is asked.
    #[test]
    fn qdrant_flow_asks_url_and_skips_embedder() {
        let (res, prompts) = run("qdrant\nmy_code\n\nhttps://c1.eu.cloud:6334\nvendor, examples\n");
        let a = res.unwrap();
        assert_eq!(a.backend, "qdrant");
        assert_eq!(a.collection, "my_code");
        assert_eq!(a.model, DEFAULT_QDRANT_MODEL);
        assert_eq!(a.vector_dim, 384, "e5-small dim auto-detected");
        assert_eq!(a.qdrant_url.as_deref(), Some("https://c1.eu.cloud:6334"));
        assert_eq!(a.extra_exclude_dirs, vec!["vendor", "examples"]);
        assert!(
            !prompts.contains("Embedder —"),
            "qdrant embeds server-side: no embedder question"
        );
        assert!(prompts.contains("Qdrant cluster gRPC URL"));
    }

    /// Ollama path: model default switches to mxbai-embed-large; a known custom model
    /// (nomic-embed-text) resolves its dim automatically; the server URL is asked.
    #[test]
    fn ollama_flow_with_known_model_auto_dim() {
        let (res, prompts) = run("duckdb\nollama\n\nnomic-embed-text\n\nhttp://gpu:11434\n\n");
        let a = res.unwrap();
        assert_eq!(a.embedder, "ollama");
        assert_eq!(a.model, "nomic-embed-text");
        assert_eq!(a.vector_dim, 768, "nomic-embed-text dim auto-detected");
        assert_eq!(a.ollama_url, "http://gpu:11434");
        assert!(prompts.contains(&format!("[{DEFAULT_OLLAMA_MODEL}]")));
    }

    /// An invalid backend answer re-prompts instead of failing or being accepted.
    #[test]
    fn invalid_choice_reprompts() {
        let (res, prompts) = run("postgres\nqdrant\n\n\n\n\n");
        assert_eq!(res.unwrap().backend, "qdrant");
        assert!(prompts.contains("please answer one of: duckdb, qdrant"));
    }

    /// Choice answers are case-insensitive.
    #[test]
    fn choice_is_case_insensitive() {
        let (res, _) = run("QDRANT\n\n\n\n\n");
        assert_eq!(res.unwrap().backend, "qdrant");
    }

    /// An unknown model asks for vector_dim, rejecting junk until a positive integer.
    #[test]
    fn unknown_model_asks_dim_and_validates() {
        let (res, prompts) = run("duckdb\nort\n\nacme/strange-model\nabc\n0\n1024\n\n\n");
        let a = res.unwrap();
        assert_eq!(a.model, "acme/strange-model");
        assert_eq!(a.vector_dim, 1024);
        assert!(prompts.contains("Vector dimensionality of 'acme/strange-model'"));
        assert!(prompts.contains("please enter a positive integer"));
    }

    /// EOF while the dim of an unknown model is pending is a hard, explained error.
    #[test]
    fn unknown_model_eof_on_dim_errors() {
        let (res, _) = run("duckdb\nort\n\nacme/strange-model\n");
        let err = res.unwrap_err().to_string();
        assert!(err.contains("vector_dim is required"), "got: {err}");
        assert!(err.contains("acme/strange-model"), "got: {err}");
    }

    /// List parsing trims entries and drops empties.
    #[test]
    fn exclude_dirs_list_is_trimmed_and_filtered() {
        let (res, _) = run("\n\n\n\n\n vendor ,, examples ,\n");
        assert_eq!(res.unwrap().extra_exclude_dirs, vec!["vendor", "examples"]);
    }

    /// The known-model dim table covers the models the templates recommend.
    #[test]
    fn known_model_dims() {
        assert_eq!(
            known_model_dim("jinaai/jina-embeddings-v2-base-code"),
            Some(768)
        );
        assert_eq!(known_model_dim("intfloat/multilingual-e5-small"), Some(384));
        assert_eq!(known_model_dim("Xenova/multilingual-e5-small"), Some(384));
        assert_eq!(known_model_dim("intfloat/multilingual-e5-base"), Some(768));
        assert_eq!(
            known_model_dim("intfloat/multilingual-e5-large"),
            Some(1024)
        );
        assert_eq!(known_model_dim("mxbai-embed-large"), Some(1024));
        assert_eq!(known_model_dim("nomic-embed-text"), Some(768));
        assert_eq!(known_model_dim("acme/strange-model"), None);
    }
}
