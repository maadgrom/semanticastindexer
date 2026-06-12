#!/usr/bin/env bash
#
# semanticastindexer MCP Setup Script
#
# Sets up the semantic code search indexer as an MCP server for any agentic
# coding system (Claude Code, Cursor, Windsurf, Continue, etc.).
#
# Usage:
#   ./setup.sh                    # Interactive
#   ./setup.sh --non-interactive --backend duckdb --embedder ollama
#   ./setup.sh --help
#
# This script is designed to be used both by humans and by agents via MCP skills.
#
set -euo pipefail

# --- Configuration ---
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BINARY_NAME="semanticastindexer"
# We recommend building with the full "all" feature set so every capability
# (both backends, both embedders, MCP server, AST chunker) is present.
DEFAULT_FEATURES="all"
RECOMMENDED_FEATURES_ORT="all"
RECOMMENDED_FEATURES_OLLAMA="all"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

log() { echo -e "${BLUE}[setup]${NC} $*"; }
success() { echo -e "${GREEN}[setup]${NC} $*"; }
warn() { echo -e "${YELLOW}[setup]${NC} $*"; }
error() { echo -e "${RED}[setup]${NC} $*" >&2; }

print_help() {
    cat <<EOF
semanticastindexer MCP Setup

Sets up semantic code search as an MCP server for agentic coding tools.

Options:
  --non-interactive          Run without prompts (good for agents)
  --backend <qdrant|duckdb>  Vector backend (default: duckdb)
  --embedder <ort|ollama>    Embedder when using duckdb (default: ollama)
  --features <list>          Custom cargo features
  --target-dir <path>        Directory to index (default: current dir)
  --collection <name>        Collection name (default: source_code)
  --install-global           Install binary to ~/.local/bin and create wrapper
  --help                     Show this help

Examples:
  # Fully offline, high quality (recommended — uses --features all)
  ./setup.sh --backend duckdb --embedder ort --features "$RECOMMENDED_FEATURES_ORT"

  # Also excellent (uses --features all under the hood)
  ./setup.sh --backend duckdb --embedder ollama

  # Non-interactive for agent use
  ./setup.sh --non-interactive --backend duckdb --embedder ollama
EOF
}

# --- Argument Parsing ---
NON_INTERACTIVE=false
BACKEND="duckdb"
EMBEDDER="ollama"
FEATURES=""
TARGET_DIR="."
COLLECTION="source_code"
INSTALL_GLOBAL=false

while [[ $# -gt 0 ]]; do
    case "$1" in
        --non-interactive) NON_INTERACTIVE=true; shift ;;
        --backend) BACKEND="$2"; shift 2 ;;
        --embedder) EMBEDDER="$2"; shift 2 ;;
        --features) FEATURES="$2"; shift 2 ;;
        --target-dir) TARGET_DIR="$2"; shift 2 ;;
        --collection) COLLECTION="$2"; shift 2 ;;
        --install-global) INSTALL_GLOBAL=true; shift ;;
        --help|-h) print_help; exit 0 ;;
        *) error "Unknown argument: $1"; print_help; exit 1 ;;
    esac
done

# Derive features if not explicitly provided
if [[ -z "$FEATURES" ]]; then
    if [[ "$BACKEND" == "duckdb" && "$EMBEDDER" == "ort" ]]; then
        FEATURES="$RECOMMENDED_FEATURES_ORT"
    elif [[ "$BACKEND" == "duckdb" && "$EMBEDDER" == "ollama" ]]; then
        FEATURES="$RECOMMENDED_FEATURES_OLLAMA"
    else
        FEATURES="$DEFAULT_FEATURES"
    fi
fi

# --- Prerequisite Checks ---
check_prerequisites() {
    log "Checking prerequisites..."

    if ! command -v cargo &> /dev/null; then
        error "Rust/Cargo is required but not found."
        error "Install from https://rustup.rs/"
        exit 1
    fi

    if ! command -v rustup &> /dev/null; then
        warn "rustup not found — some features may fail to compile."
    fi

    success "Rust toolchain found: $(rustc --version 2>/dev/null || echo 'unknown')"
}

# --- Build the binary ---
build_binary() {
    log "Building semanticastindexer with features: $FEATURES"

    cd "$PROJECT_ROOT"

    if [[ "$NON_INTERACTIVE" == false ]]; then
        log "This may take several minutes the first time (especially with ort)..."
    fi

    cargo build --release --features "$FEATURES"

    local bin_path="$PROJECT_ROOT/target/release/$BINARY_NAME"
    if [[ ! -x "$bin_path" ]]; then
        error "Build succeeded but binary not found at $bin_path"
        exit 1
    fi

    success "Binary built: $bin_path"
    echo "$bin_path"
}

# --- Create or update sai-cfg.yml for agentic use ---
create_indexer_config() {
    local target="$1"
    local config_path="$target/sai-cfg.yml"

    if [[ -f "$config_path" && "$NON_INTERACTIVE" == false ]]; then
        warn "sai-cfg.yml already exists in $target"
        read -rp "Overwrite with recommended agentic defaults? [y/N] " answer
        if [[ ! "$answer" =~ ^[Yy]$ ]]; then
            log "Keeping existing sai-cfg.yml"
            return
        fi
    fi

    log "Creating recommended sai-cfg.yml for agentic code search..."

    cat > "$config_path" <<'YAML'
# semanticastindexer configuration optimized for agentic coding systems.
# Good balance of recall, speed, and noise reduction.

backend: duckdb
embedder: ollama          # Change to "ort" for fully offline (bigger binary; then Jina code model becomes the default)
chunker: lines            # Use "ast" (with --features ast) for symbol-aware chunks
collection: source_code
model: intfloat/multilingual-e5-small
vector_dim: 384           # When using embedder=ort the runtime default is now jinaai/jina-embeddings-v2-base-code (768d)

duckdb:
  path: .index/code.duckdb

# Strong but practical excludes for modern codebases
exclude_dirs:
  - node_modules
  - .git
  - dist
  - build
  - target
  - .next
  - coverage
  - .turbo
  - __tests__
  - .venv
  - venv

exclude:
  - "**/*.test.*"
  - "**/*.spec.*"
  - "**/*.d.ts"
  - "**/components/ui/**"     # shadcn, radix, etc.
  - "**/*.generated.*"
  - "**/*.gen.*"
  - "**/generated/**"
  - "**/*.pb.go"
  - "**/*_gen.go"
  - "**/mock_*.go"
  - "**/zz_generated*.go"
  - "**/*.min.js"
  - "**/*.min.css"

skip_generated_marker: true
strip_comments: true

# Similarity thresholds tuned for e5-small (lower them if you switch to a code model like Jina)
similarity:
  find_similar_min_score: 0.82
  duplicate_min_score: 0.91
  duplicate_min_cluster_size: 2
  top_k: 12
YAML

    success "Created $config_path"
}

# --- Generate MCP configuration snippets ---
generate_mcp_configs() {
    local bin_path="$1"
    local target="$2"

    log "Generating MCP configuration snippets..."

    local abs_bin
    abs_bin="$(cd "$(dirname "$bin_path")" && pwd)/$(basename "$bin_path")"

    local mcp_args='["mcp", "--backend", "'"$BACKEND"'", "--embedder", "'"$EMBEDDER"'", "--collection", "'"$COLLECTION"'"]'

    # .mcp.json (Claude Code / many tools)
    cat > "$target/.mcp.json.example" <<EOF
{
  "mcpServers": {
    "semantic-code-search": {
      "command": "$abs_bin",
      "args": $mcp_args,
      "cwd": "$target"
    }
  }
}
EOF

    # Claude Desktop (macOS example)
    cat > "$target/claude-desktop-config.example.json" <<EOF
{
  "mcpServers": {
    "semantic-code-search": {
      "command": "$abs_bin",
      "args": $mcp_args,
      "cwd": "$target"
    }
  }
}
EOF

    success "MCP configs written to:"
    echo "  - $target/.mcp.json.example"
    echo "  - $target/claude-desktop-config.example.json"
    echo ""
    echo "Copy the relevant block into your actual config file."
}

# --- Install globally (optional) ---
install_globally() {
    local bin_path="$1"

    local dest_dir="$HOME/.local/bin"
    mkdir -p "$dest_dir"

    local dest_bin="$dest_dir/$BINARY_NAME"
    cp "$bin_path" "$dest_bin"
    chmod +x "$dest_bin"

    success "Installed to $dest_bin"

    if ! echo "$PATH" | grep -q "$dest_dir"; then
        warn "$dest_dir is not in your PATH."
        warn "Add this to your shell rc:"
        echo "  export PATH=\"\$HOME/.local/bin:\$PATH\""
    fi

    # Create a convenience wrapper for MCP use
    local wrapper="$dest_dir/code-search-mcp"
    cat > "$wrapper" <<EOF
#!/usr/bin/env bash
# Convenience wrapper for semantic code search MCP server
exec "$dest_bin" mcp "\$@"
EOF
    chmod +x "$wrapper"
    success "Created convenience command: code-search-mcp"
}

# --- Main flow ---
main() {
    echo ""
    log "semanticastindexer MCP Setup for Agentic Systems"
    echo ""

    check_prerequisites

    local bin_path
    bin_path=$(build_binary)

    local target
    target="$(cd "$TARGET_DIR" && pwd)"

    log "Target project directory: $target"

    create_indexer_config "$target"
    generate_mcp_configs "$bin_path" "$target"

    if [[ "$INSTALL_GLOBAL" == true ]]; then
        install_globally "$bin_path"
    fi

    echo ""
    success "Setup complete!"
    echo ""
    echo "Next steps:"
    echo "  1. cd into a project you want to search"
    echo "  2. Run: $bin_path --dry-run                 (see what would be indexed)"
    echo "  3. Run: $bin_path                          (index it)"
    echo "  4. Add the MCP server config from the generated .example files"
    echo "  5. Restart your agentic tool"
    echo ""
    echo "Recommended first command in a new project:"
    echo "  $bin_path --root src --ext ts,tsx,js,jsx --dry-run"
    echo ""
    echo "We build with --features all by default (includes ort, ollama, ast, mcp, etc.)."
    echo "If you ever want a smaller binary you can build with a subset of features."
    echo ""
}

main "$@"
