//! Local embedding for the DuckDB backend.
//!
//! The ort embedder now defaults to the code-trained `jinaai/jina-embeddings-v2-base-code`
//! (symmetric, prefix_style: none, 768d). The classic E5 model (`intfloat/multilingual-e5-small`
//! / Xenova variant) remains available and is still the default for Qdrant server inference.
//! E5 uses asymmetric prefixes (`passage:` / `query:`) which are applied inside the embedder.
//!
//! The embedder is an ENUM (mirrors the `Backend` enum): `Ort` (raw ONNX Runtime) or
//! `Ollama` (HTTP). Methods are async: Ollama is HTTP; Ort runs sync CPU inference
//! inside the async fn — acceptable for a CLI.

#[cfg(any(feature = "ort", feature = "ollama"))]
use crate::config::Plan;
#[cfg(any(feature = "ort", feature = "ollama"))]
use crate::vectordbs::{PrefixStyle, format_passage, format_query};

/// Build the ort embedder from a resolved plan (model repo + cache dir + prefix policy).
#[cfg(feature = "ort")]
pub fn ort_embedder(plan: &Plan) -> anyhow::Result<Box<ort_impl::OrtEmbedder>> {
    Ok(Box::new(ort_impl::OrtEmbedder::new(
        &plan.model_repo,
        plan.duckdb_model_cache.as_deref(),
        plan.prefix_style,
    )?))
}

/// Build the Ollama embedder from a resolved plan (url + required model + prefix policy).
#[cfg(feature = "ollama")]
pub fn ollama_embedder(plan: &Plan) -> anyhow::Result<ollama_impl::OllamaEmbedder> {
    ollama_impl::OllamaEmbedder::new(
        &plan.ollama_url,
        plan.ollama_model.as_deref(),
        plan.prefix_style,
    )
}

/// The local embedder. Match-dispatched (no trait objects), mirroring `Backend`.
///
/// Both arms apply the shared E5 `passage:`/`query:` prefixes. The methods are async
/// so the `Ollama` HTTP arm fits naturally; `Ort` runs synchronous CPU inference.
#[cfg(feature = "duckdb")]
pub enum Embedder {
    // Boxed: OrtEmbedder owns an ONNX session + tokenizer and is far larger than the
    // Ollama variant (clippy::large_enum_variant).
    #[cfg(feature = "ort")]
    Ort(Box<ort_impl::OrtEmbedder>),
    #[cfg(feature = "ollama")]
    Ollama(ollama_impl::OllamaEmbedder),
}

#[cfg(feature = "duckdb")]
impl Embedder {
    /// Embed a batch of passages (each gets the `passage: ` prefix).
    pub async fn embed_passages(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        match self {
            #[cfg(feature = "ort")]
            Embedder::Ort(e) => e.embed_passages(texts),
            #[cfg(feature = "ollama")]
            Embedder::Ollama(e) => e.embed_passages(texts).await,
            // No embedder feature compiled in: the factory rejects this before
            // construction, but the match must stay total.
            #[cfg(not(any(feature = "ort", feature = "ollama")))]
            _ => {
                let _ = texts;
                anyhow::bail!(
                    "no embedder compiled in (build with --features ort or --features ollama)"
                )
            }
        }
    }

    /// Embed a single query (gets the `query: ` prefix).
    pub async fn embed_query(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        match self {
            #[cfg(feature = "ort")]
            Embedder::Ort(e) => e.embed_query(text),
            #[cfg(feature = "ollama")]
            Embedder::Ollama(e) => e.embed_query(text).await,
            #[cfg(not(any(feature = "ort", feature = "ollama")))]
            _ => {
                let _ = text;
                anyhow::bail!(
                    "no embedder compiled in (build with --features ort or --features ollama)"
                )
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Ort embedder — raw ONNX Runtime (no fastembed).
// ---------------------------------------------------------------------------
#[cfg(feature = "ort")]
pub mod ort_impl {
    use super::{PrefixStyle, format_passage, format_query};
    use anyhow::{Context, Result};
    use hf_hub::api::sync::ApiBuilder;
    use ndarray::{Array2, Axis};
    use ort::session::Session;
    use ort::value::Tensor;
    use std::path::Path;
    use std::sync::Mutex;
    use tokenizers::Tokenizer;

    /// Passages embedded per ONNX forward pass.
    const EMBED_BATCH: usize = 32;
    /// E5 context window — truncate/pad tokens to this length.
    const MAX_TOKENS: usize = 512;

    /// Local ONNX E5 embedder: an `ort::Session` over `onnx/model.onnx` plus the
    /// matching `tokenizer.json`, both pulled from a HuggingFace repo via `hf-hub`.
    ///
    /// Pipeline (correctness-critical): prefix → tokenize (pad/trunc 512) → feed
    /// `input_ids` + `attention_mask` (+ zeroed `token_type_ids` iff the model
    /// declares it) → run → `last_hidden_state` [batch, seq, dim] → MEAN-POOL over
    /// the attention mask → L2-NORMALIZE.
    pub struct OrtEmbedder {
        /// Mutex because ort 2.0.0-rc.10+ requires `&mut Session` for `run()`; usage is
        /// single-threaded (the worker owns the backend), so the lock is uncontended.
        session: Mutex<Session>,
        tokenizer: Tokenizer,
        /// Whether the loaded model declares a `token_type_ids` input.
        needs_token_type_ids: bool,
        /// Model-aware prefix policy (E5 by default; Qwen / None for other models).
        prefix_style: PrefixStyle,
    }

    impl OrtEmbedder {
        /// Download (or reuse from cache) the model + tokenizer for `model_repo` and
        /// build the ONNX session. `model_cache` sets the HF cache dir for offline reuse.
        pub fn new(
            model_repo: &str,
            model_cache: Option<&str>,
            prefix_style: PrefixStyle,
        ) -> Result<Self> {
            // from_env (not new): honors HF_HOME/HF_TOKEN like hf-hub <= 0.3 did, which the
            // Docker images and CI rely on to relocate the model cache.
            let mut builder = ApiBuilder::from_env().with_progress(true);
            if let Some(dir) = model_cache {
                builder = builder.with_cache_dir(dir.into());
            }
            let api = builder
                .build()
                .context("failed to initialize the HuggingFace Hub client")?;
            let repo = api.model(model_repo.to_string());

            let model_path = repo.get("onnx/model.onnx").with_context(|| {
                format!("failed to download onnx/model.onnx from {model_repo} (check network or set duckdb.model_cache to a pre-populated dir)")
            })?;
            let tokenizer_path = repo
                .get("tokenizer.json")
                .with_context(|| format!("failed to download tokenizer.json from {model_repo}"))?;

            Self::from_files(&model_path, &tokenizer_path, prefix_style)
        }

        /// Build the embedder from already-resolved model + tokenizer file paths.
        fn from_files(
            model_path: &Path,
            tokenizer_path: &Path,
            prefix_style: PrefixStyle,
        ) -> Result<Self> {
            let mut tokenizer = Tokenizer::from_file(tokenizer_path)
                .map_err(|e| anyhow::anyhow!("failed to load tokenizer: {e}"))?;
            // Truncate/pad to the E5 context window so every row in a batch is the same
            // length (ONNX needs rectangular tensors) and never exceeds 512 tokens.
            let truncation = tokenizers::TruncationParams {
                max_length: MAX_TOKENS,
                ..Default::default()
            };
            tokenizer
                .with_truncation(Some(truncation))
                .map_err(|e| anyhow::anyhow!("failed to set tokenizer truncation: {e}"))?;
            tokenizer.with_padding(Some(tokenizers::PaddingParams {
                strategy: tokenizers::PaddingStrategy::BatchLongest,
                ..Default::default()
            }));

            // Size the ONNX intra-op thread pool to the machine. Indexing is a one-shot,
            // throughput-bound batch job, so we want every core working the forward pass;
            // the default pool can be conservative. Falls back to 1 if the count is
            // unavailable.
            let intra_threads = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1);
            // ort's builder errors are generic over the builder (for recovery) and don't
            // satisfy anyhow's Context bounds, so stringify them via map_err.
            let session = Session::builder()
                .map_err(|e| anyhow::anyhow!("failed to create ONNX session builder: {e}"))?
                .with_intra_threads(intra_threads)
                .map_err(|e| anyhow::anyhow!("failed to set ONNX intra-op thread count: {e}"))?
                .commit_from_file(model_path)
                .map_err(|e| {
                    anyhow::anyhow!(
                        "failed to load ONNX model from {}: {e}",
                        model_path.display()
                    )
                })?;

            let needs_token_type_ids = session.inputs().iter().any(|i| i.name() == "token_type_ids");

            Ok(Self {
                session: Mutex::new(session),
                tokenizer,
                needs_token_type_ids,
                prefix_style,
            })
        }

        /// Tokenize, run inference, mean-pool over the attention mask, L2-normalize.
        ///
        /// Inputs are length-sorted before batching. Padding is `BatchLongest`, so one
        /// long passage otherwise inflates its whole batch and wastes compute on padding.
        /// We sort by char length (a cheap proxy for token length — no extra tokenize pass)
        /// and scatter results back to the caller's order, so output ordering is unchanged.
        fn embed_prefixed(&self, prefixed: &[String]) -> Result<Vec<Vec<f32>>> {
            let mut order: Vec<usize> = (0..prefixed.len()).collect();
            order.sort_by_key(|&i| prefixed[i].len());

            let mut out: Vec<Vec<f32>> = vec![Vec::new(); prefixed.len()];
            for idx_batch in order.chunks(EMBED_BATCH) {
                let texts: Vec<String> = idx_batch.iter().map(|&i| prefixed[i].clone()).collect();
                // embed_batch preserves input order, so vectors[k] maps to idx_batch[k].
                for (&orig, vec) in idx_batch.iter().zip(self.embed_batch(texts)?) {
                    out[orig] = vec;
                }
            }
            Ok(out)
        }

        fn embed_batch(&self, batch: Vec<String>) -> Result<Vec<Vec<f32>>> {
            let encodings = self
                .tokenizer
                .encode_batch(batch, true)
                .map_err(|e| anyhow::anyhow!("tokenization failed: {e}"))?;

            let batch_size = encodings.len();
            let seq_len = encodings.first().map(|e| e.get_ids().len()).unwrap_or(0);
            if batch_size == 0 || seq_len == 0 {
                return Ok(Vec::new());
            }

            let mut ids = Vec::with_capacity(batch_size * seq_len);
            let mut mask = Vec::with_capacity(batch_size * seq_len);
            for enc in &encodings {
                ids.extend(enc.get_ids().iter().map(|&i| i as i64));
                mask.extend(enc.get_attention_mask().iter().map(|&m| m as i64));
            }

            let ids_arr = Array2::<i64>::from_shape_vec((batch_size, seq_len), ids)
                .context("failed to build input_ids tensor")?;
            let mask_arr = Array2::<i64>::from_shape_vec((batch_size, seq_len), mask.clone())
                .context("failed to build attention_mask tensor")?;

            let id_tensor =
                Tensor::from_array(ids_arr).context("failed to wrap input_ids in a Tensor")?;
            let mask_tensor = Tensor::from_array(mask_arr)
                .context("failed to wrap attention_mask in a Tensor")?;

            let mut session = self
                .session
                .lock()
                .map_err(|e| anyhow::anyhow!("ONNX session lock poisoned: {e}"))?;
            let outputs = if self.needs_token_type_ids {
                let tt_arr = Array2::<i64>::zeros((batch_size, seq_len));
                let tt_tensor = Tensor::from_array(tt_arr)
                    .context("failed to wrap token_type_ids in a Tensor")?;
                session
                    .run(ort::inputs![
                        "input_ids" => id_tensor,
                        "attention_mask" => mask_tensor,
                        "token_type_ids" => tt_tensor,
                    ])
                    .context("ONNX inference failed")?
            } else {
                session
                    .run(ort::inputs![
                        "input_ids" => id_tensor,
                        "attention_mask" => mask_tensor,
                    ])
                    .context("ONNX inference failed")?
            };

            // last_hidden_state: [batch, seq, hidden]. Some exports name the first
            // output differently, so fall back to the first output by index.
            let hidden = match outputs.get("last_hidden_state") {
                Some(v) => v,
                None => &outputs[0],
            };
            let (shape, data) = hidden
                .try_extract_tensor::<f32>()
                .context("failed to extract last_hidden_state as f32")?;
            if shape.len() != 3 {
                anyhow::bail!(
                    "expected last_hidden_state rank 3 [batch, seq, hidden], got shape {shape:?}"
                );
            }
            let hidden_dim = shape[2] as usize;
            let model_seq = shape[1] as usize;

            // Mean-pool over the attention mask, then L2-normalize each row.
            let mask_arr2 = Array2::<i64>::from_shape_vec((batch_size, seq_len), mask)
                .context("failed to rebuild attention_mask for pooling")?;
            let mut result = Vec::with_capacity(batch_size);
            for b in 0..batch_size {
                let mut pooled = vec![0f32; hidden_dim];
                let mut mask_sum = 0f32;
                let row_mask = mask_arr2.index_axis(Axis(0), b);
                for t in 0..model_seq {
                    let m = *row_mask.get(t).unwrap_or(&0) as f32;
                    if m == 0.0 {
                        continue;
                    }
                    mask_sum += m;
                    let base = (b * model_seq + t) * hidden_dim;
                    for (d, p) in pooled.iter_mut().enumerate() {
                        *p += data[base + d] * m;
                    }
                }
                let denom = mask_sum.max(1e-9);
                for p in pooled.iter_mut() {
                    *p /= denom;
                }
                // L2-normalize.
                let norm: f32 = pooled.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-12);
                for p in pooled.iter_mut() {
                    *p /= norm;
                }
                result.push(pooled);
            }
            Ok(result)
        }

        pub fn embed_passages(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            let prefixed: Vec<String> = texts
                .iter()
                .map(|t| format_passage(self.prefix_style, t))
                .collect();
            self.embed_prefixed(&prefixed)
        }

        pub fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
            let prefixed = vec![format_query(self.prefix_style, text)];
            let mut out = self.embed_prefixed(&prefixed)?;
            out.pop()
                .context("ort embedder returned no embedding for the query")
        }
    }
}

// ---------------------------------------------------------------------------
// Ollama embedder — HTTP /api/embed.
// ---------------------------------------------------------------------------
#[cfg(feature = "ollama")]
pub mod ollama_impl {
    use super::{PrefixStyle, format_passage, format_query};
    use anyhow::{Context, Result};
    use serde::Deserialize;
    use serde_json::json;

    /// Embed-endpoint response: `{ "embeddings": [[f32, ...], ...] }`.
    #[derive(Deserialize)]
    pub struct EmbedResponse {
        pub embeddings: Vec<Vec<f32>>,
    }

    /// Remote Ollama embedder. POSTs prefixed inputs to `{url}/api/embed`.
    pub struct OllamaEmbedder {
        client: reqwest::Client,
        url: String,
        model: String,
        /// Model-aware prefix policy (E5 by default; Qwen / None for other models).
        prefix_style: PrefixStyle,
    }

    impl OllamaEmbedder {
        /// `model` is required (no E5 default — Ollama models vary). `url` defaults
        /// upstream (config `ollama.url`, default `http://localhost:11434`).
        pub fn new(url: &str, model: Option<&str>, prefix_style: PrefixStyle) -> Result<Self> {
            let model = model
                .filter(|m| !m.is_empty())
                .context("embedder 'ollama' selected but ollama.model is unset — set ollama.model in the config to an embed-capable Ollama model (e.g. nomic-embed-text)")?
                .to_string();
            Ok(Self {
                client: reqwest::Client::new(),
                url: url.trim_end_matches('/').to_string(),
                model,
                prefix_style,
            })
        }

        /// Parse `/api/embed` JSON shape from raw bytes. Extracted so tests can
        /// exercise it against a canned payload without a live server.
        pub fn parse_response(body: &[u8]) -> Result<Vec<Vec<f32>>> {
            let parsed: EmbedResponse = serde_json::from_slice(body)
                .context("failed to parse Ollama /api/embed response")?;
            Ok(parsed.embeddings)
        }

        async fn embed_inputs(&self, inputs: Vec<String>) -> Result<Vec<Vec<f32>>> {
            if inputs.is_empty() {
                return Ok(Vec::new());
            }
            let endpoint = format!("{}/api/embed", self.url);
            let resp = self
                .client
                .post(&endpoint)
                .json(&json!({ "model": self.model, "input": inputs }))
                .send()
                .await
                .with_context(|| {
                    format!("failed to reach Ollama at {endpoint} — is `ollama serve` running?")
                })?;
            let status = resp.status();
            let body = resp
                .bytes()
                .await
                .context("failed to read Ollama response body")?;
            if !status.is_success() {
                anyhow::bail!(
                    "Ollama /api/embed returned {status}: {} (is the model '{}' pulled? try `ollama pull {}`)",
                    String::from_utf8_lossy(&body).trim(),
                    self.model,
                    self.model
                );
            }
            Self::parse_response(&body)
        }

        pub async fn embed_passages(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            let inputs: Vec<String> = texts
                .iter()
                .map(|t| format_passage(self.prefix_style, t))
                .collect();
            self.embed_inputs(inputs).await
        }

        pub async fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
            let mut out = self
                .embed_inputs(vec![format_query(self.prefix_style, text)])
                .await?;
            out.pop()
                .context("ollama embedder returned no embedding for the query")
        }
    }
}

#[cfg(all(test, any(feature = "ort", feature = "ollama")))]
mod tests {
    use super::*;

    #[test]
    fn e5_passage_and_query_prefixes_apply() {
        assert_eq!(format_passage(PrefixStyle::E5, "foo"), "passage: foo");
        assert_eq!(format_query(PrefixStyle::E5, "foo"), "query: foo");
    }

    /// Qwen: passages are bare; the query is wrapped with the task instruction.
    #[test]
    fn qwen_passage_bare_query_instructed() {
        assert_eq!(format_passage(PrefixStyle::Qwen, "foo"), "foo");
        let q = format_query(PrefixStyle::Qwen, "find the parser");
        assert!(q.starts_with("Instruct: Given a code search query"));
        assert!(q.ends_with("\nQuery: find the parser"));
    }

    /// None: both sides bare.
    #[test]
    fn none_style_is_bare_both_sides() {
        assert_eq!(format_passage(PrefixStyle::None, "foo"), "foo");
        assert_eq!(format_query(PrefixStyle::None, "foo"), "foo");
    }

    /// Ort: downloads the model (network) — gated behind `--features ort`. Proves
    /// 384-d output, distinct vectors for distinct text, and a near-unit L2 norm
    /// (validates the mean-pool + normalize pipeline).
    #[cfg(feature = "ort")]
    #[test]
    fn ort_embeds_384d_normalized_and_distinguishes_texts() {
        use crate::vectordbs::embedder::ort_impl::OrtEmbedder;

        const E5_DIM: usize = 384;
        let embedder = OrtEmbedder::new("Xenova/multilingual-e5-small", None, PrefixStyle::E5)
            .expect("ort model init (needs network or a populated model cache)");
        let texts = vec![
            "fn add(a: i32, b: i32) -> i32 { a + b }".to_string(),
            "the quick brown fox jumps over the lazy dog".to_string(),
        ];
        let vecs = embedder.embed_passages(&texts).expect("embed_passages");
        assert_eq!(vecs.len(), 2);
        assert_eq!(vecs[0].len(), E5_DIM, "passage vectors must be 384d");
        assert_eq!(vecs[1].len(), E5_DIM);
        assert_ne!(vecs[0], vecs[1], "distinct texts → distinct vectors");

        // L2-normalized → norm ≈ 1.
        let norm: f32 = vecs[0].iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!(
            (norm - 1.0).abs() < 1e-3,
            "expected unit-norm vector, got {norm}"
        );

        let q = embedder
            .embed_query("how to add two integers")
            .expect("embed_query");
        assert_eq!(q.len(), E5_DIM, "query vector must be 384d");
    }

    /// Ollama: validate the response JSON shape against a canned payload — NO live
    /// server required, so CI never depends on a running Ollama.
    #[cfg(feature = "ollama")]
    #[test]
    fn ollama_parses_embed_response_shape() {
        use crate::vectordbs::embedder::ollama_impl::OllamaEmbedder;

        let body = br#"{"model":"nomic-embed-text","embeddings":[[0.1,0.2,0.3],[0.4,0.5,0.6]]}"#;
        let vecs = OllamaEmbedder::parse_response(body).expect("parse canned response");
        assert_eq!(vecs.len(), 2);
        assert_eq!(vecs[0], vec![0.1, 0.2, 0.3]);
        assert_eq!(vecs[1], vec![0.4, 0.5, 0.6]);
    }

    /// Ollama: model is REQUIRED — construction must fail clearly when unset.
    #[cfg(feature = "ollama")]
    #[test]
    fn ollama_requires_model() {
        use crate::vectordbs::embedder::ollama_impl::OllamaEmbedder;

        let result = OllamaEmbedder::new("http://localhost:11434", None, PrefixStyle::E5);
        match result {
            Ok(_) => panic!("missing model must error"),
            Err(e) => assert!(e.to_string().contains("ollama.model")),
        }
    }
}
