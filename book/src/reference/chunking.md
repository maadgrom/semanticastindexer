# Chunking

How SAI splits each source file into the embeddable units ("chunks") that get vectorized, stored, and compared. SAI ships two chunkers — the line-window chunker (`lines`) and the symbol-aware tree-sitter chunker (`ast`) — and picks one for you unless you say otherwise.

For the terms used here (chunk, symbol, embedder), see the [glossary](../concepts/glossary.md). To choose the chunker on the command line or in `sai-cfg.yml`, see the [CLI reference](../reference/cli.md) and [Configuration](../reference/configuration.md).

## Smart default (`lines` vs `ast`)

When you do **not** explicitly set `--chunker` (CLI) or `chunker:` (in `sai-cfg.yml`), SAI defaults as follows:

- Languages with good AST support — currently **`ts`, `tsx`, `rs`, and `go`** — **and** a binary built with `--features ast` default to the symbol-aware `ast` chunker.
- Everything else defaults to the reliable `lines` chunker.

You can force a specific chunker:

```bash
# Force the line-window chunker
sai index --chunker lines

# Force the AST chunker (requires a binary built with --features ast)
sai index --chunker ast
```

```yaml
# sai-cfg.yml
chunker: ast   # or: lines
```

| Chunker | How it splits | Symbols | Availability |
| ------- | ------------- | ------- | ------------ |
| **`lines`** (fallback) | Line windows: up to ~60 lines (`MAX_LINES`) **and** up to `max_chunk_chars` characters per window, with an 8-line overlap (`OVERLAP_LINES`) | none (`symbol` is empty) | always compiled in |
| **`ast`** (preferred for TS/TSX/Rust/Go/Python when available) | tree-sitter parse → **one chunk per function** | yes (function name) | requires `--features ast` (included in `--features all`) |

## The AST chunker is function-only

The index exists to compare **functions** for near-duplicates, so the AST chunker (tree-sitter, for **TypeScript/TSX + Rust + Go + Python**) deliberately embeds functions and nothing else. Earlier versions emitted non-function nodes too, which flooded duplicate detection with tiny, near-identical vectors — making every run report the whole codebase as duplicated.

### What gets chunked

One chunk per **named function** at any nesting depth. The function's name is stored as the chunk's `symbol`:

| Language | Captured as functions |
| -------- | --------------------- |
| **TypeScript / TSX** (`.ts`, `.tsx`) | named `function` declarations; class/object methods (`method_definition`); and arrow/function-expression `const`s — the binding name (e.g. `const double = (n) => ...`) becomes the symbol |
| **Rust** (`.rs`) | every `function_item` — free functions, `impl` and trait default-body methods, and nested functions (the pattern matches at any depth) |
| **Go** (`.go`) | top-level `func` declarations and receiver methods (`func (r T) M()`). The symbol is the bare method name; the receiver is not qualified into it |
| **Python** (`.py`) | every `def` / `async def` — free functions, class methods, and nested functions (the pattern matches at any depth). Decorators are not part of the chunk: it starts at the `def` line |

### What is NOT chunked

The AST chunker drops everything that is not a function. A file made entirely of these produces **zero chunks** — nothing else is indexed:

- Classes, interfaces, and type aliases
- `const` / `static` / `mod` / `struct` / `enum` / `trait` items
- Imports and top-level statements
- Bare anonymous closures / func literals / Python `lambda`s (one-line lambdas are tiny and near-identical, so capturing them would collapse every duplicate run into one cluster)

Go's only nested-function form is a func literal (a closure), and — like Rust closures — those are intentionally not captured.

### Edge cases and fallbacks

- **Oversized function** — a function whose byte length exceeds `max_chunk_chars` is line-split over its own span via the shared line chunker, and **every** resulting window keeps the function's `symbol`.
- **No functions in the file** — the file produces no chunks; nothing else is indexed.
- **Parse-failure fallback** — a file that fails to parse (root parse error), or any extension without a tree-sitter grammar (anything other than `.ts` / `.tsx` / `.rs` / `.go` / `.py`, e.g. Java), silently falls back to the line chunker.
- **Comments are stripped *before* chunking** — so the AST parses comment-stripped text. Stripping preserves the exact line count, so a chunk's `start_line`/`end_line` still point at the real lines in the original file.
- **Exact-span dedupe** — two captures sharing an identical byte span are collapsed to one chunk, preferring the one that carries a symbol. Nested functions are emitted in their own right **and** remain part of their enclosing function's chunk (no carve-out); the overlap is acceptable for near-duplicate detection.

### Feature gating

The `ast` chunker is **feature-gated**. If `chunker: ast` is explicitly selected — or auto-selected for a TS/TSX/Rust/Go/Python file — on a binary built **without** `--features ast`, SAI fails fast at startup with a clear, actionable error:

```text
chunker 'ast' selected but this binary was built without the 'ast' feature (rebuild with --features ast)
```

Rebuild with the feature to enable it:

```bash
cargo build --release --features ast
# or pull in everything:
cargo build --release --features all
```

An unknown chunker value (anything other than `lines` or `ast`) is likewise rejected at startup:

```text
unknown chunker '<value>' (expected 'lines' or 'ast')
```

## Chunk-size cap (`max_chunk_chars`)

`max_chunk_chars` is the character bound that **both** chunkers honor. It is a ~4-characters-per-token approximation of the embedding model's token window, so no tokenizer is needed at index time. The line chunker uses it to bound each window; the AST chunker uses it to decide when a function is oversized and must be line-split.

When `max_chunk_chars` is left unset, SAI picks a **model-aware default**:

| Model / embedder | Token window | Default cap |
| ---------------- | ------------ | ----------- |
| **e5** / Qdrant | 512 tokens | **≈ 1400 chars** (the historical line-path behavior) |
| **qwen** / generic Ollama | ~8K tokens | **≈ 32000 chars** (so whole functions fit in one chunk) |

For picking a model and embedder, see [Choosing a model](../guides/choosing-a-model.md) and [Backends and embedders](../reference/backends-and-embedders.md).

## Embedding prefixes (`prefix_style: e5 | qwen | none`)

Some embedding models expect their input to carry a task prefix. SAI resolves the prefix style **once** and applies it through one shared helper used by **both** the local embedders and the Qdrant `Document` path, so passages and queries are prefixed identically everywhere.

**Resolution order:** an explicit `prefix_style` in config wins. Otherwise SAI auto-detects from the model name: a name containing `e5` → **E5**; a name containing **`qwen` → Qwen**; anything else → **None**. So the Qwen prefix applies when the model name contains `qwen`, or when you set `prefix_style: qwen` explicitly.

| Style | Passage prefix | Query prefix |
| ----- | -------------- | ------------ |
| **`e5`** | `passage: <text>` | `query: <text>` |
| **`qwen`** | `<text>` (bare, no prefix) | `Instruct: Given a code search query, retrieve relevant code\nQuery: <text>` |
| **`none`** | `<text>` | `<text>` |

## Related pages

- [Indexing a project](../guides/indexing.md) — running the indexer end to end.
- [Opt-out markers](../guides/opt-out-markers.md) — `sai-noindexing` / `sai-noduplicate` markers, which are scanned per chunk over the original (pre-strip) lines.
- [Configuration](../reference/configuration.md) — `chunker`, `max_chunk_chars`, `prefix_style`, and `strip_comments` keys.
- [Backends and embedders](../reference/backends-and-embedders.md) — how chunks are embedded and stored.
