# Getting started

This is a single, linear walkthrough: install the binary, index a project, ask it a
question in plain English, find near-duplicate functions, and (optionally) connect a coding
agent. The whole tutorial uses the **fully-offline default** — the DuckDB backend with the
on-device `ort` (ONNX Runtime) embedder — so there are **no API keys and no servers** to set
up. After the first model download, nothing else leaves your machine.

If you only want the reference rather than a tutorial, jump to [CLI usage](reference/cli.md)
or the [installation guide](installation.md).

## 1. Install

One line, no Rust toolchain required (it downloads a prebuilt binary from the latest GitHub
Release and puts `semanticastindexer` on your `PATH`):

```bash
# macOS / Linux
curl -fsSL https://maadgrom.github.io/semanticastindexer/install.sh | bash

# Windows (PowerShell)
powershell -c "irm https://maadgrom.github.io/semanticastindexer/install.ps1 | iex"
```

Prefer to build it yourself? Build with all features enabled so the offline default
(DuckDB + ort) works out of the box, then use the binary at
`./target/release/semanticastindexer`:

```bash
cargo build --release --features all
```

See the [installation guide](installation.md) for build requirements and details.

## 2. Move into your project

Run the binary from the **repo root of the project you want to search**, so the stored paths
are project-relative:

```bash
cd /path/to/your/project
```

Optionally generate a starter `sai-cfg.yml` — the fully-commented standard config. A short
interview asks for the backend, embedder, collection, and model (Enter accepts every
default; `--yes` skips the questions entirely):

```bash
semanticastindexer init
```

Without a config, the built-in defaults apply — fine for this tutorial. See the
[configuration reference](reference/configuration.md) for every key.

## 3. Dry-run to preview what gets indexed

Before touching the index, do a **dry-run**. It reports exactly which files would be included
or skipped — **no network, no embedding, no model download**:

```bash
semanticastindexer --root src --ext ts,tsx --dry-run
```

Expected output (abbreviated — your file list will differ):

```text
[include] src/app.ts
[include] src/utils/format.ts
[skip]    src/app.test.ts        (test file)
[skip]    src/components/ui/...   (shadcn)
...
dry-run: 42 included, 7 excluded (no upload)
```

`--root` defaults to `src` and `--ext` defaults to `ts,tsx`. Adjust them for your project
(for example `--ext go` or `--ext rs`). Always dry-run first to confirm the inclusion set.

## 4. Index it

Now run the real index. It creates the local DuckDB table if it's missing and embeds each
chunk on-device:

```bash
semanticastindexer --root src --ext ts,tsx
```

> **First run downloads a model.** With the default `ort` embedder, the code-trained model
> [`jina-embeddings-v2-base-code`](https://huggingface.co/jinaai/jina-embeddings-v2-base-code)
> (161M params, 768-dim) is pulled from Hugging Face the first time you index. That's a
> few hundred MB and only happens once — it's cached for every later run. If the download
> stalls or fails (proxy, offline, Hugging Face hiccup), see
> [troubleshooting](operations/troubleshooting.md).

Expected output (abbreviated):

```text
downloading model jinaai/jina-embeddings-v2-base-code ... done
indexing src (ext: ts,tsx) → collection source_code
embedded 318 chunks
upserted 318 points
done
```

Subsequent indexes reuse the cached model and start immediately.

## 5. Ask a question in plain English

Search the index with `--query-only` (this skips indexing and only searches — it never
uploads your codebase):

```bash
semanticastindexer --query-only --query "where do we open the duckdb connection"
```

Expected output (abbreviated — `--limit` defaults to 5 results):

```text
0.71  src/db/connection.ts:12-40   openDuckDb
0.64  src/db/pool.ts:8-31          createPool
0.58  src/index.ts:55-77           bootstrap
...
```

Each line is `score  path:start-end  symbol`. Higher scores are closer matches.

## 6. Find near-duplicate functions

The `duplicates` command scans the **stored vectors** (no re-embedding) and groups
near-identical functions into clusters using nearest-neighbour edges plus union-find:

```bash
semanticastindexer duplicates
```

Expected output (abbreviated):

```text
cluster (size 3, sim 0.94..0.97):
  src/utils/format.ts:10-24      formatDuration
  src/lib/time.ts:31-44          humanizeSeconds
  src/components/Clock.tsx:60-72  toClock

cluster (size 2, sim 0.93..0.93):
  src/api/users.ts:18-29         mapUser
  src/api/admin.ts:40-51         mapAdminUser
```

Clusters print largest-first. The built-in cutoff is a similarity of `0.93`; you can tune it
with `--min-score`, `--top-k`, and friends. See
[CLI usage](reference/cli.md) for the full set of `duplicates` and `similar` flags.

## 7. Connect a coding agent (optional)

The binary is a complete CLI on its own, but you can also expose it to a coding agent over
MCP. Re-run the installer with `--platform claude-code` (macOS/Linux) or
`-Platform claude-code` (Windows) and it wires up the project's `.mcp.json` (and installs
the Claude Code skill):

```bash
# macOS / Linux
curl -fsSL https://maadgrom.github.io/semanticastindexer/install.sh | bash -s -- --platform claude-code
```

```powershell
# Windows (the scriptblock form is how flags pass through irm)
powershell -c "& ([scriptblock]::Create((irm https://maadgrom.github.io/semanticastindexer/install.ps1))) -Platform claude-code"
```

Other supported ids include `claude-desktop`, `cursor`, `windsurf`, `continue`, `codex`,
`hermes`, `ollama`, and `generic`. See the [installation guide](installation.md) for
per-platform config locations.

Once your agent has restarted and picked up the MCP server, ask it something that triggers a
search. It calls the `sai_search_code` tool — the same semantic search you ran
in step 5:

```text
You: Using semantic code search, where do we create the Qdrant collection?

Agent (via sai_search_code):
  src/db/qdrant.ts:22-48  ensureCollection — creates the collection if it doesn't exist
  src/db/setup.ts:9-30    bootstrapVectorStore
```

The other shipped tools are `sai_find_similar`, `sai_find_duplicates`, `sai_index_status`,
and the write-only `sai_refresh` (which requires `--allow-write`). The server is **read-only
by default**.

## What next

- Tune what leaves the repo and learn the opt-out markers via the
  [installation guide](installation.md) and [CLI reference](reference/cli.md).
- Hit a snag (model download, no results, wrong paths)? See
  [troubleshooting](operations/troubleshooting.md).
