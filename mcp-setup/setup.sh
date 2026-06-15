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
SERVER_NAME="sai"
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
BOLD='\033[1m'
NC='\033[0m' # No Color

log() { echo -e "${BLUE}[setup]${NC} $*"; }
success() { echo -e "${GREEN}[setup]${NC} $*"; }
warn() { echo -e "${YELLOW}[setup]${NC} $*"; }
error() { echo -e "${RED}[setup]${NC} $*" >&2; }

# Shared MCP client-wiring library (snippet generators + configure_platform).
# Canonical source; docs/install.sh keeps a curl-pipe-safe inline copy kept in sync
# by mcp-setup/tests/test_setup.sh.
# shellcheck source=lib/mcp-config.sh
. "$SCRIPT_DIR/lib/mcp-config.sh"

print_help() {
    cat <<EOF
semanticastindexer MCP Setup

Sets up semantic code search as an MCP server for agentic coding tools.

Options:
  --non-interactive          Run without prompts (good for agents)
  --backend <qdrant|duckdb>  Vector backend (default: duckdb)
  --embedder <ort|ollama>    Embedder when using duckdb (default: ort, fully offline)
  --features <list>          Custom cargo features (default: all)
  --target-dir <path>        Directory to index (default: current dir)
  --collection <name>        Collection name (default: source_code)
  --platform <id>            Also wire up a client: claude-code, claude-desktop, cursor,
                             windsurf, continue, codex, hermes, generic (repeatable)
  --write                    Merge config into the client's file (JSON clients; backs up)
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
EMBEDDER="ort"
FEATURES=""
TARGET_DIR="."
COLLECTION="source_code"
INSTALL_GLOBAL=false
PLATFORMS=""
WRITE=false

while [[ $# -gt 0 ]]; do
    case "$1" in
        --non-interactive) NON_INTERACTIVE=true; shift ;;
        --backend) BACKEND="$2"; shift 2 ;;
        --embedder) EMBEDDER="$2"; shift 2 ;;
        --features) FEATURES="$2"; shift 2 ;;
        --target-dir) TARGET_DIR="$2"; shift 2 ;;
        --collection) COLLECTION="$2"; shift 2 ;;
        --platform) PLATFORMS="$PLATFORMS $2"; shift 2 ;;
        --write) WRITE=true; shift ;;
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

    local template="$SCRIPT_DIR/templates/sai-cfg.yml"
    if [[ ! -f "$template" ]]; then
        error "Config template not found at $template"
        exit 1
    fi

    # Single source of truth: start from the canonical template (duckdb + ort + jina-768 +
    # ast + tuned thresholds), then patch only the top-level backend/embedder/collection
    # lines to the chosen flags. Portable sed (no in-place; works on macOS + Linux).
    sed -E \
        -e "s|^backend: .*|backend: ${BACKEND}|" \
        -e "s|^embedder: .*|embedder: ${EMBEDDER}|" \
        -e "s|^collection: .*|collection: ${COLLECTION}|" \
        "$template" > "$config_path"

    success "Created $config_path (from templates/sai-cfg.yml)"

    if [[ "$EMBEDDER" == "ollama" ]]; then
        warn "embedder=ollama: set ollama.model and a MATCHING vector_dim in $config_path"
        warn "  (mxbai-embed-large = 1024, nomic-embed-text = 768 — the file's comments show how)."
    fi
    if [[ "$BACKEND" == "qdrant" ]]; then
        warn "backend=qdrant: set qdrant.url (or QDRANT_URL) and export QDRANT_API_KEY (secret)."
    fi
}

# --- Generate MCP configuration snippets ---
# Uses the shared json_snippet() from lib/mcp-config.sh so the example matches exactly
# what docs/install.sh emits. Relies on SERVER_NAME + PROJECT_DIR being set (see main()).
generate_mcp_configs() {
    local abs_bin="$1"
    local target="$2"

    log "Generating MCP configuration snippets..."

    # Both files share one shape (Claude Code project .mcp.json vs Claude Desktop config —
    # same JSON, different destinations); build once from the single shared builder.
    local snippet
    snippet="$(json_snippet "$abs_bin")"
    printf '%s\n' "$snippet" > "$target/.mcp.json.example"
    printf '%s\n' "$snippet" > "$target/claude-desktop-config.example.json"

    success "MCP configs written to:"
    echo "  - $target/.mcp.json.example                 (Claude Code: rename to .mcp.json in your project)"
    echo "  - $target/claude-desktop-config.example.json (Claude Desktop: merge into claude_desktop_config.json)"
    echo ""
    echo "NOTE: the \"command\" path is THIS machine's binary — change it if you move/share the file."
    echo "For other clients (Cursor, Windsurf, Continue, Codex), re-run with --platform <id>,"
    echo "or use the one-line installer: $PROJECT_ROOT/docs/install.sh --platform <id>"
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

    # Create a convenience wrapper (true passthrough — sai mcp, sai index, sai search all work)
    local wrapper="$dest_dir/sai"
    cat > "$wrapper" <<EOF
#!/usr/bin/env bash
# sai CLI / MCP server wrapper
exec "$dest_bin" "\$@"
EOF
    chmod +x "$wrapper"
    success "Created convenience command: sai"
}

# --- Install the portable agent skills ---
# Skills under .agents/skills/ are PORTABLE across agentic tools. Claude Code auto-loads
# them from ~/.claude/skills/, so we drop copies there; other clients (Cursor, Windsurf,
# Continue, …) read the same SKILL.md files from the repo's .agents/skills/ tree.
# Idempotent: safe to call from main() and from configure_platform()'s claude-code branch.
install_agent_skill() {
    [[ -n "${_SAI_SKILLS_INSTALLED:-}" ]] && return
    _SAI_SKILLS_INSTALLED=1

    # Upgrade cleanup: remove old-named skill dir so upgraders are not stranded.
    if [[ -d "$HOME/.claude/skills/semantic-code-search-mcp" ]]; then
        rm -rf "$HOME/.claude/skills/semantic-code-search-mcp"
        success "Removed old skill dir $HOME/.claude/skills/semantic-code-search-mcp"
    fi

    local src_root="$PROJECT_ROOT/.agents/skills"
    if [[ ! -d "$src_root" ]]; then
        warn "No skills dir at $src_root — skipping skill install."
        return
    fi

    # Install every skill under .agents/skills/<name>/SKILL.md (sai, sai-deslop, …).
    local installed=0
    local skill_src
    for skill_src in "$src_root"/*/SKILL.md; do
        [[ -f "$skill_src" ]] || continue
        local name
        name="$(basename "$(dirname "$skill_src")")"
        local skill_dir="$HOME/.claude/skills/$name"
        mkdir -p "$skill_dir"
        cp "$skill_src" "$skill_dir/SKILL.md"
        success "Installed skill → $skill_dir/SKILL.md"
        installed=$((installed + 1))
    done
    [[ "$installed" -gt 0 ]] || warn "No SKILL.md files found under $src_root."
}

# --- Install the Claude Code subagents ---
install_claude_agents() {
    local src_root="$PROJECT_ROOT/.claude/agents"
    if [[ ! -d "$src_root" ]]; then
        warn "No subagents dir at $src_root — skipping subagent install."
        return
    fi

    local agent_dir="$HOME/.claude/agents"
    mkdir -p "$agent_dir"
    local agent_src
    for agent_src in "$src_root"/*.md; do
        [[ -f "$agent_src" ]] || continue
        cp "$agent_src" "$agent_dir/$(basename "$agent_src")"
        success "Installed subagent → $agent_dir/$(basename "$agent_src")"
    done
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
    # PROJECT_DIR is the contract var the shared lib (json_snippet/configure_platform) reads.
    PROJECT_DIR="$target"
    # Absolute binary path, computed once and reused for both the example configs and
    # any --platform wiring (the shared lib embeds it as the server "command").
    local abs_bin
    abs_bin="$(cd "$(dirname "$bin_path")" && pwd)/$(basename "$bin_path")"

    log "Target project directory: $target"

    create_indexer_config "$target"
    generate_mcp_configs "$abs_bin" "$target"
    install_agent_skill
    install_claude_agents

    # If a client was requested, wire it up for real via the shared platform module —
    # the same code path docs/install.sh uses, so agents are no longer Claude-only.
    if [[ -n "${PLATFORMS// /}" ]]; then
        local id
        for id in $PLATFORMS; do configure_platform "$id" "$abs_bin"; done
    fi

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

# Run main when executed, but NOT when sourced for tests (tests/test_setup.sh sets the
# sentinel to load the functions only).
if [[ -z "${SAI_SETUP_SH_NO_MAIN:-}" ]]; then
    main "$@"
fi
