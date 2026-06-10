# Documentation Plan — semanticastindexer (SAI)

Status: proposal for review. Grounded in a full source inventory (CLI, config, MCP, internals)
and a gap analysis of the existing docs.

## 1. Goals & audiences

A "proper" doc set serves three audiences; today's docs serve the second one well and the
other two barely:

| Audience | Need | Current state |
| --- | --- | --- |
| **New user / evaluator** | What is this, is it for me, get it running in 5 min | Thin: only inline Quickstarts, no tutorial, no concepts page |
| **Operator / power user** | Reference for every flag, key, tool; how to tune, sync, run in CI | Strong reference, but scattered; missing CI/CD, perf, troubleshooting hub |
| **Agent integrator** | MCP tools, exact I/O schemas, per-client wiring | Partial: 5 of 6 tools listed, no output schemas, no per-client walkthroughs |
| **Contributor** | Build/test, module map, invariants, PR flow | Essentially absent (one line about the setup script) |

Principles: one linear happy-path for newcomers; one authoritative table per reference surface
(no reconstructing facts across three pages); every error has a landing page; every claim
matches the code (the inventory found several stale statements — see §6).

## 2. What exists today

`README.md` + seven `docs/*.md` (architecture, backends-and-embedders, chunking, configuration,
cli, mcp-server, install) + `mcp-setup/{README,SKILL}.md` + `CHANGELOG.md`. The reference pages
are dense and mostly accurate. The landing page (`docs/index.html`, GitHub Pages) is the public
front door and links to the `docs/*.md` on GitHub.

## 3. Proposed information architecture

Tooling-agnostic (the page set is the same whether rendered as flat markdown or a docs site —
see §5). New pages marked **[NEW]**; expansions of existing pages marked **[exp]**.

```
Get started
  introduction.md            what/why/when, the mental model (index once → ask in NL)   [NEW]
  getting-started.md         install → index → query → duplicates, offline default,     [NEW]
                             expected output, first-run model download
  installation.md            (= install.md, per-OS, connect-agent, uninstall)           [exp]

Concepts
  how-it-works.md            (= architecture.md) pipeline + query path + worker model    [exp]
  glossary.md                embedding, vector_dim, cosine, HNSW/VSS, point ID, chunk,   [NEW]
                             symbol, collection, prefix (E5/Qwen), union-find, server-side inference

Guides (task-oriented)
  indexing.md                root/ext/dry-run/recreate, what is filtered, selection order [exp]
  search-and-duplicates.md   search_code vs find_similar vs find_duplicates, by example   [exp]
  keeping-in-sync.md         git-hook recipes: pre-commit/pre-push/post-merge,            [NEW]
                             husky/lefthook, backgrounding, repo-root caveat
  ci-cd.md                   GitHub Actions / GitLab: index-on-PR, HF cache in CI,        [NEW]
                             Qdrant secrets, "fail build on new duplicates"
  choosing-a-model.md        offline ort vs ollama, e5-small vs jina vs ollama models,    [NEW]
                             vector_dim matching, prefix styles
  opt-out-markers.md         sai-noindexing / sai-noduplicate, granularity, honor_* flags [exp]
  tuning-similarity.md       per-model thresholds, reading raw scores (min_score 0)       [NEW]

Reference
  cli.md                     every subcommand + every flag (incl. subcommand-local),      [exp]
                             defaults, exit/timing behavior, the two --limit defaults
  configuration.md           ONE table: every indexer.yaml key + type + default +         [exp]
                             resolution + "config-only vs has-CLI-flag"
  mcp-server.md              all 6 sai_ tools, --allow-write AND --allow-setup, gating     [exp]
  output-schemas.md          JSON response shape per tool + CLI text output formats        [NEW]
  backends-and-embedders.md  (existing, strongest page)                                    [keep]
  chunking.md                (existing; fix "AST = ts/tsx" → ts/tsx/rs/go)                 [exp]
  environment.md             QDRANT_URL/API_KEY; note: no OLLAMA_HOST/HF_* env support     [NEW]

Integrations
  mcp-clients/               per-client: paste → restart → verify sai_ tools → test query  [NEW]
    claude-code, claude-desktop, cursor, windsurf, continue, codex
  qdrant-cloud.md            inference enabled, model, vector size 384, gRPC :6334         [exp]
  ollama.md                  serve, pull, url/model, vector_dim matching                   [NEW]

Operations
  troubleshooting.md         FAQ hub (HIGHEST priority) — see §4                           [NEW]
  performance.md             index size, ort threads/batch, HNSW build cost, large repos  [NEW]
  security.md                creds, read-only default, --allow-write/--allow-setup threat  [NEW]
                             model, data-leaves-machine matrix per backend

Project
  contributing.md            build/test, feature matrix, module map, run MCP locally       [NEW]
  internals/LOGICAL_AUDIT.md the named invariants (fixes architecture.md's dangling link)  [NEW]
  changelog.md (link)        link CHANGELOG.md; index-format compat; XxHash re-index note  [exp]
```

## 4. Highest-value new content (gap-driven, prioritized)

1. **`troubleshooting.md` (FAQ hub) — #1.** Collect the first-run failures that are currently
   buried as asides in `backends-and-embedders.md`: HF model download (slow/offline, where the
   cache lives, `HF_HUB_OFFLINE`); the `hf-hub` Xet `tokenizer.json` staging workaround;
   dimension mismatch (`embedder produced 768-d but vector_dim=384`) + the `--recreate` / delete
   `.index/code.duckdb` fix; "Document API only works on Qdrant Cloud"; `rebuild with --features …`
   feature-gate errors; empty/zero results and DuckDB recall after deletes; "MCP server not
   showing up in my client."
2. **`output-schemas.md`.** Every MCP tool emits structured JSON; nothing documents the shapes.
   From the inventory: `sai_search_code → {hits:[{path,start_line,end_line,symbol|null,score,snippet}]}`,
   `sai_find_duplicates → {clusters:[{size,members:[{path,start_line,end_line,symbol|null}],min_sim,max_sim}]}`,
   `sai_index_status → {backend,collection,model,vector_dim,chunk_count,chunker}`,
   `sai_refresh → {refreshed:[{path,chunks}],removed:[path]}`, plus CLI text formats.
3. **`mcp-server.md` completeness.** It lists 5 of 6 tools — **`sai_prepare_mcp_setup` is a real
   registered tool** and **`--allow-setup`** (which lets the server execute the setup script) is
   undocumented in `docs/` (only in `SKILL.md`). Add both; reconcile `SKILL.md` as a pointer.
4. **`getting-started.md` + `introduction.md`.** A single copy-pasteable happy path and a
   concepts-first intro; today newcomers jump straight into pipeline mechanics.
5. **`internals/LOGICAL_AUDIT.md`.** `architecture.md` tells maintainers to "re-read the internal
   audit notes" but **no such file exists**. Write it (enumerate the invariants: chunking
   "nothing dropped", DuckDB HNSW bulk begin/end contract, cross-backend point IDs, prefix
   consistency, worker-thread isolation, dimension guards) or remove the dangling pointer.
6. **`contributing.md`.** Build/test, the feature matrix (`mcp`/`duckdb`/`qdrant`/`ort`/`ollama`/`ast`),
   module map (config/indexer/main/mcp/search/worker/vectordbs), running the MCP server locally.
7. **`ci-cd.md` + `keeping-in-sync.md`.** Real recipes (Actions/GitLab, husky/lefthook, HF cache
   in CI) instead of one `post-commit` stub.
8. Secondary: `performance.md`, `glossary.md`, `environment.md`, per-client integration pages,
   linked + de-staled changelog.

## 5. Tooling & hosting (a decision for you)

The page set in §3 is identical regardless of renderer; only nav/search/hosting differ.

- **Option A — Flat markdown in `docs/` (zero tooling).** Grow the existing `docs/*.md`, organized
  per §3, linked from README + the landing page. Works today on GitHub, no build step. No search,
  no sidebar. Lowest effort; good incremental step.
- **Option B — mdBook (recommended).** The Rust-ecosystem standard (rustc/cargo books). Pure
  markdown + a `SUMMARY.md`, gives sidebar nav + full-text search + theming, builds to static HTML.
  The existing `docs/*.md` port over with light edits. Hosting: keep the custom landing page at the
  Pages root and build the book to a subpath (e.g. `/book/`), via a GitHub Actions Pages deploy;
  the landing page's "Docs" link points at the book.
- **Option C — MkDocs Material.** Best-in-class search/UX, Python-based; same subpath/Actions hosting.

Recommendation: **B (mdBook)** for a Rust CLI — lowest-friction "real docs site," all-markdown so
content stays portable. Since content is tooling-agnostic, we can **author the pages first** and
pick the renderer when we wire hosting (this also defers the Pages-from-`/docs` vs Pages-Actions
decision, which currently blocks nothing).

## 6. Fixes to existing content (accuracy issues the inventory found)

These are wrong/stale today and should be corrected as part of the work:

- **Code:** `main.rs` top-level clap `about` is stale — "Index source files into Qdrant via
  E5-small server-side inference (Document API)" — DuckDB+ort is now first-class and the MCP
  default. Update the binary description.
- `architecture.md`: dangling reference to nonexistent "internal audit notes" (see §4.5).
- `docs/mcp-server.md`: missing `sai_prepare_mcp_setup` (6th tool) and `--allow-setup` (see §4.3).
- `docs/chunking.md` & `indexer.yaml` comment: AST coverage stated as "ts/tsx" but code's
  `AST_PREFERRED_EXTS` is ts/tsx/**rs/go**.
- `CHANGELOG.md`: 0.1.0 says AST is "TS/TSX" only — stale vs shipped Rust/Go; also not linked
  from README.
- `configuration.md` / `indexer.yaml`: `honor_noindex_marker` / `honor_noduplicate_marker` aren't
  in the shipped YAML (default-true behavior undocumented in the file users edit);
  `skip_generated_marker` is a plain bool that defaults **false** when omitted (asymmetric with the
  Option<bool> markers that default true) — call this out; `prefix_style` is config-only (no CLI
  flag); add a single authoritative resolution table; note that **unknown YAML keys are silently
  ignored** (typo'd key = silent no-op footgun).
- `cli.md`: subcommand-local flags (`duplicates --min-score/--top-k/--min-cluster-size/--max-clusters/--path-glob`,
  `similar --code/--path/--line/--limit/--min-score`) are prose-only — tabulate them; document the
  two distinct `--limit` defaults (top-level 5 vs `similar` 8); add `--allow-setup`.

## 7. Conventions

- Every code sample is copy-pasteable and matches a real flag/key (verified against the inventory).
- One authoritative reference table per surface; guides link to reference, never re-define.
- Each reference entry carries: type, default, resolution order, and "config-only vs CLI-flag."
- Tool/CLI names use the shipped `sai_`-prefixed forms.
- Cross-link domain terms to `glossary.md`.
- Keep `mcp-setup/SKILL.md` and `docs/mcp-server.md` in sync (mcp-server is the source of truth).

## 8. Phased roadmap

- **Phase 0 — Truth pass (small, do first):** fix the §6 inaccuracies in existing pages + code
  `about` string. Cheap, high trust impact, unblocks everything else.
- **Phase 1 — Newcomer path:** `introduction.md`, `getting-started.md`, `glossary.md`,
  `troubleshooting.md`. The biggest UX gap.
- **Phase 2 — Reference completeness:** expand `cli.md`/`configuration.md`/`mcp-server.md`,
  add `output-schemas.md`, `environment.md`.
- **Phase 3 — Operate & integrate:** `ci-cd.md`, `keeping-in-sync.md`, `security.md`,
  `performance.md`, per-client MCP guides.
- **Phase 4 — Contributor:** `contributing.md`, `internals/LOGICAL_AUDIT.md`, changelog linkage.
- **Phase 5 — Tooling/hosting:** if Option B/C, stand up mdBook/MkDocs + a Pages Actions deploy,
  wire the landing page "Docs" link, redirect old `docs/*.md` deep links.

## 9. Decisions needed before execution

1. **Renderer:** flat markdown (A) vs mdBook (B, recommended) vs MkDocs (C).
2. **First execution slice:** Phase 0 truth-pass first (recommended), or jump to the newcomer path.
3. **Audit doc:** author `internals/LOGICAL_AUDIT.md`, or just soften `architecture.md`'s pointer?
