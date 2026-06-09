# semanticastindexer — build & run the source indexer.
#
# IMPORTANT: We **always build with --features all** so the binary contains every
# backend (qdrant + duckdb), every embedder (ort + ollama), the MCP server, and
# the AST chunker. This is the recommended and supported configuration.
#
# Run `make` targets from this repo root. The indexer operates on a TARGET project
# directory (default: the current dir `.`); set TARGET=/path/to/project to index
# another repo. The absolute release binary ($(BIN)) is invoked inside $(TARGET).
#
# Credentials come from the environment: QDRANT_URL, QDRANT_API_KEY.
# A local `.env` next to this Makefile is auto-loaded if present (keep it gitignored).
# Use plain `KEY=value` lines in .env (no `export`, no quotes).
#
#   make build                          # optimized release binary (ALL features)
#   make run                            # dev run — defaults to a SAFE --dry-run over TARGET/src
#   make prod TARGET=/path/to/project   # production index of TARGET against the selected backend
#   make prod TARGET=. BACKEND=duckdb   # index into the local DuckDB backend (ort embedder)
#   make prod BACKEND=duckdb EMBEDDER=ollama  # DuckDB backend, Ollama embedder
#   make sync SINCE=HEAD~1              # re-index changed files (git hook)
#   make flush                          # delete the collection/table
#   make run ARGS="--query-only --query 'create collection'"   # override args
#   make duplicates                     # codebase-wide near-duplicate clusters
#   make duplicates DUP_ARGS="--min-score 0.85 --path-glob 'src/utils/**'"
#   make similar SIM_ARGS="--code 'function f(){}'"            # neighbours of a snippet
#   make similar SIM_ARGS="--path src/utils/x.ts --line 12"    # neighbours of a chunk
#   make docs                           # live-preview the docs (mdbook serve, auto-reload)
#   make site                           # build + serve the full static site (landing page + /book/)
#   make test / check-all               # test / clippy with --features all (already the default)
#
# All capabilities (MCP server, duplicates/similar, AST chunking, both backends)
# are available out of the box with the default full-featured build.

THIS_DIR  := $(patsubst %/,%,$(dir $(abspath $(lastword $(MAKEFILE_LIST)))))
MANIFEST  := $(THIS_DIR)/Cargo.toml
BIN       := $(THIS_DIR)/target/release/semanticastindexer

# Auto-load .env (if any) and export its vars to the indexer process.
-include $(THIS_DIR)/.env
export

# Overridable knobs.
# Project directory to index (the indexer cd's here). Default: current dir.
TARGET     ?= .
ROOT       ?= src
EXT        ?= ts,tsx
LANGUAGE   ?= ts
COLLECTION ?= source_code
BACKEND    ?= qdrant
# Embedder for the duckdb backend: ort (default) or ollama. Ignored by qdrant.
EMBEDDER   ?= ort
SINCE      ?= HEAD~1
# Default args for `run` — safe (no upload) unless overridden.
ARGS       ?= --root $(ROOT) --ext $(EXT) --language $(LANGUAGE) --collection $(COLLECTION) --backend $(BACKEND) --embedder $(EMBEDDER) --dry-run
# Extra args for the similarity subcommands (override per invocation).
DUP_ARGS   ?=
SIM_ARGS   ?= --code "function example() { return 1 }"

# Docs site (mdBook). `make docs` live-previews; `make site` serves the full
# deployed layout (landing page at /, book at /book/) just like GitHub Pages.
MDBOOK     ?= mdbook
DOCS_PORT  ?= 3000
SITE_PORT  ?= 8000
SITE_DIR   := $(THIS_DIR)/.site

.PHONY: build build-ort build-ollama build-ast run prod dry-run sync flush query duplicates similar test test-all fmt clippy check-all clean help require-mdbook book-build docs site static

build: ## Compile the optimized release binary with ALL features (recommended)
	cargo build --release --manifest-path $(MANIFEST) --features all

# Legacy / convenience aliases — all now produce the full-featured binary.
build-ort: build ## (alias) Full build (we always use --features all)
build-ollama: build ## (alias) Full build (we always use --features all)
build-ast: build ## (alias) Full build (we always use --features all)

run: build ## Dev run (default: safe --dry-run over TARGET/src). Override with ARGS="..."
	cd $(TARGET) && $(BIN) $(ARGS)

prod: build ## Index $(ROOT) under TARGET into $(BACKEND) (qdrant needs QDRANT_URL/API_KEY)
	cd $(TARGET) && $(BIN) --root $(ROOT) --ext $(EXT) --language $(LANGUAGE) --collection $(COLLECTION) --backend $(BACKEND) --embedder $(EMBEDDER)

dry-run: build ## Report what would be indexed/skipped under TARGET (no network)
	cd $(TARGET) && $(BIN) --root $(ROOT) --ext $(EXT) --dry-run

sync: build ## Re-index changed files in TARGET since SINCE (default HEAD~1) — for git hooks
	cd $(TARGET) && $(BIN) sync --since $(SINCE) --ext $(EXT) --collection $(COLLECTION) --backend $(BACKEND) --embedder $(EMBEDDER)

flush: build ## Delete the collection/table (flush all vectors)
	cd $(TARGET) && $(BIN) flush --collection $(COLLECTION) --backend $(BACKEND)

query: build ## Search: make query Q="how do we create the collection"
	cd $(TARGET) && $(BIN) --query-only --collection $(COLLECTION) --backend $(BACKEND) --embedder $(EMBEDDER) --query "$(Q)"

duplicates: build ## Near-duplicate clusters (full build has all backends/embedders)
	cd $(TARGET) && $(BIN) --collection $(COLLECTION) --backend $(BACKEND) --embedder $(EMBEDDER) duplicates $(DUP_ARGS)

similar: build ## Nearest neighbours (full build has all backends/embedders)
	cd $(TARGET) && $(BIN) --collection $(COLLECTION) --backend $(BACKEND) --embedder $(EMBEDDER) similar $(SIM_ARGS)

test: ## Run unit tests (uses --features all)
	cargo test --release --manifest-path $(MANIFEST) --features all

test-all: ## Run unit tests (uses --features all)
	cargo test --release --manifest-path $(MANIFEST) --features all

fmt: ## Format the crate
	cargo fmt --manifest-path $(MANIFEST)

clippy: ## Lint, warnings as errors (--features all)
	cargo clippy --release --manifest-path $(MANIFEST) --features all -- -D warnings

check-all: ## Lint with all backends, warnings as errors (--features all)
	cargo clippy --release --manifest-path $(MANIFEST) --features all -- -D warnings

clean: ## Remove build artifacts
	cargo clean --manifest-path $(MANIFEST)

require-mdbook:
	@command -v $(MDBOOK) >/dev/null 2>&1 || { echo "mdbook not found on PATH. Install it with: cargo install mdbook"; exit 1; }

book-build: require-mdbook ## Build the mdBook docs (output: book/html)
	$(MDBOOK) build $(THIS_DIR)/book

docs: require-mdbook ## Live-preview the docs (auto-reload) at http://localhost:$(DOCS_PORT)
	$(MDBOOK) serve $(THIS_DIR)/book --open --port $(DOCS_PORT)

site: book-build ## Build + serve the full static site (landing + /book/) at http://localhost:$(SITE_PORT)
	rm -rf $(SITE_DIR)
	mkdir -p $(SITE_DIR)/book
	cp $(THIS_DIR)/docs/index.html $(THIS_DIR)/docs/install.sh $(THIS_DIR)/docs/uninstall.sh $(THIS_DIR)/docs/.nojekyll $(SITE_DIR)/
	cp -R $(THIS_DIR)/book/html/. $(SITE_DIR)/book/
	@echo ""
	@echo "  Static site assembled at $(SITE_DIR)"
	@echo "  Open http://localhost:$(SITE_PORT)  — landing page at /, docs at /book/ (what GitHub Pages serves)."
	@echo "  Press Ctrl+C to stop."
	@echo ""
	cd $(SITE_DIR) && python3 -m http.server $(SITE_PORT)

static: site ## (alias) Build + serve the full static site

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) \
		| awk 'BEGIN{FS=":.*?## "}{printf "  \033[36m%-12s\033[0m %s\n", $$1, $$2}'
