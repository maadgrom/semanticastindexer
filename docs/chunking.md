# Chunker (smart default + `lines | ast`)

How each file is split into embeddable chunks.

**Defaulting rule** (when you do **not** explicitly set `--chunker` or `chunker:` in yaml):
- Languages with good AST support (currently `ts` / `tsx` / `rs` / `go`) **and** the binary was built with `--features ast` → we default to the symbol-aware `ast` chunker.
- Everything else → the reliable `lines` chunker.

You can always force a specific chunker with `--chunker lines` / `--chunker ast` or in `indexer.yaml`.

| Chunker | How | Symbols | Build |
| ------- | --- | ------- | ----- |
| **lines** (fallback) | line windows (~`MAX_LINES` lines / `max_chunk_chars` chars, small overlap) | none | always available |
| **ast** (preferred for TS/TSX/Rust/Go when available) | tree-sitter parse → one chunk per **function** | yes | included in `--features all` |

The **ast** chunker (tree-sitter, **TypeScript/TSX + Rust + Go**) is **function-only** — the
index exists to compare functions for near-duplicates, so non-function code is deliberately
not embedded:

- One chunk per **named function** at any depth: free functions, methods, and
  arrow/function-expression `const`s (TS); free functions, `impl`/trait methods, and nested
  functions (Rust); top-level `func` declarations and receiver methods (Go). The function's
  name is stored as the chunk's `symbol`.
- **Not chunked:** classes, interfaces, type aliases, `const`/`static`/`mod`/`struct`/
  `enum`/`trait` items, imports, top-level statements, and bare anonymous closures. (Earlier
  versions emitted these too, which flooded `duplicates` with tiny near-identical non-function
  vectors — every run reported the whole codebase.)
- An **oversized function** (text > `max_chunk_chars`) is line-split, keeping its symbol.
- A file with **no functions produces no chunks** — nothing else is indexed.
- **Parse-failure fallback:** a file that fails to parse, or any non-AST extension, falls
  back to the line chunker. Comments are stripped *before* chunking (the AST parses
  comment-stripped text).
- `chunker: ast` is **feature-gated** — if it is explicitly selected (or auto-selected
  for TS/TSX/Rust/Go) on a binary built without `--features ast`, you get a clear, actionable
  error telling you to rebuild with the feature.

## Chunk-size cap (`max_chunk_chars`)

The char bound BOTH chunkers honor (a ~4-chars/token approximation of the model's token
window; no tokenizer needed). Unset → **model-aware default**:

| Model / embedder | Window | Default cap |
| ---------------- | ------ | ----------- |
| **e5** / qdrant | 512 tokens | **≈ 1400 chars** (historical line-path behavior) |
| **qwen** / generic ollama | ~8K tokens | **≈ 32000 chars** (whole functions fit) |

## Embedding prefixes (`prefix_style: e5 | qwen | none`)

Resolved once (explicit config wins; else auto-detected from `model`: contains `e5` → E5,
contains `qwen` → Qwen, else None) and applied by **both** local embedders **and** the
Qdrant `Document` path through one shared helper:

| Style | Passage | Query |
| ----- | ------- | ----- |
| **e5** | `passage: <t>` | `query: <t>` |
| **qwen** | `<t>` (bare) | `Instruct: Given a code search query, retrieve relevant code\nQuery: <t>` |
| **none** | `<t>` | `<t>` |
