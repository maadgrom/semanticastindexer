//! DuckDB backend: local embeddings (raw ONNX via `ort`, or Ollama HTTP) + VSS/HNSW
//! cosine search, persisted to a single DuckDB file (feature = "duckdb").
//!
//! Mirrors the project's WASM gotcha: INSERT into an HNSW-indexed table is the
//! expensive path (per-row graph maintenance), so bulk windows DROP the index,
//! insert, then RECREATE it. `DELETE` does not trigger HNSW rebuild, so
//! `delete_by_path` needs no index teardown.
//!
//! HNSW persistence on a file-backed DB is experimental and requires
//! `SET hnsw_enable_experimental_persistence = true` (see P2.0 spike note below).

use anyhow::{Context, Result};
use duckdb::Connection;
use duckdb::OptionalExt;
use std::fmt::Write as _;
use std::path::PathBuf;

use crate::config::Plan;
use crate::vectordbs::embedder::Embedder;
use crate::vectordbs::{CodeChunk, Hit};

/// Over-fetch factor for vector search: DuckDB's experimental HNSW can return the same
/// id more than once, so fetch 8x candidates, dedup by id, then truncate to the limit.
const OVERFETCH: u64 = 8;

/// An existing DuckDB index whose stored embedding dimension no longer matches the
/// configured model (the classic "switched models without --recreate" mistake). Carried
/// as a typed error so the CLI can recognize it and offer an interactive delete-and-rebuild
/// instead of string-matching the message. `Display` keeps the original guidance intact.
#[derive(Debug)]
pub struct DimMismatch {
    /// Path of the DuckDB file that must be deleted to recover.
    pub duckdb_path: String,
    message: String,
}

impl std::fmt::Display for DimMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for DimMismatch {}

/// DuckDB-backed vector store. Owns its connection, local embedder, and the
/// resolved collection/dim/path. Single-threaded (the connection is not `Sync`).
pub struct DuckDbBackend {
    conn: Connection,
    embedder: Embedder,
    collection: String,
    vector_dim: u64,
    path: PathBuf,
}

impl DuckDbBackend {
    /// Open (or create) the DuckDB file, load the VSS extension, enable HNSW
    /// persistence, and attach the embedder built by the factory.
    pub fn connect(plan: &Plan, embedder: Embedder) -> Result<Self> {
        let path = PathBuf::from(&plan.duckdb_path);
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create dir for {}", path.display()))?;
        }

        let conn = Connection::open(&path)
            .with_context(|| format!("failed to open DuckDB file at {}", path.display()))?;

        load_vss(&conn)?;
        // LOGICAL CONTRACT: VSS must be loadable on both writable and read-only
        // connections. load_vss now prefers a pure LOAD first so read-only MCP
        // servers (the primary advertised use case) do not require write access
        // at startup time.

        // Required for HNSW indexes to survive across connection close/reopen on a
        // file-backed DB.
        //
        // P2.0 spike outcome (duckdb crate 1.4.4 / bundled DuckDB): VERIFIED working.
        // With this PRAGMA set, an HNSW index created in one session persists in the
        // catalog, returns correct nearest neighbours after reopen, AND accepts
        // post-reopen inserts. No rebuild-on-open fallback is needed.
        conn.execute_batch("SET hnsw_enable_experimental_persistence = true;")
            .context("failed to enable hnsw_enable_experimental_persistence")?;

        let backend = Self {
            conn,
            embedder,
            collection: plan.collection.clone(),
            vector_dim: plan.vector_dim,
            path,
        };
        backend.validate_existing_collection_dim()?;
        Ok(backend)
    }

    /// Open the DuckDB file READ-ONLY for the MCP server. Loads the VSS extension (needed
    /// for `array_cosine_distance`) but does NOT enable HNSW persistence writes — a
    /// read-only handle must not mutate the DB. The file must already exist (a missing
    /// index is an actionable error, since read-only search never indexes).
    pub fn connect_readonly(plan: &Plan, embedder: Embedder) -> Result<Self> {
        let path = PathBuf::from(&plan.duckdb_path);
        if !path.exists() {
            anyhow::bail!(
                "DuckDB index not found at {} — run an index first (the MCP server is read-only)",
                path.display()
            );
        }
        let config = duckdb::Config::default()
            .access_mode(duckdb::AccessMode::ReadOnly)
            .context("failed to build read-only DuckDB config")?;
        let conn = Connection::open_with_flags(&path, config)
            .with_context(|| format!("failed to open DuckDB (read-only) at {}", path.display()))?;
        load_vss(&conn)?;
        // LOGICAL CONTRACT (read-only path): Same VSS requirement as writable path.
        // The improved load_vss makes this viable for pure search/MCP usage.

        let backend = Self {
            conn,
            embedder,
            collection: plan.collection.clone(),
            vector_dim: plan.vector_dim,
            path,
        };
        backend.validate_existing_collection_dim()?;
        Ok(backend)
    }

    /// Fully-qualified HNSW index name for this collection.
    fn index_name(&self) -> String {
        format!("{}_hnsw", self.collection)
    }

    /// Runtime guard: the embedder's output dimensionality MUST equal the configured
    /// `vector_dim` (the table column is `FLOAT[vector_dim]`). A mismatch means the
    /// chosen model does not match the config (e5-small=384, nomic=768, mxbai=1024).
    fn check_dim(&self, produced: usize) -> Result<()> {
        if produced as u64 != self.vector_dim {
            anyhow::bail!(
                "embedder produced {produced}-d vectors but vector_dim={} — set vector_dim to match the model (e5-small=384, nomic-embed-text=768, mxbai-embed-large=1024)",
                self.vector_dim
            );
        }
        Ok(())
    }

    /// If the collection table already exists, verify that the `embedding` column was
    /// declared with exactly the dimension in our Plan (`FLOAT[N]`). This catches the
    /// very common mistake of changing the embedding model (or vector_dim) without
    /// recreating the index. A mismatch produces confusing runtime errors much later
    /// (during INSERT of wrong-sized arrays or array_cosine_distance calls).
    fn validate_existing_collection_dim(&self) -> Result<()> {
        if !self.table_exists()? {
            return Ok(());
        }
        let type_str: Option<String> = self
            .conn
            .query_row(
                "SELECT data_type FROM information_schema.columns \
                 WHERE table_name = ? AND column_name = 'embedding'",
                [&self.collection],
                |r| r.get(0),
            )
            .optional()
            .context("failed to inspect embedding column type of existing table")?;

        match type_str {
            Some(t) => {
                let expected = format!("FLOAT[{}]", self.vector_dim);
                if t != expected {
                    let message = format!(
                        "DuckDB table '{}' has embedding column of type {} but config/vector_dim={} \
                         (expected {}). This usually means the embedding model was changed without \
                         --recreate. Delete the DuckDB file or re-index with --recreate.",
                        self.collection, t, self.vector_dim, expected
                    );
                    return Err(anyhow::Error::new(DimMismatch {
                        duckdb_path: self.path.to_string_lossy().into_owned(),
                        message,
                    }));
                }
                Ok(())
            }
            None => {
                // Table exists but no embedding column? Very strange (corrupt or manual tampering).
                // Let later operations fail with a clearer "no such column" if needed.
                Ok(())
            }
        }
    }

    /// Does the collection's table already exist?
    fn table_exists(&self) -> Result<bool> {
        let n: i64 = self
            .conn
            .query_row(
                "SELECT count(*) FROM information_schema.tables WHERE table_name = ?",
                [&self.collection],
                |r| r.get(0),
            )
            .context("failed to check table existence")?;
        Ok(n > 0)
    }

    /// The cosine HNSW index DDL. Single source of truth so the index created with
    /// the table and the one recreated by `end_bulk` can never drift apart.
    fn create_index_sql(&self) -> String {
        format!(
            "CREATE INDEX IF NOT EXISTS {idx} ON {coll} USING HNSW(embedding) WITH (metric='cosine');",
            idx = self.index_name(),
            coll = self.collection,
        )
    }

    /// `CREATE TABLE` + HNSW index for the collection.
    fn create_table_and_index(&self) -> Result<()> {
        let dim = self.vector_dim;
        self.conn
            .execute_batch(&format!(
                "CREATE TABLE IF NOT EXISTS {coll}(
                   id UBIGINT PRIMARY KEY,
                   path VARCHAR,
                   language VARCHAR,
                   start_line INTEGER,
                   end_line INTEGER,
                   text VARCHAR,
                   symbol VARCHAR,
                   embedding FLOAT[{dim}],
                   commit_sha VARCHAR,
                   dirty BOOLEAN DEFAULT false,
                   no_duplicate BOOLEAN DEFAULT false);
                 {index_sql}",
                coll = self.collection,
                index_sql = self.create_index_sql(),
            ))
            .context("failed to create table/HNSW index")
    }

    /// Create table (+index) if missing; `recreate` drops and recreates the table.
    #[tracing::instrument(skip(self), fields(collection = %self.collection))]
    pub async fn ensure_ready(&self, recreate: bool) -> Result<()> {
        if recreate && self.table_exists()? {
            self.conn
                .execute_batch(&format!("DROP TABLE IF EXISTS {};", self.collection))
                .context("failed to drop table for recreate")?;
            tracing::info!(collection = %self.collection, "dropped existing collection");
        }
        let existed = self.table_exists()?;
        self.create_table_and_index()?;
        if existed {
            // Best-effort additive migration for commit/dirty stamping and no_duplicate flag.
            let _ = self.conn.execute_batch(&format!(
                "ALTER TABLE {} ADD COLUMN commit_sha VARCHAR; ALTER TABLE {} ADD COLUMN dirty BOOLEAN DEFAULT false;",
                self.collection, self.collection
            ));
            let _ = self.conn.execute_batch(&format!(
                "ALTER TABLE {} ADD COLUMN no_duplicate BOOLEAN DEFAULT false;",
                self.collection
            ));
            if !recreate {
                tracing::info!(collection = %self.collection, "using existing collection");
            }
        } else {
            tracing::info!(
                collection = %self.collection,
                dims = self.vector_dim,
                path = %self.path.display(),
                "created collection (cosine HNSW)"
            );
        }
        Ok(())
    }

    /// Drop the HNSW index before bulk inserts (per-row HNSW maintenance is the
    /// expensive path — same reasoning as the project's WASM bulk gotcha).
    ///
    /// LOGICAL INVARIANT: Every write path that performs deletes followed by upserts
    /// (sync, MCP refresh via handle_refresh, etc.) MUST call begin_bulk before the
    /// first delete/upsert and end_bulk after the last one. Failure to do so leaves
    /// the experimental HNSW index in a degraded-recall state after deletes.
    pub async fn begin_bulk(&self) -> Result<()> {
        // sai-noduplicate: inverse of end_bulk; intentionally symmetric bookend
        self.conn
            .execute_batch(&format!("DROP INDEX IF EXISTS {};", self.index_name()))
            .context("failed to drop HNSW index for bulk insert")
    }

    /// Recreate the HNSW index after the bulk window.
    ///
    /// LOGICAL INVARIANT: See begin_bulk. This call is what restores full recall.
    pub async fn end_bulk(&self) -> Result<()> {
        // sai-noduplicate: inverse of begin_bulk; intentionally symmetric bookend
        self.conn
            .execute_batch(&self.create_index_sql())
            .context("failed to recreate HNSW index after bulk insert")
    }

    /// Embed chunk texts locally, then UPSERT rows (id is PRIMARY KEY → replace on conflict).
    pub async fn upsert(&self, chunks: &[CodeChunk]) -> Result<()> {
        if chunks.is_empty() {
            return Ok(());
        }
        let texts: Vec<String> = chunks.iter().map(|c| c.text.clone()).collect();
        let vectors = self.embedder.embed_passages(&texts).await?;
        if vectors.len() != chunks.len() {
            anyhow::bail!(
                "embedder returned {} vectors for {} chunks",
                vectors.len(),
                chunks.len()
            );
        }

        // One transaction per batch instead of one implicit commit per row. Without this
        // each INSERT auto-commits (a durability fsync per chunk), which dominates the
        // write cost during bulk indexing. A failure mid-batch rolls the whole batch back,
        // which is safe: callers retry the batch and ids are PRIMARY KEY (idempotent).
        self.conn
            .execute_batch("BEGIN TRANSACTION;")
            .context("failed to begin upsert transaction")?;

        // Run the inserts inside the transaction; on any failure roll back so the
        // connection isn't left with a dangling open transaction that poisons later calls.
        let result = (|| {
            for (c, vec) in chunks.iter().zip(vectors.iter()) {
                self.check_dim(vec.len())?;
                let literal = float_array_literal(vec);
                // `symbol` is nullable: the line chunker leaves it None (→ SQL NULL); the AST
                // chunker sets the captured symbol. Additive — the line path is unchanged.
                self.conn
                    .execute(
                        &format!(
                            "INSERT INTO {coll} (id, path, language, start_line, end_line, text, symbol, embedding, commit_sha, dirty, no_duplicate)
                             VALUES (?, ?, ?, ?, ?, ?, ?, {lit}::FLOAT[{dim}], ?, ?, ?)
                             ON CONFLICT (id) DO UPDATE SET
                               path = excluded.path,
                               language = excluded.language,
                               start_line = excluded.start_line,
                               end_line = excluded.end_line,
                               text = excluded.text,
                               symbol = excluded.symbol,
                               embedding = excluded.embedding,
                               commit_sha = excluded.commit_sha,
                               dirty = excluded.dirty,
                               no_duplicate = excluded.no_duplicate;",
                            coll = self.collection,
                            lit = literal,
                            dim = self.vector_dim,
                        ),
                        duckdb::params![
                            c.id,
                            c.path,
                            c.language,
                            c.start_line as i64,
                            c.end_line as i64,
                            c.text,
                            c.symbol,
                            c.commit_sha,
                            c.dirty,
                            c.no_duplicate,
                        ],
                    )
                    .with_context(|| format!("failed to upsert chunk id {}", c.id))?;
            }
            Ok::<(), anyhow::Error>(())
        })();

        match result {
            Ok(()) => {
                self.conn
                    .execute_batch("COMMIT;")
                    .context("failed to commit upsert transaction")?;
                Ok(())
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK;");
                Err(e)
            }
        }
    }

    /// Delete every stored chunk for a file path. Cheap: `DELETE` does not trigger
    /// an HNSW rebuild, so no index teardown is needed.
    pub async fn delete_by_path(&self, path: &str) -> Result<()> {
        self.conn
            .execute(
                &format!("DELETE FROM {} WHERE path = ?", self.collection),
                duckdb::params![path],
            )
            .with_context(|| format!("failed to delete points for {path}"))?;
        Ok(())
    }

    /// Embed the query and return the nearest rows by cosine similarity.
    /// `score = 1 - array_cosine_distance` (higher is better, matching Qdrant).
    pub async fn query(&self, q: &str, limit: u64) -> Result<Vec<Hit>> {
        let qvec = self.embedder.embed_query(q).await?;
        self.check_dim(qvec.len())?;
        let literal = float_array_literal(&qvec);
        let sql = format!(
            "SELECT id, path, language, start_line, end_line, text,
                    array_cosine_distance(embedding, {lit}::FLOAT[{dim}]) AS d,
                    commit_sha, dirty
             FROM {coll}
             ORDER BY d
             LIMIT ?",
            lit = literal,
            dim = self.vector_dim,
            coll = self.collection,
        );
        let mut stmt = self.conn.prepare(&sql).context("failed to prepare query")?;
        let rows = stmt
            .query_map(duckdb::params![limit], |row| {
                let id: u64 = row.get(0)?;
                let path: String = row.get(1)?;
                let language: String = row.get(2)?;
                let start_line: i32 = row.get(3)?;
                let end_line: i32 = row.get(4)?;
                let text: String = row.get(5)?;
                let distance: f32 = row.get(6)?;
                let commit_sha: Option<String> = row.get(7).ok();
                let dirty: Option<bool> = row.get(8).ok();
                Ok(Hit {
                    id,
                    path,
                    language,
                    start_line: start_line.max(0) as usize,
                    end_line: end_line.max(0) as usize,
                    text,
                    score: 1.0 - distance,
                    symbol: None,
                    commit_sha,
                    dirty: dirty.unwrap_or(false),
                    // query() is not used by the duplicates path — no need to read the column.
                    no_duplicate: false,
                })
            })
            .context("failed to run query")?;
        rows.map(|r| r.context("failed to read query row"))
            .collect()
    }

    /// Nearest-neighbour search by a RAW vector (no embedding). Over-fetches by 8x and
    /// dedups by id before truncating to `limit` — DuckDB's experimental HNSW can return
    /// the same id more than once, so this mirrors the dedup the indexer relies on.
    /// Optionally excludes one id (self-exclusion). `score = 1 - array_cosine_distance`.
    pub async fn query_by_vector(
        &self,
        vec: &[f32],
        limit: u64,
        exclude_id: Option<u64>,
    ) -> Result<Vec<Hit>> {
        self.check_dim(vec.len())?;
        let literal = float_array_literal(vec);
        // Over-fetch to survive HNSW duplicate candidates + the excluded self row.
        let fetch = limit.saturating_mul(OVERFETCH).max(limit);
        let where_clause = match exclude_id {
            Some(_) => "WHERE id != ?",
            None => "",
        };
        // Column layout (0-based):
        //   0=id, 1=path, 2=language, 3=start_line, 4=end_line, 5=text,
        //   6=symbol, 7=d, 8=commit_sha, 9=dirty, [10=no_duplicate if present]
        // no_duplicate is appended last only when the column exists (old indexes lack it).
        let has_nodup = self.has_no_duplicate_col();
        let nodup_col = if has_nodup { ", no_duplicate" } else { "" };
        let sql = format!(
            "SELECT id, path, language, start_line, end_line, text, symbol,
                    array_cosine_distance(embedding, {lit}::FLOAT[{dim}]) AS d,
                    commit_sha, dirty{nodup_col}
             FROM {coll}
             {where_clause}
             ORDER BY d
             LIMIT {fetch}",
            lit = literal,
            dim = self.vector_dim,
            coll = self.collection,
        );
        let mut stmt = self
            .conn
            .prepare(&sql)
            .context("failed to prepare query_by_vector")?;
        let map_row = move |row: &duckdb::Row<'_>| -> duckdb::Result<Hit> {
            let id: u64 = row.get(0)?;
            let path: String = row.get(1)?;
            let language: String = row.get(2)?;
            let start_line: i32 = row.get(3)?;
            let end_line: i32 = row.get(4)?;
            let text: String = row.get(5)?;
            let symbol: Option<String> = row.get(6)?;
            let distance: f32 = row.get(7)?;
            let commit_sha: Option<String> = row.get(8).ok();
            let dirty: Option<bool> = row.get(9).ok();
            // Index 10 is only present when has_nodup; otherwise default to false.
            let no_duplicate: bool = if has_nodup {
                row.get::<_, bool>(10).unwrap_or(false)
            } else {
                false
            };
            Ok(Hit {
                id,
                path,
                language,
                start_line: start_line.max(0) as usize,
                end_line: end_line.max(0) as usize,
                text,
                score: 1.0 - distance,
                symbol,
                commit_sha,
                dirty: dirty.unwrap_or(false),
                no_duplicate,
            })
        };
        let rows: Vec<Hit> = match exclude_id {
            Some(ex) => stmt
                .query_map(duckdb::params![ex], map_row)
                .context("failed to run query_by_vector")?
                .map(|r| r.context("failed to read query_by_vector row"))
                .collect::<Result<Vec<_>>>()?,
            None => stmt
                .query_map([], map_row)
                .context("failed to run query_by_vector")?
                .map(|r| r.context("failed to read query_by_vector row"))
                .collect::<Result<Vec<_>>>()?,
        };
        Ok(dedup_truncate(rows, limit as usize))
    }

    /// Fetch one stored chunk plus its vector by `path` + 1-based `start_line`.
    pub async fn get_by_location(
        &self,
        path: &str,
        line: usize,
    ) -> Result<Option<(Hit, Vec<f32>)>> {
        let sql = format!(
            "SELECT id, path, language, start_line, end_line, text, symbol, embedding
             FROM {coll}
             WHERE path = ? AND start_line = ?
             LIMIT 1",
            coll = self.collection,
        );
        let mut stmt = self
            .conn
            .prepare(&sql)
            .context("failed to prepare get_by_location")?;
        let mut rows = stmt
            .query_map(duckdb::params![path, line as i64], |row| {
                let id: u64 = row.get(0)?;
                let path: String = row.get(1)?;
                let language: String = row.get(2)?;
                let start_line: i32 = row.get(3)?;
                let end_line: i32 = row.get(4)?;
                let text: String = row.get(5)?;
                let symbol: Option<String> = row.get(6)?;
                let embedding = embedding_to_vec(row.get(7)?)?;
                Ok((
                    Hit {
                        id,
                        path,
                        language,
                        start_line: start_line.max(0) as usize,
                        end_line: end_line.max(0) as usize,
                        text,
                        score: 1.0,
                        symbol,
                        commit_sha: row.get(8).ok(),
                        dirty: row.get(9).ok().unwrap_or(false),
                        // get_by_location is not used by the duplicates path — no need to read.
                        no_duplicate: false,
                    },
                    embedding,
                ))
            })
            .context("failed to run get_by_location")?;
        match rows.next() {
            Some(r) => Ok(Some(r.context("failed to read get_by_location row")?)),
            None => Ok(None),
        }
    }

    /// Every stored chunk paired with its vector (for `find_duplicates`). The optional
    /// `path_glob` is applied in Rust (globset) to keep the SQL simple and portable.
    pub async fn all_chunks_with_vectors(
        &self,
        path_glob: Option<&str>,
    ) -> Result<Vec<(Hit, Vec<f32>)>> {
        let matcher = compile_glob(path_glob)?;
        // Column layout (0-based):
        //   0=id, 1=path, 2=language, 3=start_line, 4=end_line, 5=text,
        //   6=symbol, 7=embedding, [8=no_duplicate if present]
        // commit_sha/dirty are intentionally omitted from this SELECT; they were previously
        // read via out-of-bounds row.get(8/9).ok() → always None/false. That behaviour is
        // preserved: when has_nodup=true, no_duplicate lands at index 8 and the old OOB
        // reads for commit_sha/dirty would shift to 9/10 (still OOB → same None/false result).
        let has_nodup = self.has_no_duplicate_col();
        let nodup_col = if has_nodup { ", no_duplicate" } else { "" };
        let sql = format!(
            "SELECT id, path, language, start_line, end_line, text, symbol, embedding{nodup_col} FROM {coll}",
            coll = self.collection,
        );
        let mut stmt = self
            .conn
            .prepare(&sql)
            .context("failed to prepare all_chunks_with_vectors")?;
        let rows = stmt
            .query_map([], move |row| {
                let id: u64 = row.get(0)?;
                let path: String = row.get(1)?;
                let language: String = row.get(2)?;
                let start_line: i32 = row.get(3)?;
                let end_line: i32 = row.get(4)?;
                let text: String = row.get(5)?;
                let symbol: Option<String> = row.get(6)?;
                let embedding = embedding_to_vec(row.get(7)?)?;
                // Index 8 is only present when has_nodup; otherwise default to false.
                let no_duplicate: bool = if has_nodup {
                    row.get::<_, bool>(8).unwrap_or(false)
                } else {
                    false
                };
                Ok((
                    Hit {
                        id,
                        path,
                        language,
                        start_line: start_line.max(0) as usize,
                        end_line: end_line.max(0) as usize,
                        text,
                        score: 1.0,
                        symbol,
                        commit_sha: None,
                        dirty: false,
                        no_duplicate,
                    },
                    embedding,
                ))
            })
            .context("failed to run all_chunks_with_vectors")?;
        let mut out = Vec::new();
        for r in rows {
            let (hit, vec) = r.context("failed to read all_chunks_with_vectors row")?;
            if let Some(m) = &matcher
                && !m.is_match(&hit.path)
            {
                continue;
            }
            out.push((hit, vec));
        }
        Ok(out)
    }

    /// Total stored chunk count.
    #[cfg_attr(not(feature = "mcp"), allow(dead_code))]
    pub async fn chunk_count(&self) -> Result<u64> {
        let n: i64 = self
            .conn
            .query_row(
                &format!("SELECT count(*) FROM {}", self.collection),
                [],
                |r| r.get(0),
            )
            .context("failed to count chunks")?;
        Ok(n.max(0) as u64)
    }

    /// Cheap EXISTS for dirty rows (for duplicates warning).
    pub async fn has_dirty(&self) -> Result<bool> {
        // Column may not exist on old indexes; treat as no dirty.
        let sql = format!(
            "SELECT EXISTS(SELECT 1 FROM {} WHERE dirty LIMIT 1)",
            self.collection
        );
        match self.conn.query_row(&sql, [], |r| r.get::<_, bool>(0)) {
            Ok(v) => Ok(v),
            Err(_) => Ok(false),
        }
    }

    /// Whether the table has the `no_duplicate` column (old indexes opened read-only
    /// may lack it; a missing column fails at prepare time, not at runtime).
    fn has_no_duplicate_col(&self) -> bool {
        let sql = format!(
            "SELECT EXISTS(SELECT 1 FROM information_schema.columns \
             WHERE table_name = '{}' AND column_name = 'no_duplicate')",
            self.collection
        );
        matches!(
            self.conn.query_row(&sql, [], |r| r.get::<_, bool>(0)),
            Ok(true)
        )
    }

    /// Embed a query through the owned local embedder (asymmetric `query:` side).
    #[cfg_attr(not(feature = "mcp"), allow(dead_code))]
    pub async fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        let v = self.embedder.embed_query(text).await?;
        self.check_dim(v.len())?;
        Ok(v)
    }

    /// Embed code as a stored PASSAGE through the owned local embedder.
    pub async fn embed_passage(&self, text: &str) -> Result<Vec<f32>> {
        let mut v = self.embedder.embed_passages(&[text.to_string()]).await?;
        let v = v
            .pop()
            .context("embedder returned no vector for the passage")?;
        self.check_dim(v.len())?;
        Ok(v)
    }

    /// Drop the whole collection table (flush all vectors).
    pub async fn flush(&self) -> Result<()> {
        if self.table_exists()? {
            self.conn
                .execute_batch(&format!("DROP TABLE IF EXISTS {};", self.collection))
                .context("failed to drop table")?;
            tracing::info!(collection = %self.collection, "flushed: dropped table");
        } else {
            tracing::info!(collection = %self.collection, "nothing to flush: collection does not exist");
        }
        Ok(())
    }
}

/// Load the VSS extension. Tries the bundled/installed extension first, then the
/// community repository. Returns an actionable error if neither works (no network
/// / version mismatch).
///
/// Special handling for read-only connections (common MCP server case): we first
/// attempt a pure `LOAD` (which succeeds if VSS was previously installed by any
/// process). Only if that fails do we attempt INSTALL, which requires write access
/// to the extension directory.
fn load_vss(conn: &Connection) -> Result<()> {
    // Fast path: already installed somewhere on this machine (very common for
    // read-only MCP servers that were previously indexed with a writable run).
    if conn.execute_batch("LOAD vss;").is_ok() {
        return Ok(());
    }

    // Try the normal install sequence (may require network + write access to
    // DuckDB's extension cache directory).
    if conn.execute_batch("INSTALL vss; LOAD vss;").is_ok() {
        return Ok(());
    }

    // Last attempt: community repo (sometimes needed for certain DuckDB versions).
    conn.execute_batch("INSTALL vss FROM community; LOAD vss;")
        .context(
            "failed to load the DuckDB VSS extension (required for HNSW vector search and array_cosine_distance).\n\
             \n\
             Common causes & fixes:\n\
             • First-time setup: run the indexer at least once with write access so it can INSTALL vss.\n\
             • Read-only MCP server: pre-install VSS by running `duckdb -c \"INSTALL vss;\"` (or the full indexer) once as a user that can write to DuckDB's extension directory.\n\
             • Air-gapped / restricted env: copy the VSS extension into DuckDB's extension search path before starting the read-only server.\n\
             \n\
             See the DuckDB VSS extension docs for manual installation steps.",
        )
}

/// Convert a DuckDB `FLOAT[N]` column value into a `Vec<f32>`. The VSS array type comes
/// back as `Value::Array` (or `Value::List`) of `Value::Float`; anything else is an error.
fn embedding_to_vec(v: duckdb::types::Value) -> duckdb::Result<Vec<f32>> {
    use duckdb::types::Value;
    let items = match v {
        Value::Array(items) | Value::List(items) => items,
        other => {
            return Err(duckdb::Error::FromSqlConversionFailure(
                0,
                duckdb::types::Type::Any,
                format!("expected FLOAT[] embedding, got {other:?}").into(),
            ));
        }
    };
    let mut out = Vec::with_capacity(items.len());
    for it in items {
        match it {
            Value::Float(f) => out.push(f),
            Value::Double(d) => out.push(d as f32),
            other => {
                return Err(duckdb::Error::FromSqlConversionFailure(
                    0,
                    duckdb::types::Type::Any,
                    format!("expected FLOAT element, got {other:?}").into(),
                ));
            }
        }
    }
    Ok(out)
}

/// Dedup hits by id (keeping first/best, since rows arrive sorted by distance) and
/// truncate to `limit`. Mirrors the dedup the line-search path relies on for HNSW.
fn dedup_truncate(rows: Vec<Hit>, limit: usize) -> Vec<Hit> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(limit.min(rows.len()));
    for h in rows {
        if seen.insert(h.id) {
            out.push(h);
            if out.len() >= limit {
                break;
            }
        }
    }
    out
}

/// Compile an optional path glob into a matcher, erroring on a bad pattern.
fn compile_glob(pattern: Option<&str>) -> Result<Option<globset::GlobMatcher>> {
    // sai-noduplicate: glob-compile twin of mcp::compile_glob_opt; different error domain (anyhow vs McpError)
    match pattern {
        None => Ok(None),
        Some(p) => {
            let g = globset::Glob::new(p)
                .with_context(|| format!("invalid path_glob: {p}"))?
                .compile_matcher();
            Ok(Some(g))
        }
    }
}

/// Render a `f32` slice as a DuckDB list literal `[v1, v2, ...]` (cast to
/// `FLOAT[N]` at the call site). Avoids intermediate copies — writes directly.
fn float_array_literal(v: &[f32]) -> String {
    let mut s = String::with_capacity(v.len() * 12 + 2);
    s.push('[');
    for (i, x) in v.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        // Plain Display is fine for f32; DuckDB parses it as a FLOAT literal.
        let _ = write!(s, "{x}");
    }
    s.push(']');
    s
}

/// Tests that don't require a live embedder: column-guard and no_duplicate round-trip
/// via raw SQL inserts. Gated on `ollama` too: the embedder is constructed via
/// `embedder::ollama_embedder`, which only exists under that feature.
#[cfg(all(test, feature = "duckdb", feature = "ollama"))]
mod no_duplicate_tests {
    use super::*;
    use crate::config::Plan;
    use crate::vectordbs::embedder;
    use tempfile::TempDir;

    fn make_plan(db_path: &std::path::Path, dim: u64) -> Plan {
        // Start from the shared DuckDB/Ollama base and override only the fields
        // specific to the no-duplicate tests (Rust sources, nomic model, symmetric
        // prefix, dedicated collection).
        Plan {
            ext: vec!["rs".to_string()],
            prefix_style: crate::vectordbs::PrefixStyle::None,
            collection: "test_nodup".to_string(),
            model: "nomic-embed-text".to_string(),
            model_repo: "nomic".to_string(),
            ..crate::config::test_support::duckdb_plan(db_path, dim)
        }
    }

    fn make_ollama_embedder(plan: &Plan) -> Embedder {
        Embedder::Ollama(
            embedder::ollama_embedder(plan).expect("ollama embedder construction must succeed"),
        )
    }

    /// Block on a future using a throwaway current-thread runtime (these tests run
    /// outside any async context).
    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(f)
    }

    /// `CREATE TABLE` for the legacy schema that predates the `no_duplicate` column —
    /// used by the "old table" guard tests.
    const OLD_SCHEMA_SQL: &str = "CREATE TABLE test_nodup(
                   id UBIGINT PRIMARY KEY,
                   path VARCHAR,
                   language VARCHAR,
                   start_line INTEGER,
                   end_line INTEGER,
                   text VARCHAR,
                   symbol VARCHAR,
                   embedding FLOAT[4],
                   commit_sha VARCHAR,
                   dirty BOOLEAN DEFAULT false
                 );";

    /// has_no_duplicate_col returns false when the column is absent.
    #[test]
    fn has_no_duplicate_col_absent() {
        // sai-noduplicate: paired column-guard test (absent case), mirror of _present
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("nodup_absent.duckdb");

        // Create table WITHOUT no_duplicate column.
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(OLD_SCHEMA_SQL).unwrap();
        }

        let plan = make_plan(&db_path, 4);
        let emb = make_ollama_embedder(&plan);
        let backend = DuckDbBackend::connect(&plan, emb).unwrap();
        assert!(
            !backend.has_no_duplicate_col(),
            "column absent: has_no_duplicate_col must return false"
        );
    }

    /// has_no_duplicate_col returns true after ensure_ready creates the table with the column.
    #[test]
    fn has_no_duplicate_col_present() {
        // sai-noduplicate: paired column-guard test (present case), mirror of _absent
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("nodup_present.duckdb");

        let plan = make_plan(&db_path, 4);
        let emb = make_ollama_embedder(&plan);
        let backend = DuckDbBackend::connect(&plan, emb).unwrap();

        // ensure_ready creates the table with no_duplicate column.
        block_on(backend.ensure_ready(false)).unwrap();

        assert!(
            backend.has_no_duplicate_col(),
            "column present after ensure_ready: has_no_duplicate_col must return true"
        );
    }

    /// all_chunks_with_vectors defaults no_duplicate to false on a table lacking the column.
    #[test]
    fn all_chunks_defaults_no_duplicate_false_on_old_table() {
        // sai-noduplicate: paired all_chunks test (old-table case)
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("nodup_old.duckdb");

        // Seed: table WITHOUT no_duplicate, one row inserted.
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch("INSTALL vss; LOAD vss;").ok();
            conn.execute_batch("SET hnsw_enable_experimental_persistence = true;")
                .ok();
            conn.execute_batch(OLD_SCHEMA_SQL).unwrap();
            conn.execute_batch(
                "INSERT INTO test_nodup VALUES (1, 'a.rs', 'rust', 1, 5, 'fn foo() {}', NULL,
                   [0.1, 0.2, 0.3, 0.4]::FLOAT[4], NULL, false);",
            )
            .unwrap();
        }

        let plan = make_plan(&db_path, 4);
        let emb = make_ollama_embedder(&plan);
        let backend = DuckDbBackend::connect(&plan, emb).unwrap();

        let results = block_on(backend.all_chunks_with_vectors(None)).unwrap();

        assert_eq!(results.len(), 1, "should return the one seeded row");
        assert!(
            !results[0].0.no_duplicate,
            "no_duplicate must default to false when column is absent"
        );
    }

    /// all_chunks_with_vectors reads the real no_duplicate value from a table that has the column.
    #[test]
    fn all_chunks_reads_no_duplicate_true() {
        // sai-noduplicate: paired all_chunks test (new-table case)
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("nodup_new.duckdb");

        // Seed: table WITH no_duplicate, one row with no_duplicate=true.
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch("INSTALL vss; LOAD vss;").ok();
            conn.execute_batch("SET hnsw_enable_experimental_persistence = true;")
                .ok();
            conn.execute_batch(
                "CREATE TABLE test_nodup(
                   id UBIGINT PRIMARY KEY,
                   path VARCHAR,
                   language VARCHAR,
                   start_line INTEGER,
                   end_line INTEGER,
                   text VARCHAR,
                   symbol VARCHAR,
                   embedding FLOAT[4],
                   commit_sha VARCHAR,
                   dirty BOOLEAN DEFAULT false,
                   no_duplicate BOOLEAN DEFAULT false
                 );
                 INSERT INTO test_nodup VALUES (42, 'b.rs', 'rust', 10, 20, 'fn bar() {}', NULL,
                   [0.5, 0.6, 0.7, 0.8]::FLOAT[4], NULL, false, true);",
            )
            .unwrap();
        }

        let plan = make_plan(&db_path, 4);
        let emb = make_ollama_embedder(&plan);
        let backend = DuckDbBackend::connect(&plan, emb).unwrap();

        let results = block_on(backend.all_chunks_with_vectors(None)).unwrap();

        assert_eq!(results.len(), 1, "should return the one seeded row");
        assert!(
            results[0].0.no_duplicate,
            "no_duplicate must be true when stored as true"
        );
    }
}

#[cfg(all(test, feature = "duckdb", any(feature = "ort", feature = "ollama")))]
mod validation_tests {
    use super::*;
    use crate::config::Plan;
    use crate::vectordbs::embedder;
    use std::path::PathBuf;
    use tempfile::TempDir;

    /// Build a minimal Plan pointing at a temp DuckDB path with a chosen vector_dim.
    fn test_plan(duckdb_path: &std::path::Path, dim: u64) -> Plan {
        // We only need the fields that DuckDbBackend::connect reads for validation.
        // Using a dummy embedder (ollama is cheapest to construct). The shared
        // `duckdb_plan` builder already carries these E5 validation defaults.
        crate::config::test_support::duckdb_plan(duckdb_path, dim)
    }

    fn make_ollama_embedder() -> Embedder {
        // Construction only — we never call embed in these validation tests.
        // We use a throwaway plan just for construction (the actual Plan is passed to connect).
        let dummy_plan = test_plan(&PathBuf::from("/tmp/dummy"), 384);
        Embedder::Ollama(
            embedder::ollama_embedder(&dummy_plan)
                .expect("ollama embedder construction should succeed for test"),
        )
    }

    #[test]
    fn duckdb_rejects_mismatched_dim_on_existing_table() {
        // sai-noduplicate: paired dim-validation test (reject case)
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("mismatch.duckdb");

        // Manually create a table with 384-d embeddings (as if previously indexed with e5-small).
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS test_validation(
                   id UBIGINT PRIMARY KEY,
                   embedding FLOAT[384]
                 );",
            )
            .unwrap();
        }

        // Now try to open with vector_dim=768 (as if user switched to a larger model without recreate).
        let plan = test_plan(&db_path, 768);
        let embedder = make_ollama_embedder();

        let err = match DuckDbBackend::connect(&plan, embedder) {
            Err(e) => e,
            Ok(_) => panic!("expected dimension mismatch error but connect succeeded"),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("FLOAT[384]") && msg.contains("vector_dim=768"),
            "error should clearly mention the actual vs expected dim: {msg}"
        );
        assert!(
            msg.contains("--recreate") || msg.contains("recreate"),
            "error should suggest --recreate"
        );
    }

    #[test]
    fn duckdb_accepts_matching_dim_on_existing_table() {
        // sai-noduplicate: paired dim-validation test (accept case)
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("match.duckdb");

        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS test_validation(
                   id UBIGINT PRIMARY KEY,
                   embedding FLOAT[384]
                 );",
            )
            .unwrap();
        }

        let plan = test_plan(&db_path, 384);
        let embedder = make_ollama_embedder();

        // Should open cleanly.
        let _backend = DuckDbBackend::connect(&plan, embedder).expect("matching dim must succeed");
    }

    #[test]
    fn duckdb_validation_skips_when_table_does_not_exist_yet() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("new.duckdb");

        // No table created yet.
        let plan = test_plan(&db_path, 1024);
        let embedder = make_ollama_embedder();

        // Should succeed (the table will be created later by ensure_ready).
        let _backend = DuckDbBackend::connect(&plan, embedder)
            .expect("missing table should not trigger dim validation");
    }
}
