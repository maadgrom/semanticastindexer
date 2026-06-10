# Glossary

Plain-language definitions for the domain terms used across the SAI docs. Terms with a dedicated reference page link to it.

## AST / tree-sitter

An **Abstract Syntax Tree (AST)** is the structured, parsed form of source code. SAI's `ast` chunker uses [tree-sitter](https://tree-sitter.github.io/) to parse TypeScript/TSX, Rust, and Go and emit **one chunk per named function**. It is feature-gated behind `--features ast`; non-function code (classes, types, imports, top-level statements) is deliberately not embedded. See [Chunking](../reference/chunking.md).

## chunk

One embeddable slice of a source file, ready to be turned into a vector and stored. A chunk carries its file path, language, start/end line, text, an optional captured `symbol` name, the commit it was indexed at, and a `dirty` flag. The `ast` chunker emits one chunk per function; the `lines` chunker emits line windows. See [Chunking](../reference/chunking.md).

## collection

The storage unit on the **qdrant** backend that holds all of a project's vectors. Creating the collection (or the equivalent DuckDB table plus index) is what `ensure_ready` does before indexing. The DuckDB backend uses a local file plus a VSS/HNSW index in place of a remote collection. See [Backends & embedders](../reference/backends-and-embedders.md).

## cosine similarity

A measure of how close two vectors point in the same direction, ranging up to `1.0` for identical direction. SAI ranks search results and clusters near-duplicates by cosine similarity; because vectors are L2-normalized, cosine similarity is what the DuckDB VSS/HNSW index and Qdrant compare on.

## embedding

The numeric vector representation of a chunk's text produced by an **embedder**. Similar code produces similar embeddings, which is what makes semantic search and near-duplicate detection possible. The DuckDB backend produces embeddings locally (via `ort` or `ollama`); the qdrant backend produces them server-side. See [Backends & embedders](../reference/backends-and-embedders.md).

## HNSW

**Hierarchical Navigable Small World**, the approximate-nearest-neighbor graph index used by the DuckDB **VSS** extension to make cosine search fast. HNSW loses recall after in-place deletes, so a DuckDB `sync` drops and recreates the index around its changed-file loop — effectively a full graph rebuild. See [Backends & embedders](../reference/backends-and-embedders.md).

## L2-normalization

Scaling a vector so its length (L2 norm) equals 1, leaving only its direction. The `ort` pipeline L2-normalizes every embedding after mean-pooling, which lets cosine similarity be compared directly and consistently across all stored vectors. See [Backends & embedders](../reference/backends-and-embedders.md).

## mean-pooling

Averaging the per-token output vectors of a transformer into a single fixed-size vector. The `ort` pipeline mean-pools the ONNX `last_hidden_state` **over the attention mask** (so padding tokens are excluded) before L2-normalizing — yielding 384 dimensions for e5-small. See [Backends & embedders](../reference/backends-and-embedders.md).

## MSRV

**Minimum Supported Rust Version** — the oldest Rust toolchain version the project compiles and is tested against. Building with an older toolchain is unsupported.

## near-duplicate cluster

A group of functions whose vectors are mutually close enough (above the duplicate score threshold) to be flagged as near-identical. `find_duplicates` (CLI `duplicates`) builds these clusters codebase-wide from stored vectors. Functions marked with a `sai-noduplicate` marker are still indexed and searchable but excluded from clustering. See [union-find clustering](#union-find-clustering).

## ONNX Runtime (ort)

The local embedding path used by the DuckDB backend. The **`ort`** embedder runs an [ONNX Runtime](https://onnxruntime.ai/) (`ort` 2.x) inference session over a model and tokenizer downloaded from Hugging Face (`onnx/model.onnx` + `tokenizer.json`). Its pipeline is: prefix → tokenize (pad/truncate to 512) → ONNX `last_hidden_state` → mean-pool → L2-normalize. It is the default embedder. See [Backends & embedders](../reference/backends-and-embedders.md).

## passage vs query prefix (E5 / Qwen / none)

Asymmetric text prefixes some embedding models expect. The resolved `prefix_style` (E5, Qwen, or None) is applied by both local embedders and the Qdrant path through one shared helper:

| Style | Passage (stored code) | Query (search text) |
| ----- | --------------------- | ------------------- |
| **e5** | `passage: <t>` | `query: <t>` |
| **qwen** | `<t>` (bare) | `Instruct: Given a code search query, retrieve relevant code\nQuery: <t>` |
| **none** | `<t>` | `<t>` |

The style is set explicitly via `prefix_style` or auto-detected from the model name (contains `e5` → E5, contains `qwen` → Qwen, otherwise None). Symmetric code models use `none`. See [Chunking → prefixes](../reference/chunking.md#embedding-prefixes).

## point ID

The numeric identifier (`id`, a `u64`) attached to each stored chunk/vector so it can be located, deduplicated, and self-excluded during search. Raw-vector search over-fetches and dedups by id because HNSW can return the same id more than once; `find_similar` and `find_duplicates` use the id to exclude a query chunk from its own results.

## semantic vs lexical search

**Lexical** search matches literal tokens or substrings. **Semantic** search matches *meaning* by comparing embeddings, so it can find code that does the same thing with different names or wording. SAI's `sai_search_code` is semantic. See the [MCP tools reference](../reference/mcp-server.md).

## server-side inference

Embedding performed by the remote service rather than locally. The **qdrant** backend uses Qdrant Cloud's server-side inference (the `Document` API) and has no local model, so the server itself turns text into vectors; plain OSS/local Qdrant has no inference engine. Contrast with the DuckDB backend's local `ort`/`ollama` embedders. See [Backends & embedders](../reference/backends-and-embedders.md).

## symbol

The captured name of a function stored on a chunk by the `ast` chunker (free functions, methods, and arrow/function-expression consts in TS; functions, impl/trait methods, and nested functions in Rust; `func` declarations and receiver methods in Go). It is `None` for line-window chunks, and is surfaced by the `similar`/`duplicates` CLI subcommands and by the `sai_search_code` / `sai_find_duplicates` MCP tools. See [Chunking](../reference/chunking.md).

## union-find clustering

The disjoint-set algorithm used to merge near-duplicate pairs into clusters: each function that is similar enough to another is unioned into the same group, so a chain of pairwise matches collapses into one cluster. This is how `sai_find_duplicates` turns pairwise similarity into [near-duplicate clusters](#near-duplicate-cluster).

## vector

The list of floating-point numbers (`Vec<f32>`) that represents a chunk's embedding. Vectors are what the backend stores, indexes (HNSW), and compares by cosine similarity. Search-by-vector reuses an exact stored vector when possible (e.g. `find_similar` reuses an existing function's vector rather than re-embedding).

## vector_dim

The configured dimensionality of the vectors a backend stores. It **must match the embedder model** and is validated at runtime — a mismatch is a clear error (`embedder produced 768-d vectors but vector_dim=384 …`). Examples: e5-small = 384, nomic-embed-text = 768, mxbai-embed-large = 1024. Changing `vector_dim` requires a fresh index (delete `.index/code.duckdb` or run with `--recreate`). See [Backends & embedders](../reference/backends-and-embedders.md).

## VSS

**Vector Similarity Search**, the DuckDB extension that provides the HNSW cosine index for the **duckdb** backend. It must be available for local vector search; the `duckdb` backend stores vectors in a DuckDB file and queries them through the VSS HNSW index. See [Backends & embedders](../reference/backends-and-embedders.md).

## Xet storage

Hugging Face's content-addressed storage system, used by some model repos. It matters because the pinned `hf-hub` (0.3) fails to fetch `tokenizer.json` from Xet-backed repos (e.g. `jinaai/jina-embeddings-v2-base-code`), producing a `relative URL without a base` error; the workaround is to stage `tokenizer.json` into the HF cache once with `curl` until `hf-hub` is upgraded. See [Backends & embedders](../reference/backends-and-embedders.md).
