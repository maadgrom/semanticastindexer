//! Qdrant backend. Two embedding modes, selected by the `embedder` field:
//!
//! - **`embedder: qdrant` (default):** embeddings are produced inside the Qdrant Cloud
//!   cluster (Inference enabled) by passing `Document::new(text, model)` — no client-side
//!   model is loaded here. Stored chunks are embedded as `passage: <code>` and queries as
//!   `query: <text>`. This is the historical, backward-compatible path.
//! - **`embedder: ort` / `ollama` (needs `--features qdrant,ort` / `qdrant,ollama`):**
//!   embeddings are produced locally via the shared [`Embedder`], and RAW `Vec<f32>` points
//!   are upserted (`PointStruct::new(id, vec, payload)`). The query side embeds locally and
//!   delegates to the single [`QdrantBackend::query_by_vector`] NN core. This unlocks
//!   OSS/self-hosted Qdrant (no inference engine) and code models without Cloud billing.
//!   Both modes build structurally byte-identical payloads via the shared
//!   [`QdrantBackend::payload_for`].

use anyhow::{Context, Result};
use serde_json::json;
use std::collections::HashMap;

#[cfg(any(feature = "ort", feature = "ollama"))]
use crate::vectordbs::embedder::Embedder;

use qdrant_client::Payload;
use qdrant_client::Qdrant;
use qdrant_client::qdrant::value::Kind;
use qdrant_client::qdrant::{
    Condition, CreateCollectionBuilder, CreateFieldIndexCollectionBuilder, DeletePointsBuilder,
    Distance, Document, FieldType, Filter, GetPointsBuilder, OptimizersConfigDiffBuilder,
    PointStruct, Query, QueryPointsBuilder, ScrollPointsBuilder, UpdateCollectionBuilder,
    UpsertPointsBuilder, Value, VectorParamsBuilder, VectorsOutput,
};

use crate::config::Plan;
use crate::vectordbs::{CodeChunk, Hit, PrefixStyle, format_passage, format_query};

/// Upsert batch size — server-side inference runs per request, so keep it modest.
const UPSERT_BATCH: usize = 32;
/// Qdrant's default `indexing_threshold` (KB of vectors before the HNSW index builds).
/// `end_bulk` restores this; we never set a custom value at creation, so it is the server
/// default. Setting the threshold to `0` (in `begin_bulk`) disables index building.
const DEFAULT_INDEXING_THRESHOLD: u64 = 20_000;
/// Over-fetch factor for vector search: HNSW can surface the same id more than once,
/// so fetch extra candidates, dedup by id, then truncate. Mirrors the DuckDB path.
const QUERY_OVERFETCH: u64 = 8;
/// Page size when scrolling all points for `find_duplicates`.
const SCROLL_PAGE: u32 = 256;

/// Qdrant backend: wraps the client plus the collection/model/dim from the plan.
pub struct QdrantBackend {
    client: Qdrant,
    collection: String,
    model: String,
    vector_dim: u64,
    /// Embedding prefix policy (Qdrant's model is e5, but route through the shared
    /// helper so it stays consistent with the local embedders).
    prefix_style: PrefixStyle,
    /// Local embedder for the `embedder: ort` / `ollama` modes. `None` = server-side
    /// inference (the default/historical `embedder: qdrant` path). Only exists when an
    /// embedder feature is compiled in; a bare `--features qdrant` build contains only the
    /// server-side path.
    #[cfg(any(feature = "ort", feature = "ollama"))]
    embedder: Option<Embedder>,
}

/// Build a Qdrant client. The cluster URL comes from the `QDRANT_URL` env var (which
/// wins) or `qdrant.url` in `sai-cfg.yml`. The API key is a SECRET and is read ONLY
/// from `QDRANT_API_KEY` in the environment (never from YAML). Shared by both
/// [`QdrantBackend::connect`] (server-side) and [`QdrantBackend::connect_local`].
fn build_client(plan: &Plan) -> Result<Qdrant> {
    let url = std::env::var("QDRANT_URL")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| plan.qdrant_url.clone())
        .context(
            "set the Qdrant URL via the QDRANT_URL env var or `qdrant.url` in sai-cfg.yml, \
             e.g. https://<id>.<region>.aws.cloud.qdrant.io:6334",
        )?;
    let mut builder = Qdrant::from_url(&url);
    match std::env::var("QDRANT_API_KEY") {
        Ok(key) if !key.is_empty() => builder = builder.api_key(key),
        _ => {
            tracing::warn!("QDRANT_API_KEY not set — Qdrant Cloud will reject the request")
        }
    }
    Ok(builder.build()?)
}

impl QdrantBackend {
    /// Build a Qdrant client in SERVER-side inference mode (the historical default). The
    /// cluster URL comes from `QDRANT_URL` (which wins) or `qdrant.url` in `sai-cfg.yml`;
    /// the API key is read ONLY from `QDRANT_API_KEY` (never from YAML).
    pub fn connect(plan: &Plan) -> Result<Self> {
        Ok(Self {
            client: build_client(plan)?,
            collection: plan.collection.clone(),
            model: plan.model.clone(),
            vector_dim: plan.vector_dim,
            prefix_style: plan.prefix_style,
            // Server-side inference: no local embedder.
            #[cfg(any(feature = "ort", feature = "ollama"))]
            embedder: None,
        })
    }

    /// Build a Qdrant backend in LOCAL-EMBED mode (`embedder: ort` / `ollama`): the same
    /// client/collection/dim setup as [`connect`], but it owns a local [`Embedder`] used to
    /// embed passages on upsert and queries on search, upserting RAW `Vec<f32>` points. This
    /// is what makes OSS/self-hosted Qdrant (no inference engine) usable. The embedder is
    /// built by the factory only when this mode is selected.
    #[cfg(any(feature = "ort", feature = "ollama"))]
    pub fn connect_local(plan: &Plan, embedder: Embedder) -> Result<Self> {
        tracing::info!(
            embedder = %plan.embedder,
            model = %plan.model,
            dims = plan.vector_dim,
            "qdrant: local-embed mode"
        );
        Ok(Self {
            client: build_client(plan)?,
            collection: plan.collection.clone(),
            model: plan.model.clone(),
            vector_dim: plan.vector_dim,
            prefix_style: plan.prefix_style,
            embedder: Some(embedder),
        })
    }

    /// Create the collection if missing (recreate on demand). Vector size/distance from the plan.
    /// Also creates a keyword payload index on `path` so sync's delete-by-path filter is fast.
    #[tracing::instrument(skip(self), fields(collection = %self.collection))]
    pub async fn ensure_ready(&self, recreate: bool) -> Result<()> {
        let exists = self.client.collection_exists(&self.collection).await?;
        if exists && recreate {
            self.client.delete_collection(&self.collection).await?;
            tracing::info!(collection = %self.collection, "dropped existing collection");
        }
        if !self.client.collection_exists(&self.collection).await? {
            self.client
                .create_collection(
                    CreateCollectionBuilder::new(&self.collection).vectors_config(
                        VectorParamsBuilder::new(self.vector_dim, Distance::Cosine),
                    ),
                )
                .await?;
            // Keyword index on `path` enables efficient delete-by-path during sync.
            self.client
                .create_field_index(CreateFieldIndexCollectionBuilder::new(
                    &self.collection,
                    "path",
                    FieldType::Keyword,
                ))
                .await?;
            tracing::info!(
                collection = %self.collection,
                dims = self.vector_dim,
                "created collection (cosine, path index)"
            );
        } else {
            self.validate_existing_collection_dim().await?;
            tracing::info!(collection = %self.collection, "using existing collection");
        }
        Ok(())
    }

    /// Begin a bulk-insert window: disable HNSW index building (`indexing_threshold = 0`)
    /// so a large upsert is not slowed by incremental graph maintenance and repeated
    /// re-indexing of growing segments. Mirrors the DuckDB backend's drop-index-before-bulk
    /// step; [`end_bulk`](Self::end_bulk) restores the threshold so the index builds once.
    /// This is Qdrant's documented bulk-upload optimization.
    pub async fn begin_bulk(&self) -> Result<()> {
        self.set_indexing_threshold(0)
            .await
            .context("failed to disable indexing for the bulk window")
    }

    /// End a bulk-insert window: restore the default `indexing_threshold`, letting the
    /// optimizer build the HNSW index in a single pass over the freshly-upserted points.
    pub async fn end_bulk(&self) -> Result<()> {
        self.set_indexing_threshold(DEFAULT_INDEXING_THRESHOLD)
            .await
            .context("failed to re-enable indexing after the bulk window")
    }

    /// Update the collection's `indexing_threshold` (KB). `0` disables HNSW building
    /// entirely; the default rebuilds it. Brackets bulk upserts — see
    /// [`begin_bulk`](Self::begin_bulk) / [`end_bulk`](Self::end_bulk).
    async fn set_indexing_threshold(&self, threshold: u64) -> Result<()> {
        self.client
            .update_collection(
                UpdateCollectionBuilder::new(&self.collection).optimizers_config(
                    OptimizersConfigDiffBuilder::default().indexing_threshold(threshold),
                ),
            )
            .await?;
        Ok(())
    }

    /// Build the payload for a chunk. SINGLE SOURCE OF TRUTH shared by BOTH the server
    /// (`Document`) and local (raw-vector) build paths, so payloads are byte-identical
    /// across modes (same key order, same conditional `symbol` insertion) — only the
    /// vector source differs.
    fn payload_for(c: &CodeChunk) -> Result<Payload> {
        let mut payload_json = json!({
            "path": c.path,
            "language": c.language,
            "start_line": c.start_line as i64,
            "end_line": c.end_line as i64,
            "text": c.text,
            "commit": c.commit_sha,
            "dirty": c.dirty,
            "no_duplicate": c.no_duplicate,
        });
        if let Some(symbol) = &c.symbol {
            payload_json["symbol"] = json!(symbol);
        }
        Payload::try_from(payload_json).context("failed to build Qdrant payload from chunk")
    }

    /// Upsert a batch of chunks. In SERVER mode each point is a `passage:`-prefixed
    /// `Document` (server-side inference). In LOCAL mode the chunk slice is embedded AS-IS
    /// via the owned [`Embedder`] (callers already pass bounded ≤`UPSERT_BATCH` slices, so
    /// no second batching layer), each vector is dim-checked, and RAW `Vec<f32>` points are
    /// upserted. Network upserts use the existing `UPSERT_BATCH` batches in both modes.
    pub async fn upsert(&self, chunks: &[CodeChunk]) -> Result<()> {
        let points = self.build_points(chunks).await?;
        // Local mode embeds without server inference; server mode relies on it.
        let local = self.is_local();
        for batch in points.chunks(UPSERT_BATCH) {
            let n = batch.len();
            self.client
                .upsert_points(
                    UpsertPointsBuilder::new(&self.collection, batch.to_vec()).wait(true),
                )
                .await
                .with_context(|| {
                    if local {
                        format!("upsert of {n} points failed")
                    } else {
                        format!(
                            "upsert of {n} points failed (is Inference enabled on the cluster?)"
                        )
                    }
                })?;
        }
        Ok(())
    }

    /// Whether this backend embeds locally (`embedder: ort` / `ollama`). Always `false` in
    /// a build without an embedder feature (server-side inference only).
    fn is_local(&self) -> bool {
        #[cfg(any(feature = "ort", feature = "ollama"))]
        {
            self.embedder.is_some()
        }
        #[cfg(not(any(feature = "ort", feature = "ollama")))]
        {
            false
        }
    }

    /// Build points from chunks. SERVER mode builds `Document` points (server-side
    /// inference); LOCAL mode embeds passages locally and builds RAW `Vec<f32>` points,
    /// dim-checking each vector BEFORE constructing the `PointStruct`. Both modes share
    /// [`Self::payload_for`], so the payload is byte-identical across modes.
    async fn build_points(&self, chunks: &[CodeChunk]) -> Result<Vec<PointStruct>> {
        #[cfg(any(feature = "ort", feature = "ollama"))]
        if let Some(embedder) = &self.embedder {
            if chunks.is_empty() {
                return Ok(Vec::new());
            }
            // Embed the received slice as-is — callers already bound it to ≤UPSERT_BATCH
            // and the embedder batches internally; do NOT add a second batching layer.
            let texts: Vec<String> = chunks.iter().map(|c| c.text.clone()).collect();
            let vectors = embedder.embed_passages(&texts).await?;
            if vectors.len() != chunks.len() {
                anyhow::bail!(
                    "embedder returned {} vectors for {} chunks",
                    vectors.len(),
                    chunks.len()
                );
            }
            let mut points = Vec::with_capacity(chunks.len());
            for (c, vec) in chunks.iter().zip(vectors) {
                super::check_dim(vec.len(), self.vector_dim)?;
                let payload = Self::payload_for(c)?;
                // `Vec<f32>` implements `Into<Vectors>`: a raw dense vector, no Document.
                points.push(PointStruct::new(c.id, vec, payload));
            }
            return Ok(points);
        }
        // Server mode: `Document::new(...)` points (server-side inference). Same payload
        // helper as local, so only the vector source differs.
        let mut points = Vec::with_capacity(chunks.len());
        for c in chunks {
            let payload = Self::payload_for(c)?;
            let document = Document::new(format_passage(self.prefix_style, &c.text), &self.model);
            points.push(PointStruct::new(c.id, document, payload));
        }
        Ok(points)
    }

    /// Delete every point whose `path` payload equals `path`.
    pub async fn delete_by_path(&self, path: &str) -> Result<()> {
        self.client
            .delete_points(
                DeletePointsBuilder::new(&self.collection)
                    .points(Filter::must([Condition::matches("path", path.to_string())]))
                    .wait(true),
            )
            .await
            .with_context(|| format!("delete of points for {path} failed"))?;
        Ok(())
    }

    /// Nearest-neighbour search. In LOCAL mode embed the query locally (`embed_query` +
    /// `check_dim`) and DELEGATE to the single [`Self::query_by_vector`] NN core — so CLI
    /// `query` (local) and the MCP `SearchByQuery` path return identical results (same
    /// over-fetch/dedup). In SERVER mode use a `query:`-prefixed `Document` (server-side
    /// inference), the historical path. The `Backend::query` signature is unchanged.
    pub async fn query(&self, q: &str, limit: u64) -> Result<Vec<Hit>> {
        #[cfg(any(feature = "ort", feature = "ollama"))]
        if self.embedder.is_some() {
            // `embed_query` already calls `check_dim`. One NN core: query_by_vector.
            let vec = self.embed_query(q).await?;
            return self.query_by_vector(&vec, limit, None).await;
        }
        let response = self
            .client
            .query(
                QueryPointsBuilder::new(&self.collection)
                    .query(Query::new_nearest(Document::new(
                        format_query(self.prefix_style, q),
                        &self.model,
                    )))
                    .limit(limit)
                    .with_payload(true),
            )
            .await?;

        Ok(response
            .result
            .into_iter()
            .map(|p| {
                let payload = &p.payload;
                Hit {
                    id: 0,
                    path: payload_str(payload, "path"),
                    language: payload_str(payload, "language"),
                    start_line: payload_int(payload, "start_line"),
                    end_line: payload_int(payload, "end_line"),
                    text: payload_str(payload, "text"),
                    score: p.score,
                    symbol: payload_str_opt(payload, "symbol"),
                    commit_sha: payload
                        .get("commit")
                        .and_then(|v| v.as_str().map(|s| s.to_string())),
                    dirty: payload
                        .get("dirty")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                    no_duplicate: false,
                }
            })
            .collect())
    }

    /// Nearest-neighbour search by a RAW vector (no embedding). Over-fetches + dedups by
    /// the stored point id, optionally excluding `exclude_id` (self-exclusion). Scores are
    /// Qdrant cosine similarities (already `1 - distance` semantics).
    pub async fn query_by_vector(
        &self,
        vec: &[f32],
        limit: u64,
        exclude_id: Option<u64>,
    ) -> Result<Vec<Hit>> {
        let fetch = limit.saturating_mul(QUERY_OVERFETCH).max(limit);
        let response = self
            .client
            .query(
                QueryPointsBuilder::new(&self.collection)
                    .query(Query::new_nearest(vec.to_vec()))
                    .limit(fetch)
                    .with_payload(true),
            )
            .await?;
        let mut seen = std::collections::HashSet::new();
        let mut out: Vec<Hit> = Vec::new();
        for p in response.result {
            let id = point_id_u64(&p.id);
            if Some(id) == exclude_id {
                continue;
            }
            if !seen.insert(id) {
                continue;
            }
            let payload = &p.payload;
            out.push(Hit {
                id,
                path: payload_str(payload, "path"),
                language: payload_str(payload, "language"),
                start_line: payload_int(payload, "start_line"),
                end_line: payload_int(payload, "end_line"),
                text: payload_str(payload, "text"),
                score: p.score,
                symbol: payload_str_opt(payload, "symbol"),
                commit_sha: payload
                    .get("commit")
                    .and_then(|v| v.as_str().map(|s| s.to_string())),
                dirty: payload
                    .get("dirty")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                no_duplicate: payload
                    .get("no_duplicate")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
            });
            if out.len() >= limit as usize {
                break;
            }
        }
        Ok(out)
    }

    /// Fetch one stored point (plus its vector) by `path` + `line`, keyed by the same
    /// `XxHash64(path, line)` point id the indexer assigns.
    pub async fn get_by_location(
        &self,
        path: &str,
        line: usize,
    ) -> Result<Option<(Hit, Vec<f32>)>> {
        let id = crate::indexer::point_id(path, line);
        let response = self
            .client
            .get_points(
                GetPointsBuilder::new(&self.collection, vec![id.into()])
                    .with_payload(true)
                    .with_vectors(true),
            )
            .await?;
        match response.result.into_iter().next() {
            None => Ok(None),
            Some(p) => {
                let payload = &p.payload;
                let hit = Hit {
                    id,
                    path: payload_str(payload, "path"),
                    language: payload_str(payload, "language"),
                    start_line: payload_int(payload, "start_line"),
                    end_line: payload_int(payload, "end_line"),
                    text: payload_str(payload, "text"),
                    score: 1.0,
                    symbol: payload_str_opt(payload, "symbol"),
                    commit_sha: payload
                        .get("commit")
                        .and_then(|v| v.as_str().map(|s| s.to_string())),
                    dirty: payload
                        .get("dirty")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                    no_duplicate: payload
                        .get("no_duplicate")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                };
                let vec = extract_vector(p.vectors)?;
                Ok(Some((hit, vec)))
            }
        }
    }

    /// Scroll every stored point with its vector (for `find_duplicates`). The optional
    /// `path_glob` is applied in Rust to mirror the DuckDB path.
    pub async fn all_chunks_with_vectors(
        &self,
        path_glob: Option<&str>,
    ) -> Result<Vec<(Hit, Vec<f32>)>> {
        let matcher = match path_glob {
            None => None,
            Some(p) => Some(
                globset::Glob::new(p)
                    .with_context(|| format!("invalid path_glob: {p}"))?
                    .compile_matcher(),
            ),
        };
        let mut out: Vec<(Hit, Vec<f32>)> = Vec::new();
        let mut offset: Option<qdrant_client::qdrant::PointId> = None;
        loop {
            let mut builder = ScrollPointsBuilder::new(&self.collection)
                .limit(SCROLL_PAGE)
                .with_payload(true)
                .with_vectors(true);
            if let Some(o) = offset.clone() {
                builder = builder.offset(o);
            }
            let response = self.client.scroll(builder).await?;
            if response.result.is_empty() {
                break;
            }
            for p in &response.result {
                let payload = &p.payload;
                let hit = Hit {
                    id: point_id_u64(&p.id),
                    path: payload_str(payload, "path"),
                    language: payload_str(payload, "language"),
                    start_line: payload_int(payload, "start_line"),
                    end_line: payload_int(payload, "end_line"),
                    text: payload_str(payload, "text"),
                    score: 1.0,
                    symbol: payload_str_opt(payload, "symbol"),
                    commit_sha: payload
                        .get("commit")
                        .and_then(|v| v.as_str().map(|s| s.to_string())),
                    dirty: payload
                        .get("dirty")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                    no_duplicate: payload
                        .get("no_duplicate")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                };
                if let Some(m) = &matcher
                    && !m.is_match(&hit.path)
                {
                    continue;
                }
                let vec = extract_vector(p.vectors.clone())?;
                out.push((hit, vec));
            }
            offset = response.next_page_offset;
            if offset.is_none() {
                break;
            }
        }
        Ok(out)
    }

    /// Total stored point count for `index_status`.
    #[cfg_attr(not(feature = "mcp"), allow(dead_code))]
    pub async fn chunk_count(&self) -> Result<u64> {
        let info = self.client.collection_info(&self.collection).await?;
        Ok(info.result.and_then(|r| r.points_count).unwrap_or(0))
    }

    /// Stub: dirty awareness for Qdrant can be added via payload filter count later.
    pub async fn has_dirty(&self) -> Result<bool> {
        Ok(false)
    }

    /// Embed a search query (asymmetric `query:` side) through the owned local embedder, or
    /// bail when the backend is in server mode (no local embedder). cfg-pair: the real impl
    /// exists only when an embedder feature is compiled in; the stub keeps the
    /// `Backend::Qdrant(_)` arm resolvable in a bare `--features qdrant` build. Mirrors the
    /// DuckDB twin (which `check_dim`s the result).
    #[cfg(any(feature = "ort", feature = "ollama"))]
    #[cfg_attr(not(feature = "mcp"), allow(dead_code))]
    pub async fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        let embedder = self.embedder.as_ref().context(
            "qdrant backend embeds server-side; no local query embedding (set embedder: ort or ollama)",
        )?;
        let v = embedder.embed_query(text).await?;
        super::check_dim(v.len(), self.vector_dim)?;
        Ok(v)
    }

    /// Bare `--features qdrant` stub: no embedder field exists, so there is no local query
    /// embedding. Keeps `Backend::embed_query`'s qdrant arm resolvable.
    #[cfg(not(any(feature = "ort", feature = "ollama")))]
    #[cfg_attr(not(feature = "mcp"), allow(dead_code))]
    pub async fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        let _ = text;
        anyhow::bail!(
            "qdrant backend embeds server-side; no local query embedding (rebuild with --features qdrant,ort or qdrant,ollama)"
        )
    }

    /// Embed code as a stored PASSAGE (asymmetric `passage:` side) through the owned local
    /// embedder, or bail in server mode. cfg-pair (see [`Self::embed_query`]). Mirrors the
    /// DuckDB twin (which `check_dim`s the result). Feeds MCP `sai_find_similar { code }`.
    #[cfg(any(feature = "ort", feature = "ollama"))]
    pub async fn embed_passage(&self, text: &str) -> Result<Vec<f32>> {
        let embedder = self.embedder.as_ref().context(
            "qdrant backend embeds server-side; no local passage embedding (set embedder: ort or ollama)",
        )?;
        let mut v = embedder.embed_passages(&[text.to_string()]).await?;
        let v = v
            .pop()
            .context("embedder returned no vector for the passage")?;
        super::check_dim(v.len(), self.vector_dim)?;
        Ok(v)
    }

    /// Bare `--features qdrant` stub for [`Self::embed_passage`] (see [`Self::embed_query`]).
    #[cfg(not(any(feature = "ort", feature = "ollama")))]
    pub async fn embed_passage(&self, text: &str) -> Result<Vec<f32>> {
        let _ = text;
        anyhow::bail!(
            "qdrant backend embeds server-side; no local passage embedding (rebuild with --features qdrant,ort or qdrant,ollama)"
        )
    }

    /// Delete the whole collection (flush all vectors).
    pub async fn flush(&self) -> Result<()> {
        if self.client.collection_exists(&self.collection).await? {
            self.client.delete_collection(&self.collection).await?;
            tracing::info!(collection = %self.collection, "flushed: deleted collection");
        } else {
            tracing::info!(collection = %self.collection, "nothing to flush: collection does not exist");
        }
        Ok(())
    }

    /// If the collection already exists (and we are not recreating), validate that its
    /// configured vector dimension matches `self.vector_dim`. This catches the common
    /// mistake of pointing the indexer at an old collection after changing the embedding
    /// model / vector_dim in config. Qdrant will otherwise fail later with dimension
    /// mismatch errors during upsert or query.
    async fn validate_existing_collection_dim(&self) -> Result<()> {
        let info = self.client.collection_info(&self.collection).await?;
        let Some(result) = info.result else {
            return Ok(()); // shouldn't happen on a successful response
        };

        let actual_dim = result
            .config
            .and_then(|cfg| cfg.params)
            .and_then(|params| params.vectors_config)
            .and_then(|vc| vc.config)
            .and_then(|c| match c {
                // Single unnamed vector (the case this indexer always uses).
                qdrant_client::qdrant::vectors_config::Config::Params(p) => Some(p.size),
                // Named vectors (ParamsMap) or other — not used by us.
                _ => None,
            });

        match actual_dim {
            Some(dim) if dim != self.vector_dim => {
                anyhow::bail!(
                    "Qdrant collection '{}' has vector dimension {} but this run uses vector_dim={}. \
                     This usually means the embedding model was changed without recreating the collection. \
                     Re-run with --recreate (or manually delete the collection in the Qdrant Cloud UI).",
                    self.collection,
                    dim,
                    self.vector_dim
                );
            }
            _ => Ok(()),
        }
    }

    /// Test-only backend with a lazily-built dummy client (no network until a request is
    /// made). Lets the offline unit tests exercise `check_dim`/`payload_for` without a live
    /// cluster. Server mode (no local embedder).
    #[cfg(test)]
    fn test_backend(vector_dim: u64) -> Self {
        Self {
            client: Qdrant::from_url("http://127.0.0.1:6334")
                .build()
                .expect("dummy qdrant client builds (lazy channel, no connect)"),
            collection: "test_collection".to_string(),
            model: "intfloat/multilingual-e5-small".to_string(),
            vector_dim,
            prefix_style: PrefixStyle::E5,
            #[cfg(any(feature = "ort", feature = "ollama"))]
            embedder: None,
        }
    }
}

/// Render a string payload field. Only the kinds we actually store are handled.
fn payload_str(payload: &HashMap<String, Value>, key: &str) -> String {
    match payload.get(key).and_then(|v| v.kind.as_ref()) {
        Some(Kind::StringValue(s)) => s.clone(),
        Some(Kind::IntegerValue(i)) => i.to_string(),
        _ => String::new(),
    }
}

/// Render an integer payload field.
fn payload_int(payload: &HashMap<String, Value>, key: &str) -> usize {
    match payload.get(key).and_then(|v| v.kind.as_ref()) {
        Some(Kind::IntegerValue(i)) => (*i).max(0) as usize,
        _ => 0,
    }
}

/// Render an optional string payload field (e.g. `symbol`, set only by the AST chunker).
fn payload_str_opt(payload: &HashMap<String, Value>, key: &str) -> Option<String> {
    match payload.get(key).and_then(|v| v.kind.as_ref()) {
        Some(Kind::StringValue(s)) => Some(s.clone()),
        _ => None,
    }
}

/// Extract the numeric (u64) id from a Qdrant `PointId`. The indexer always stores
/// numeric ids, so a non-numeric id falls back to 0.
fn point_id_u64(id: &Option<qdrant_client::qdrant::PointId>) -> u64 {
    use qdrant_client::qdrant::point_id::PointIdOptions;
    match id.as_ref().and_then(|p| p.point_id_options.as_ref()) {
        Some(PointIdOptions::Num(n)) => *n,
        _ => 0,
    }
}

/// Extract a dense `Vec<f32>` from a retrieved point's `VectorsOutput` (single unnamed
/// vector). Retrieved points carry the `*Output` proto types, not the input `Vectors`.
fn extract_vector(vectors: Option<VectorsOutput>) -> Result<Vec<f32>> {
    use qdrant_client::qdrant::vector_output::Vector as DenseOrSparse;
    use qdrant_client::qdrant::vectors_output::VectorsOptions;
    let v = vectors
        .and_then(|vs| vs.vectors_options)
        .context("point has no vector (was with_vectors(true) set?)")?;
    let out = match v {
        VectorsOptions::Vector(out) => out,
        VectorsOptions::Vectors(_) => {
            anyhow::bail!("named vectors are not supported; expected a single dense vector")
        }
    };
    // Qdrant >= 1.16 returns the dense values in the nested `vector` oneof and leaves
    // the flat `data` field empty (it is `#[deprecated]`); older servers populated
    // `data`. Read the new field first, fall back to the legacy one, so both work.
    // (Mirrors `VectorOutput::into_vector` in qdrant-client.) Without this, every
    // retrieved vector is empty against current Qdrant Cloud and the NN query fails
    // with "expected dim: N, got 0".
    match out.vector {
        Some(DenseOrSparse::Dense(d)) => Ok(d.data),
        Some(DenseOrSparse::Sparse(_)) => {
            anyhow::bail!("sparse vectors are not supported; expected a dense vector")
        }
        Some(DenseOrSparse::MultiDense(_)) => {
            anyhow::bail!("multi-dense vectors are not supported; expected a single dense vector")
        }
        None => {
            #[allow(deprecated)]
            let legacy = out.data;
            if legacy.is_empty() {
                anyhow::bail!(
                    "point vector is empty (neither the nested dense field nor legacy data is set)"
                );
            }
            Ok(legacy)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vectordbs::CodeChunk;

    /// A representative chunk with all optional fields populated.
    fn chunk_with_symbol() -> CodeChunk {
        CodeChunk {
            id: 42,
            path: "src/foo.rs".to_string(),
            language: "rs".to_string(),
            start_line: 10,
            end_line: 20,
            text: "fn foo() {}".to_string(),
            symbol: Some("foo".to_string()),
            commit_sha: Some("abc123".to_string()),
            dirty: true,
            no_duplicate: true,
        }
    }

    /// A chunk with no symbol (line chunker).
    fn chunk_no_symbol() -> CodeChunk {
        CodeChunk {
            symbol: None,
            ..chunk_with_symbol()
        }
    }

    /// `payload_for` is the SINGLE source of truth shared by the server (Document) and
    /// local (raw-vector) build paths, so calling it for the same chunk is byte-identical
    /// BY CONSTRUCTION — proving cross-mode payload identity without a live embedder.
    #[test]
    fn payload_for_is_byte_identical_across_calls() {
        let c = chunk_with_symbol();
        let a = QdrantBackend::payload_for(&c).unwrap();
        let b = QdrantBackend::payload_for(&c).unwrap();
        assert_eq!(a, b, "shared payload_for must produce identical payloads");
    }

    /// `payload_for` carries every stored field with the right kind, and the optional
    /// `symbol` is present only when the chunk has one (line-path byte-identity).
    #[test]
    fn payload_for_includes_symbol_only_when_present() {
        let with = QdrantBackend::payload_for(&chunk_with_symbol()).unwrap();
        let map: HashMap<String, Value> = with.into();
        assert_eq!(payload_str(&map, "path"), "src/foo.rs");
        assert_eq!(payload_str(&map, "language"), "rs");
        assert_eq!(payload_int(&map, "start_line"), 10);
        assert_eq!(payload_int(&map, "end_line"), 20);
        assert_eq!(payload_str(&map, "text"), "fn foo() {}");
        assert_eq!(payload_str(&map, "commit"), "abc123");
        assert_eq!(
            map.get("dirty").and_then(|v| v.as_bool()),
            Some(true),
            "dirty stored as a bool"
        );
        assert_eq!(
            map.get("no_duplicate").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(payload_str_opt(&map, "symbol"), Some("foo".to_string()));

        let without = QdrantBackend::payload_for(&chunk_no_symbol()).unwrap();
        let map: HashMap<String, Value> = without.into();
        assert!(
            !map.contains_key("symbol"),
            "no symbol key when the chunk has none"
        );
    }

    /// The shared `check_dim` guard passes when the produced length equals `vector_dim`,
    /// and bails with the actionable message otherwise — the guard that stops a wrong-dim
    /// vector from ever reaching a `PointStruct`.
    #[test]
    fn check_dim_guards_vector_length() {
        use crate::vectordbs::check_dim;
        let backend = QdrantBackend::test_backend(768);
        assert!(
            check_dim(768, backend.vector_dim).is_ok(),
            "matching dim passes"
        );

        let err = match check_dim(384, backend.vector_dim) {
            Ok(()) => panic!("expected a dim-mismatch error"),
            Err(e) => e.to_string(),
        };
        assert!(err.contains("produced 384-d vectors"), "got: {err}");
        assert!(err.contains("vector_dim=768"), "got: {err}");
    }

    /// In server mode (no local embedder) `build_points` produces `Document` points (not
    /// raw vectors) with the shared payload — the historical, backward-compatible path.
    #[tokio::test]
    async fn server_build_points_uses_documents_with_shared_payload() {
        let backend = QdrantBackend::test_backend(384);
        assert!(!backend.is_local(), "test backend defaults to server mode");
        let chunks = vec![chunk_with_symbol(), chunk_no_symbol()];
        let points = backend.build_points(&chunks).await.unwrap();
        assert_eq!(points.len(), 2);
        // Each point's payload equals the shared payload_for output (byte-identity).
        for (p, c) in points.iter().zip(chunks.iter()) {
            let expected: HashMap<String, Value> = QdrantBackend::payload_for(c).unwrap().into();
            assert_eq!(p.payload, expected, "server payload uses payload_for");
        }
    }
}
