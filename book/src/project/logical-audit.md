# Internals: logical audit

This page is a maintainer-facing enumeration of the **logical invariants** that the core
of SAI relies on for correctness. These are properties that the type system does *not*
enforce on its own — they are upheld by convention, by call-site discipline, and by the
tests that pin them. **Re-read this page before changing core indexing, clustering, or
backend logic.** Breaking one of these invariants typically produces silently wrong
results (duplicated points, degraded recall, stale data) rather than a compile error or a
crash.

Each invariant below is stated with **what** must hold, **why** it matters, and **where**
the relevant code lives. For the narrative walkthrough of the same pipelines, see
[How it works](../concepts/how-it-works.md); terms in bold are defined in the
[glossary](../concepts/glossary.md).

## Index of invariants

| # | Invariant | Primary file |
| --- | --- | --- |
| 1 | Chunking "nothing dropped" / single source of truth | `src/indexer.rs` |
| 2 | DuckDB HNSW bulk contract | `src/vectordbs/duckdb.rs`, `src/worker.rs` |
| 3 | Cross-backend deterministic point IDs | `src/indexer.rs` |
| 4 | Prefix consistency | `src/vectordbs/mod.rs` |
| 5 | Worker-thread isolation of the `!Send` backend | `src/worker.rs` |
| 6 | Dimension guards | `src/vectordbs/duckdb.rs` |
| 7 | Additive migration | `src/vectordbs/duckdb.rs` |
| 8 | `no_duplicate` semantics | `src/search.rs`, `src/indexer.rs` |

---

## 1. Chunking "nothing dropped" — one decision path, no drift

**What must hold.** The decision "which files and which line ranges get embedded?" has
exactly **one** implementation. The function `load_file_for_indexing` is the single source
of truth for the per-file gate (wanted extension → `passes_globs` → generated-marker skip
→ readable UTF-8), and `build_chunks` is the single source of truth for splitting a file
into chunks and honouring opt-out markers. Every consumer must go through these, so the
*real* indexing path, the *dry-run preview*, and the *incremental re-index* can never
disagree about what would be indexed.

**Why.** Three call sites need the same answer:

- `collect_chunks` — the full CLI `sync` walk (pure; touches no backend).
- `dry_run` — the `--dry-run` preview that reports include/exclude counts.
- `reindex_file` — the shared per-file bridge used by **both** CLI `sync` and the MCP
  `sai_refresh` tool, and the only function in the module that calls into a `Backend`.

If any of these reimplemented the gate, a file could be previewed as "would index" but then
skipped at runtime, or `sai_refresh` could re-add a file that `sync` would have excluded.
`dry_run` is explicit about this: it calls `plan.passes_globs` (the shared decision) for the
actual include/exclude gate and uses the separate manual checks *only* to produce a
human-friendly "why" reason — never to make the decision.

**Where.** `src/indexer.rs`:

- `load_file_for_indexing` — the documented "single source of truth for the decision
  'should we attempt to produce chunks from this file?'".
- `build_chunks` → `chunk_content` → `scan_markers` — chunking and marker handling; the AST
  and line chunkers both route oversize/remainder splitting through the shared
  `chunk_line_range`, so window sizing and overlap can never drift between the two.
- `reindex_file` — deletes the file's existing points first, then re-applies
  `load_file_for_indexing` + `build_chunks`. Its doc comment states the contract: this logic
  "must stay in sync with `collect_chunks` and `dry_run`", which is achieved structurally by
  all three calling the same functions rather than by parallel reimplementations.

> The comment stripper carries its own sub-invariant: after stripping C-family comments the
> **line count is identical** to the input, so a chunk's `start_line`/`end_line` always index
> the real source lines. Marker scanning therefore runs on the **raw** (pre-strip) lines.

---

## 2. DuckDB HNSW bulk contract — bracket every delete+upsert window

**What must hold.** Every write path that performs deletes followed by upserts MUST call
`begin_bulk()` **before** the first delete/upsert and `end_bulk()` **after** the last one.
`begin_bulk` drops the HNSW index; `end_bulk` recreates it. Three corollaries:

1. **`end_bulk` runs even on error.** The index must never be left dropped after an
   operation, regardless of whether an individual path failed mid-batch.
2. **`DELETE` alone does not trigger an HNSW rebuild**, so `delete_by_path` needs no index
   teardown of its own — it is only the *combination* of mass deletes + upserts inside a
   bulk window that requires the bracket.
3. **`create_index_sql` is the single source** for both the table-create index DDL and the
   `end_bulk` rebuild, so the index created with the table and the one recreated after a
   bulk window can never drift apart.

**Why.** Per-row HNSW maintenance is the expensive path (the project's documented "WASM bulk
gotcha"): inserting into an HNSW-indexed table maintains the graph per row. Dropping the
index, bulk-inserting, then recreating it is far cheaper. The experimental persistent HNSW
index is also left in a **degraded-recall** state if deletes happen without a subsequent
rebuild — so skipping the bracket does not error loudly, it silently lowers search quality.

**Where.**

- `src/vectordbs/duckdb.rs`: `begin_bulk` (drops the index), `end_bulk` (recreates via
  `create_index_sql`), `create_index_sql` (the single index-DDL source, used by both
  `create_table_and_index` and `end_bulk`), and `delete_by_path` (documented as cheap — no
  index teardown). The invariant is spelled out in the `begin_bulk` doc comment.
- `src/worker.rs`: `handle_refresh` is the MCP `sai_refresh` implementation and the
  reference example of the contract — it calls `begin_bulk`, loops `reindex_file` per path,
  and then `end_bulk`. Its structure guarantees corollary (1): it stores the first error,
  **always** awaits `end_bulk`, and only then returns the stored error.

```rust
// src/worker.rs — handle_refresh (shape)
backend.begin_bulk().await?;
// per-path: reindex_file(...); on Err, record first_err and break
let end = backend.end_bulk().await;   // always rebuild, even on error
if let Some(e) = first_err { return Err(e); }
end?;
```

> Note: the per-batch `upsert` additionally wraps its inserts in a `BEGIN TRANSACTION` /
> `COMMIT` (rolling back on failure) so a mid-batch failure cannot leave a dangling open
> transaction. That transaction is a separate concern from the HNSW bulk bracket above.

---

## 3. Cross-backend deterministic point IDs

**What must hold.** A chunk's storage ID is `XxHash64` (seed `0`) over `path` then
`start_line`, and **nothing else**. The same `point_id` function is used by every backend,
so Qdrant and DuckDB key the same chunk identically.

**Why.** Because the ID is a pure function of `path + start_line`, re-running the indexer
**updates points in place** rather than duplicating them — the DuckDB `id` is the table's
`PRIMARY KEY`, so an upsert with a colliding ID does `ON CONFLICT (id) DO UPDATE`. A fixed
seed makes the hash **stable across builds and machines**; `DefaultHasher` would not be (its
algorithm is unspecified), which would scatter IDs and re-duplicate everything. Changing the
algorithm, the seed, or the hashed fields is a breaking change that requires a one-time
`flush` / `--recreate`.

**Where.** `src/indexer.rs`: `point_id`, called from `build_chunks` when constructing each
`CodeChunk { id: point_id(rel, c.start_line), ... }`. The test
`point_id_is_stable_and_deterministic` pins the exact value
(`point_id("src/foo.ts", 1) == 10293058119199652890`) — if it ever fails, the ID algorithm
changed and a re-index is needed.

---

## 4. Prefix consistency — one `PrefixStyle`, both code paths

**What must hold.** Exactly **one** `PrefixStyle` is resolved (in `build_plan`: explicit
config wins, else auto-detected from the model name) and is applied identically by **both**
the local DuckDB embedders **and** the Qdrant `Document` path, through the shared
`format_passage` / `format_query` helpers.

**Why.** Asymmetric embedding models require the *stored passage* and the *query* to be
prefixed consistently (E5 uses `passage: ` / `query: `; Qwen wraps only the query with a
task instruction; `None` adds nothing). If the storage path and the query path disagreed —
or if one backend hard-coded a prefix the other did not — every search would be comparing
vectors from two different input distributions, silently wrecking recall. Routing both sides
through one pair of formatter functions makes the policy impossible to apply inconsistently.

**Where.** `src/vectordbs/mod.rs`: the `PrefixStyle` enum (`E5` / `Qwen` / `None`) with
`PrefixStyle::parse` (explicit config) and `PrefixStyle::detect` (model-name heuristic:
contains `e5` → E5, contains `qwen` → Qwen, else None), and the single formatter pair
`format_passage` / `format_query`. The module doc on `PrefixStyle` states it is "applied by
BOTH embedders and the Qdrant `Document` path through the shared
`format_passage`/`format_query` helpers".

---

## 5. Worker-thread isolation of the `!Send` backend

**What must hold.** The `!Send` resources — the DuckDB `Connection` and (for the `ort`
embedder) the ONNX `Session` — live on **one dedicated OS thread** that owns them, driven by
that thread's own **current-thread** Tokio runtime. Requests are served **one at a time**.
The MCP server holds only a `Send + Sync` handle (a wrapper around an `mpsc::Sender`); the
`!Send` backend never crosses a thread boundary.

**Why.** `rmcp` requires every `#[tool]` handler future to be `Send`. A `duckdb::Connection`
held across an `.await` inside a handler makes that future non-`Send`, and the crate fails to
compile under the `mcp` feature. The actor pattern moves the backend off the async handler
path: handler futures capture only the channel handle (`Send`), so they compile, while the
backend's own async methods (e.g. the Ollama embedder's `reqwest` calls) run locally on the
worker's current-thread runtime. Serving one request at a time guarantees the single,
non-`Sync` DuckDB connection is never touched concurrently — there is no lock to forget,
because there is only ever one in-flight operation.

**Where.** `src/worker.rs`:

- `spawn` builds the named OS thread (`semanticastindexer-backend`) and a
  `new_current_thread` runtime, moving `backend` + `plan` onto it.
- `worker_loop` is the strictly sequential consumer — `while let Some(req) = rx.recv()` —
  fully handling each request (including its `.await`s) before taking the next.
- `BackendHandle` (`#[derive(Clone)]`, wrapping `mpsc::Sender<Request>`) is the `Send + Sync`
  handle the MCP server holds; each method builds a `Request` + a `oneshot` reply and awaits
  it. The request channel is bounded (buffer 32). Dropping every `BackendHandle` clone closes
  the channel and ends the worker loop and thread.

This is backend-agnostic by design: Qdrant's backend is already `Send`, but every backend is
routed through the same worker so the handler code is identical.

---

## 6. Dimension guards — never store or compare a wrong-sized vector

**What must hold.** The embedding dimension is checked at three layers:

1. **`check_dim` on every embed / upsert / query.** Each produced or queried vector's length
   must equal the configured `vector_dim`; the storage column is declared `FLOAT[vector_dim]`.
2. **`validate_existing_collection_dim` on open.** When the table already exists, the
   `embedding` column's declared type must be exactly `FLOAT[vector_dim]`. A mismatch returns
   a **typed** `DimMismatch` error that carries the DuckDB file path.
3. **The column itself** is `embedding FLOAT[dim]`, so the database enforces array width on
   every insert as a backstop.

**Why.** Switching embedding models without `--recreate` is the classic mistake (e5-small =
384, nomic-embed-text = 768, mxbai-embed-large = 1024). Without these guards the failure
surfaces far away from the cause — a confusing error deep inside an `INSERT` of a wrong-sized
array or an `array_cosine_distance` call. Catching it at open time, with the actual vs.
expected dimension in the message, makes the fix obvious. The error is **typed** rather than a
string so the CLI can recognise it via `dim_mismatch_duckdb_path` and offer an interactive
"delete & re-index?" using the carried path — **without string-matching the message** (which
would be brittle). `DimMismatch`'s `Display` still preserves the original human guidance.

**Where.** `src/vectordbs/duckdb.rs`:

- `check_dim` — called in `upsert` (per produced vector), `query`, `query_by_vector`,
  `embed_query`, and `embed_passage`.
- `validate_existing_collection_dim` — called at the end of both `connect` and
  `connect_readonly`; constructs `DimMismatch { duckdb_path, message }` on mismatch.
- `DimMismatch` (the typed error carrying `duckdb_path`) and the `FLOAT[{dim}]` column in
  `create_table_and_index`.
- `src/vectordbs/mod.rs`: `dim_mismatch_duckdb_path` downcasts the error to `DimMismatch` and
  returns the path for the interactive recovery flow.

Tests `duckdb_rejects_mismatched_dim_on_existing_table` and
`duckdb_accepts_matching_dim_on_existing_table` pin this behaviour.

---

## 7. Additive migration — old indexes keep working

**What must hold.** New columns (`commit_sha`, `dirty`, `no_duplicate`) are added to an
existing table via **best-effort** `ALTER TABLE ... ADD COLUMN` in `ensure_ready`, and reads
that depend on `no_duplicate` **conditionally include it** based on whether the column
actually exists. An index created by an older binary (which lacks these columns) must still
open and serve search and duplicate scans.

**Why.** Users index once and then upgrade SAI; forcing a full re-index on every schema
addition would be hostile. The `ALTER` statements are wrapped so a failure (e.g. the column
already exists) is ignored rather than fatal. Reads must not assume the new column: a `SELECT`
naming `no_duplicate` against a table that lacks it fails at *prepare* time, not at runtime —
so the read paths first probe `has_no_duplicate_col` and only then add the column to the
projection, defaulting to `false` when absent.

**Where.** `src/vectordbs/duckdb.rs`:

- `ensure_ready` — when the table already existed, runs best-effort
  `ALTER TABLE ... ADD COLUMN commit_sha` / `dirty` and a separate
  `ALTER TABLE ... ADD COLUMN no_duplicate` (each discarded with `let _ = ...`).
- `has_no_duplicate_col` — `information_schema.columns` probe.
- `all_chunks_with_vectors` and `query_by_vector` — build `nodup_col` (`", no_duplicate"` or
  `""`) from that probe and read index `8` / `10` only when present, otherwise default to
  `false`.

The `no_duplicate` round-trip is pinned by `has_no_duplicate_col_absent` /
`has_no_duplicate_col_present`, `all_chunks_defaults_no_duplicate_false_on_old_table`, and
`all_chunks_reads_no_duplicate_true`.

---

## 8. `no_duplicate` semantics — excluded as seed *and* neighbour

**What must hold.** A chunk carrying the `sai-noduplicate` marker is **indexed and
searchable** (it appears in `sai_search_code` / `sai_find_similar` results), but it is
excluded from near-duplicate clustering as **both**:

- a **seed** — a `no_duplicate` chunk forms no edges from its own neighbour list, and
- a **neighbour** — it is skipped when it appears as another chunk's neighbour.

**Why.** Some near-identical code is intentional (mirrored bookend functions, paired tests,
deliberate twins) and would otherwise dominate every duplicates run. The flag opts a chunk
out of clustering without removing it from the index, so search still finds it. Excluding it
on only one side would be insufficient: if a flagged chunk could still be pulled in as a
neighbour of an unflagged chunk, it would re-enter the cluster through the back door. The
exclusion must be symmetric.

**Where.**

- `src/search.rs`: `cluster_duplicates` — `if chunks[i].0.no_duplicate { continue; }` skips a
  flagged chunk as a seed, and `if chunks[j].0.no_duplicate { continue; }` skips it as a
  neighbour, before any edge is formed or `union` is called. The test
  `cluster_duplicates_excludes_no_duplicate_chunks` pins both directions.
- `src/indexer.rs`: `scan_markers` detects the `sai-noduplicate` substring (case-insensitive,
  on the raw line span) and `build_chunks` stamps `no_duplicate` onto the `CodeChunk`. The
  flag is then carried through storage and read back into `Hit.no_duplicate`
  (`src/vectordbs/mod.rs`), which is what `cluster_duplicates` inspects.

See [opt-out markers](../guides/opt-out-markers.md) for the user-facing description of the
marker, and [search and duplicates](../guides/search-and-duplicates.md) for the duplicates
workflow.

---

## When you change core logic

Before merging changes to indexing, clustering, or backend code, confirm each touched
invariant still holds and that its pinning test still passes:

- Chunking decision functions stay shared (1) — re-run the chunking and marker tests in
  `src/indexer.rs`.
- Every new delete+upsert path brackets `begin_bulk` / `end_bulk`, and `end_bulk` runs on
  error (2).
- `point_id` inputs, seed, and algorithm are unchanged (3) — the pinned-value test must pass.
- New embedders/backends route prefixing through `format_passage` / `format_query` (4).
- The `!Send` backend stays on the worker thread; handlers capture only the handle (5).
- New embed/upsert/query paths call `check_dim`; schema changes keep `DimMismatch` typed (6).
- Schema additions are best-effort `ALTER ADD COLUMN` and reads stay column-conditional (7).
- `no_duplicate` exclusion stays symmetric across seed and neighbour (8).
