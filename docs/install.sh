#!/usr/bin/env bash
#
# semanticastindexer one-line installer.
#
#   curl -fsSL https://maadgrom.github.io/semanticastindexer/install.sh | bash
#
# Downloads a prebuilt binary from the latest GitHub Release (no Rust toolchain
# required). Then, unless you pass --platform/--all/--non-interactive, it ASKS which
# coding agent(s) to connect (reading your keypress from the terminal, so it works even
# under `curl | bash`). For JSON-based clients, pass --write to merge the config for you;
# otherwise it prints the snippet and the exact file path.
#
set -euo pipefail

# --- Repo constants ---
OWNER="maadgrom"
REPO="semanticastindexer"
BINARY_NAME="semanticastindexer"
SERVER_NAME="sai"
RELEASE_INSTALLER="https://github.com/${OWNER}/${REPO}/releases/latest/download/${BINARY_NAME}-installer.sh"
RAW_BASE="https://raw.githubusercontent.com/${OWNER}/${REPO}/main"
PAGES_URL="https://${OWNER}.github.io/${REPO}/"
ALL_AGENTS="claude-code claude-desktop cursor windsurf continue codex hermes"

# Colors (disabled when not a tty)
if [ -t 1 ]; then
    RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; BLUE='\033[0;34m'; BOLD='\033[1m'; NC='\033[0m'
else
    RED=''; GREEN=''; YELLOW=''; BLUE=''; BOLD=''; NC=''
fi
log()     { printf '%b[install]%b %s\n' "$BLUE" "$NC" "$*"; }
success() { printf '%b[install]%b %s\n' "$GREEN" "$NC" "$*"; }
warn()    { printf '%b[install]%b %s\n' "$YELLOW" "$NC" "$*" >&2; }
error()   { printf '%b[install]%b %s\n' "$RED" "$NC" "$*" >&2; }

print_help() {
    cat <<EOF
semanticastindexer installer

Usage:
  install.sh [options]

By default the binary is installed, then you are asked which coding agent(s) to connect.

Agent selection (skip the prompt):
  --platform <id>      Connect one client non-interactively
  --all                Connect every supported client
  --non-interactive    Don't prompt; just install the binary + print a generic MCP block

Supported platform ids:
  claude-code  claude-desktop  cursor  windsurf  continue  codex  hermes  generic

Other options:
  --write              Merge config into the client's file (JSON clients only; best-effort, backs up)
  --skip-binary        Don't install the binary (just emit config)
  --help, -h           Show this help

Examples:
  curl -fsSL ${PAGES_URL}install.sh | bash                              # install + interactive picker
  curl -fsSL ${PAGES_URL}install.sh | bash -s -- --platform claude-code # one client
  curl -fsSL ${PAGES_URL}install.sh | bash -s -- --all --write          # every client, merged
EOF
}

# --- Args ---
PLATFORM=""
PLATFORM_SET=false
ALL=false
NON_INTERACTIVE=false
WRITE=false
SKIP_BINARY=false
while [ $# -gt 0 ]; do
    case "$1" in
        --platform)   PLATFORM="${2:-}"; PLATFORM_SET=true; shift 2 ;;
        --all)        ALL=true; shift ;;
        --non-interactive) NON_INTERACTIVE=true; shift ;;
        --write)      WRITE=true; shift ;;
        --skip-binary) SKIP_BINARY=true; shift ;;
        --help|-h)    print_help; exit 0 ;;
        *) error "Unknown argument: $1"; echo; print_help; exit 1 ;;
    esac
done

PROJECT_DIR="$(pwd)"

# --- Install the prebuilt binary via the cargo-dist release installer ---
install_binary() {
    if [ "$SKIP_BINARY" = true ]; then
        warn "Skipping binary install (--skip-binary)."
        return
    fi
    if ! command -v curl >/dev/null 2>&1; then
        error "curl is required to download the binary."; exit 1
    fi
    log "Downloading the latest prebuilt ${BINARY_NAME} binary..."
    if ! curl -fsSL "$RELEASE_INSTALLER" | sh; then
        error "Could not run the release installer."
        error "No release yet? Build from source instead — see https://maadgrom.github.io/semanticastindexer/book/installation.html"
        exit 1
    fi
    success "Binary installed."
}

# Resolve the absolute path to the installed binary: PATH lookup first, then the
# cargo-dist install dirs (~/.cargo/bin, ~/.local/bin). If nothing exists on disk,
# warn and default to the ~/.cargo/bin path.
resolve_binary() {
    local candidates=(
        "$(command -v "$BINARY_NAME" 2>/dev/null || true)"
        "$HOME/.cargo/bin/$BINARY_NAME"
        "$HOME/.local/bin/$BINARY_NAME"
    )
    for c in "${candidates[@]}"; do
        if [ -n "$c" ] && [ -x "$c" ]; then printf '%s' "$c"; return; fi
    done
    # Nothing found on disk — warn instead of silently printing a dead path (the MCP
    # config would point at a binary that does not exist).
    error "could not locate the installed binary; defaulting to ~/.cargo/bin/$BINARY_NAME"
    printf '%s' "$HOME/.cargo/bin/$BINARY_NAME"
}

# --- Config snippet builders ($1=binary path) ---
mcp_args_json() { printf '["mcp", "--config", "sai-cfg.yml"]'; }

json_snippet() {
    cat <<EOF
{
  "mcpServers": {
    "${SERVER_NAME}": {
      "command": "$1",
      "args": $(mcp_args_json),
      "cwd": "${PROJECT_DIR}"
    }
  }
}
EOF
}

toml_snippet() {
    cat <<EOF
[mcp_servers.${SERVER_NAME}]
command = "$1"
args = ["mcp", "--config", "sai-cfg.yml"]
cwd = "${PROJECT_DIR}"
EOF
}

yaml_snippet() {
    cat <<EOF
mcpServers:
  - name: ${SERVER_NAME}
    command: "$1"
    args: ["mcp", "--config", "sai-cfg.yml"]
    cwd: "${PROJECT_DIR}"
EOF
}

# Best-effort merge of the JSON snippet into an existing JSON config (python3); else print.
# $1 = target path, $2 = binary path
write_json_config() {
    local target="$1" bin="$2"
    if ! command -v python3 >/dev/null 2>&1; then
        warn "python3 not found — printing snippet instead of merging."
        print_block "$target" "$(json_snippet "$bin")"
        return
    fi
    mkdir -p "$(dirname "$target")"
    [ -f "$target" ] && cp "$target" "${target}.bak" && log "Backed up existing config to ${target}.bak"
    SERVER_NAME="$SERVER_NAME" BIN="$bin" CWD="$PROJECT_DIR" \
    python3 - "$target" <<'PY'
import json, os, sys
target = sys.argv[1]
try:
    with open(target) as f:
        cfg = json.load(f)
except (FileNotFoundError, json.JSONDecodeError):
    cfg = {}
cfg.setdefault("mcpServers", {})
cfg["mcpServers"][os.environ["SERVER_NAME"]] = {
    "command": os.environ["BIN"],
    "args": ["mcp", "--config", "sai-cfg.yml"],
    "cwd": os.environ["CWD"],
}
with open(target, "w") as f:
    json.dump(cfg, f, indent=2)
    f.write("\n")
print(f"[install] merged MCP server '{os.environ['SERVER_NAME']}' into {target}")
PY
    success "Wrote $target"
}

print_block() { # $1 = where it goes, $2 = content
    echo
    printf '%bAdd this to %s:%b\n' "$BOLD" "$1" "$NC"
    echo "----------------------------------------------------------------------"
    printf '%s\n' "$2"
    echo "----------------------------------------------------------------------"
}

# Agent skills are PORTABLE (Claude Code, Cursor, Windsurf, …). Claude Code auto-loads
# them from ~/.claude/skills/, so for that client we drop copies there; other clients
# read the same SKILL.md files from the repo's .agents/skills/ tree. Also delivers the
# Claude Code `dedup-auditor` subagent. All best-effort (warns, never fails the install).
install_agent_skill() {
    # Upgrade cleanup: remove old-named artifacts so upgraders are not stranded.
    if [ -e "$HOME/.local/bin/code-search-mcp" ]; then
        rm -f "$HOME/.local/bin/code-search-mcp"
        success "Removed old wrapper $HOME/.local/bin/code-search-mcp"
    fi
    if [ -d "$HOME/.claude/skills/semantic-code-search-mcp" ]; then
        rm -rf "$HOME/.claude/skills/semantic-code-search-mcp"
        success "Removed old skill dir $HOME/.claude/skills/semantic-code-search-mcp"
    fi

    local skill
    for skill in sai sai-deslop; do
        local skill_dir="$HOME/.claude/skills/$skill"
        mkdir -p "$skill_dir"
        if curl -fsSL "${RAW_BASE}/.agents/skills/${skill}/SKILL.md" -o "${skill_dir}/SKILL.md"; then
            success "Installed skill → ${skill_dir}/SKILL.md"
        else
            warn "Could not download ${skill} SKILL.md (offline, or not yet on main?). Skipping."
        fi
    done

    local agent_dir="$HOME/.claude/agents"
    mkdir -p "$agent_dir"
    if curl -fsSL "${RAW_BASE}/.claude/agents/dedup-auditor.md" -o "${agent_dir}/dedup-auditor.md"; then
        success "Installed subagent → ${agent_dir}/dedup-auditor.md"
    else
        warn "Could not download dedup-auditor subagent (offline, or not yet on main?). Skipping."
    fi
}

desktop_config_path() {
    case "$(uname -s)" in
        Darwin) printf '%s' "$HOME/Library/Application Support/Claude/claude_desktop_config.json" ;;
        Linux)  printf '%s' "$HOME/.config/Claude/claude_desktop_config.json" ;;
        *)      printf '%s' "${APPDATA:-$HOME}/Claude/claude_desktop_config.json" ;;
    esac
}

# Wire up one client. $1 = platform id, $2 = binary path.
configure_platform() {
    local id="$1" bin="$2"
    printf '\n%b• %s%b\n' "$BOLD" "$id" "$NC"
    case "$id" in
        claude-code)
            install_agent_skill
            if [ "$WRITE" = true ]; then
                # Prefer the official Claude Code CLI (writes a project-scoped .mcp.json);
                # fall back to a hand-merged JSON config if `claude` is absent or errors.
                if command -v claude >/dev/null 2>&1 \
                    && ( cd "$PROJECT_DIR" && claude mcp add sai --scope project --transport stdio -- "$bin" mcp --config sai-cfg.yml ) >/dev/null 2>&1; then
                    success "Registered 'sai' via 'claude mcp add' → ${PROJECT_DIR}/.mcp.json"
                else
                    write_json_config "${PROJECT_DIR}/.mcp.json" "$bin"
                fi
            else
                print_block "${PROJECT_DIR}/.mcp.json (in the project you want to search)" "$(json_snippet "$bin")"
                command -v claude >/dev/null 2>&1 && log "Tip: re-run with --write to register automatically via 'claude mcp add'."
            fi ;;
        claude-desktop)
            local p; p="$(desktop_config_path)"
            if [ "$WRITE" = true ]; then write_json_config "$p" "$bin"; else print_block "$p" "$(json_snippet "$bin")"; fi ;;
        cursor)
            if [ "$WRITE" = true ]; then write_json_config "$HOME/.cursor/mcp.json" "$bin"; else print_block "$HOME/.cursor/mcp.json" "$(json_snippet "$bin")"; fi ;;
        windsurf)
            if [ "$WRITE" = true ]; then write_json_config "$HOME/.codeium/windsurf/mcp_config.json" "$bin"; else print_block "$HOME/.codeium/windsurf/mcp_config.json" "$(json_snippet "$bin")"; fi ;;
        continue)
            print_block "$HOME/.continue/config.yaml" "$(yaml_snippet "$bin")"
            [ "$WRITE" = true ] && warn "--write supports JSON clients only; paste the YAML above into Continue's config." ;;
        codex)
            print_block "$HOME/.codex/config.toml" "$(toml_snippet "$bin")"
            [ "$WRITE" = true ] && warn "--write supports JSON clients only; paste the TOML above into ~/.codex/config.toml." ;;
        hermes)
            warn "Hermes config location is client-specific — paste this generic MCP block into its MCP config."
            print_block "your Hermes MCP config" "$(json_snippet "$bin")" ;;
        generic)
            print_block "your MCP client config" "$(json_snippet "$bin")" ;;
        *)
            warn "Unknown platform '$id' — skipping." ;;
    esac
    return 0
}

# Interactive multi-select, reading from the controlling terminal (works under curl | bash).
# Echoes the chosen space-separated platform ids (empty = skip).
prompt_agents() {
    local choice
    {
        echo ""
        echo "Which coding agent(s) should I connect? (the binary works as a CLI regardless)"
        echo ""
        echo "   1) Claude Code       4) Windsurf        7) Hermes"
        echo "   2) Claude Desktop    5) Continue.dev    8) Generic / manual"
        echo "   3) Cursor            6) Codex CLI"
        echo ""
        printf "Enter numbers (e.g. 1 3), 'all', or press Enter to skip: "
    } >/dev/tty
    read -r choice </dev/tty || choice=""

    case "$choice" in
        ""|n|N|no|none|skip) printf '' ; return ;;
        all|a|A|ALL) printf '%s' "$ALL_AGENTS" ; return ;;
    esac

    local out=""
    for tok in $choice; do
        case "$tok" in
            1) out="$out claude-code" ;;
            2) out="$out claude-desktop" ;;
            3) out="$out cursor" ;;
            4) out="$out windsurf" ;;
            5) out="$out continue" ;;
            6) out="$out codex" ;;
            7) out="$out hermes" ;;
            8) out="$out generic" ;;
            *) printf "Ignoring unknown choice: %s\n" "$tok" >/dev/tty ;;
        esac
    done
    printf '%s' "$out"
}

# --- Main ---
main() {
    printf '\n%bsemanticastindexer installer%b\n\n' "$BOLD" "$NC"
    install_binary
    local BIN; BIN="$(resolve_binary)"
    log "Binary: $BIN"

    # Decide which clients to wire up.
    local targets=""
    if [ "$PLATFORM_SET" = true ]; then
        targets="$PLATFORM"
    elif [ "$ALL" = true ]; then
        targets="$ALL_AGENTS"
    elif [ "$NON_INTERACTIVE" = true ] || [ ! -r /dev/tty ]; then
        targets="generic"
    else
        targets="$(prompt_agents)"
    fi

    if [ -z "$targets" ]; then
        log "No client selected. The binary is installed; re-run with --platform <id> or --all to wire one up."
    else
        local id
        for id in $targets; do configure_platform "$id" "$BIN"; done
    fi

    echo
    success "Done. Next steps:"
    # Human-facing commands use the bare name — the absolute path only belongs in the
    # MCP config snippets. If the install dir is not on this shell's PATH yet (the
    # installer edits your shell rc), say so first instead of printing a long path.
    local step=1
    if ! command -v "$BINARY_NAME" >/dev/null 2>&1; then
        echo "  $step. Open a new terminal (or source your shell rc) so '$BINARY_NAME' is on your PATH ($(dirname "$BIN"))"
        step=$((step + 1))
    fi
    echo "  $step. cd into the project you want to search"
    step=$((step + 1))
    echo "  $step. $BINARY_NAME --root src --ext ts,tsx --dry-run   # preview what gets indexed"
    step=$((step + 1))
    echo "  $step. $BINARY_NAME --root src --ext ts,tsx             # index it"
    step=$((step + 1))
    echo "  $step. Restart your client so it picks up the MCP server"
    echo
    echo "  Docs: ${PAGES_URL}"
}

# Run main when executed or piped (curl | bash), but NOT when sourced for tests
# (the drift test sets SAI_INSTALL_SH_NO_MAIN=1 to load the snippet generators only).
if [ -z "${SAI_INSTALL_SH_NO_MAIN:-}" ]; then
    main "$@"
fi
