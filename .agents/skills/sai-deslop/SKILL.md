---
name: sai-deslop
description: Use the semanticastindexer (SAI) MCP tools well during coding — search before writing, check for near-duplicates before adding a function, and TRIAGE every duplicate/similarity finding before acting. Triggers on "dedup", "deslop", "is this already implemented", "find similar", "duplicate", "are we repeating ourselves", and before/after refactors. The core of this skill is a triage protocol that forces you to read each finding's real source and classify it before proposing a fix.
version: "1.0.0"
author: semanticastindexer project
tags: [sai, mcp, deslop, near-duplicate, triage, deduplication, agentic]
---

# Using SAI: search, dedup, and triage

This skill is the **usage** companion to the `sai` setup skill. Setup wires the
MCP server up; this teaches you *when* to reach for the `sai_` tools while
coding, and — the important part — *how to interpret a finding* instead of
echoing it back.

> A duplicate cluster or a similarity hit is a **hypothesis, not a conclusion.**
> The tool found two vectors that sit close together. It does **not** know
> whether that is real logic duplication, harmless boilerplate, a required
> repetition (interface impl, generated code), or two unrelated line-windows that
> happen to embed alike. **You** decide that — and you only earn the right to
> decide after you have read the real source of every member.

## When to reach for the tools (inline reflexes)

| Situation | Tool | Why |
|-----------|------|-----|
| About to write a new function/helper | `sai_find_similar { code }` | If a tight neighbour already exists, reuse it instead of growing the slop. Needs a local embedder (the `duckdb` backend). |
| Asked "is this already implemented?" | `sai_search_code` then `sai_find_similar` | Find the existing impl in plain English, then confirm with code-vs-code. |
| About to refactor / rename a symbol | `sai_search_code` | Surface call sites and related code before you move anything. |
| Pre-PR / periodic "where do we repeat ourselves?" | `sai_find_duplicates` (or delegate to the `dedup-auditor` subagent) | Repo-wide near-duplicate audit. Heavyweight — prefer the subagent so the raw clusters don't flood your context. |
| "Is the index current?" | `sai_index_status` | Check backend/model/chunk_count before trusting results. If it looks stale, sync/re-index first. |

These tools are **read-only by default**; nothing leaves the machine on the local
DuckDB path. The write tools (`sai_refresh`, `sai_sync`) need `--allow-write`.

## Reading the output

- **`sai_search_code` / `sai_find_similar`** return `hits[]`: each hit has
  `path`, `start_line`, `end_line`, `symbol` (the function name, or **null** for a
  line-window chunk), `score` (cosine, 0–1), and `snippet`. The `snippet` from
  `sai_search_code` is **capped** (~8 lines / ~800 chars) unless you pass
  `include_text: true` — so it is a teaser, not the whole chunk.
- **`sai_find_duplicates`** returns `clusters[]`: each cluster has `size`,
  `members[]` (`path`/`start_line`/`end_line`/`symbol`), and `min_sim`/`max_sim`
  — the lowest/highest edge similarity inside the cluster. A tight `min..max` band
  means closer copies; a low `min` means a looser family that happens to be
  connected through a chain.

See [search-and-duplicates](../../../book/src/guides/search-and-duplicates.md) and
[output-schemas](../../../book/src/reference/output-schemas.md) for the exact shapes.

## The triage protocol (do this for EVERY finding)

This is the heart of the skill. Do not skip a step. Do not batch-judge from the
list. One finding at a time:

1. **Read, don't skim.** Open each member's real source at its
   `start_line..end_line` (use `Read` on the file). The snippet is capped and
   comments were stripped before embedding — you cannot judge intent from it.
2. **Classify the finding into exactly one bucket:**
   - **Real duplication** — same intent, copy-paste or near-copy logic. → a
     consolidation candidate. Proceed to step 4.
   - **Boilerplate / structural coincidence** — getters, error wrappers, simple
     loops, DTO shuffling. These embed similarly *because the shape is generic*,
     not because anyone duplicated logic. → usually leave alone. High score ≠ real
     dup.
   - **Intentional / required repetition** — trait/interface implementations,
     function overloads, generated code, test fixtures, parallel-by-design
     adapters. → leave alone; if it keeps surfacing, suggest a `sai-noduplicate`
     marker so it stops polluting the report (see
     [opt-out-markers](../../../book/src/guides/opt-out-markers.md)).
   - **Fragment artifact** — `symbol` is null (line-window chunker) so the member
     may be a *partial* slice of a larger function, not a self-contained unit. →
     verify the real boundaries before claiming anything.
3. **Cross-check the signals** before you commit to the bucket:
   `min_sim`/`max_sim` band, `symbol` present vs null, whether members live under
   generated/test/vendored paths, and whether the model+threshold are appropriate
   (cosines are **model-specific** — current model
   `jinaai/jina-embeddings-v2-base-code`; defaults `find_similar 0.85` /
   `duplicate 0.93`). See
   [tuning-similarity](../../../book/src/guides/tuning-similarity.md).
4. **Propose only verified fixes.** For a *real* duplication, give a concrete
   consolidation — extract a shared function, parametrize the one differing piece,
   hoist to a util — and then **verify it actually fits**: signatures and types
   line up, the abstraction doesn't force unnatural coupling or a god-parameter,
   and *every* call site can adopt it. If it does not cleanly fit, say so and stop
   — a half-fitting abstraction is worse than the duplication.
5. **Emit a verdict per finding**, never a bare echo of the tool output:

   ```
   <path:lines> ↔ <path:lines>  [score/band]
   verdict: real | false-positive | intentional
   why: <one line grounded in what you read>
   action: <verified consolidation, or "leave — <reason>", or "mark sai-noduplicate">
   ```

### Anti-laziness checklist — refuse these failure modes

- ❌ Judging a finding from the capped `snippet` instead of reading the file.
- ❌ Treating a high `score` as proof of duplication.
- ❌ Dumping the cluster list back to the user as if listing == analyzing.
- ❌ Proposing "extract a helper" without checking that the call sites can use it.
- ❌ "Fixing" boilerplate or intentional repetition into a forced abstraction.

If you have not read both members and cannot state *why* they are or aren't a
real duplicate, **you have not done the work.**

## Related

- [.agents/skills/sai/SKILL.md](../sai/SKILL.md) — installs/configures the MCP server.
- `dedup-auditor` subagent (Claude Code) — runs the repo-wide sweep with this
  protocol in an isolated context and returns a classified digest.
- [MCP server reference](../../../book/src/reference/mcp-server.md) — full tool contracts.
