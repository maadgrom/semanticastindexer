#!/usr/bin/env bash
#
# semanticastindexer uninstaller.
#
#   curl -fsSL https://maadgrom.github.io/semanticastindexer/uninstall.sh | bash
#
# Reverses what install.sh did: removes the binary, the Claude Code skill, and the
# `semantic-code-search` MCP server entry from known JSON client configs. Per-project
# index files (.index/) and any sai-cfg.yml are left untouched (delete them yourself).
#
set -euo pipefail

BINARY_NAME="semanticastindexer"
SERVER_NAME="semantic-code-search"
SKILL_DIR="$HOME/.claude/skills/semantic-code-search-mcp"

if [ -t 1 ]; then
    RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; BLUE='\033[0;34m'; BOLD='\033[1m'; NC='\033[0m'
else
    RED=''; GREEN=''; YELLOW=''; BLUE=''; BOLD=''; NC=''
fi
log()     { printf '%b[uninstall]%b %s\n' "$BLUE" "$NC" "$*"; }
success() { printf '%b[uninstall]%b %s\n' "$GREEN" "$NC" "$*"; }
warn()    { printf '%b[uninstall]%b %s\n' "$YELLOW" "$NC" "$*" >&2; }

ASSUME_YES=false
while [ $# -gt 0 ]; do
    case "$1" in
        --yes|-y) ASSUME_YES=true; shift ;;
        --help|-h)
            cat <<EOF
semanticastindexer uninstaller

Usage: uninstall.sh [--yes]

Removes:
  - the $BINARY_NAME binary from ~/.cargo/bin and ~/.local/bin (+ code-search-mcp wrapper)
  - the skill at $SKILL_DIR
  - the "$SERVER_NAME" entry from known JSON MCP configs (Claude Desktop, Cursor, Windsurf,
    and ./.mcp.json), each backed up to <file>.bak

Leaves alone (remove manually if you want):
  - per-project index files (.index/) and sai-cfg.yml
  - Codex (~/.codex/config.toml) and Continue (~/.continue/config.yaml) entries
  - any PATH line the install added to your shell rc

  --yes, -y   Don't prompt for confirmation
EOF
            exit 0 ;;
        *) warn "Unknown argument: $1"; exit 1 ;;
    esac
done

desktop_config_path() {
    case "$(uname -s)" in
        Darwin) printf '%s' "$HOME/Library/Application Support/Claude/claude_desktop_config.json" ;;
        Linux)  printf '%s' "$HOME/.config/Claude/claude_desktop_config.json" ;;
        *)      printf '%s' "${APPDATA:-$HOME}/Claude/claude_desktop_config.json" ;;
    esac
}

confirm() {
    [ "$ASSUME_YES" = true ] && return 0
    [ ! -r /dev/tty ] && return 0   # non-interactive (CI): proceed
    printf '%bRemove the %s binary, skill, and MCP config entries? [y/N] %b' "$BOLD" "$BINARY_NAME" "$NC" >/dev/tty
    local ans; read -r ans </dev/tty || ans=""
    case "$ans" in y|Y|yes|YES) return 0 ;; *) log "Aborted."; exit 0 ;; esac
}

remove_binary() {
    local found=false
    for dir in "$HOME/.cargo/bin" "$HOME/.local/bin"; do
        if [ -e "$dir/$BINARY_NAME" ]; then rm -f "$dir/$BINARY_NAME" && success "Removed $dir/$BINARY_NAME"; found=true; fi
        if [ -e "$dir/code-search-mcp" ]; then rm -f "$dir/code-search-mcp" && success "Removed $dir/code-search-mcp"; fi
    done
    [ "$found" = false ] && warn "No $BINARY_NAME binary found in ~/.cargo/bin or ~/.local/bin."
    return 0
}

remove_skill() {
    if [ -d "$SKILL_DIR" ]; then rm -rf "$SKILL_DIR" && success "Removed skill $SKILL_DIR"; else log "No skill directory at $SKILL_DIR."; fi
}

# Remove the server entry from one JSON config (best-effort, backed up).
remove_from_json() {
    local target="$1"
    [ -f "$target" ] || return 0
    if ! command -v python3 >/dev/null 2>&1; then
        warn "python3 not found - remove \"$SERVER_NAME\" from $target by hand."
        return 0
    fi
    SERVER_NAME="$SERVER_NAME" python3 - "$target" <<'PY'
import json, os, sys, shutil
target = sys.argv[1]
name = os.environ["SERVER_NAME"]
try:
    with open(target) as f:
        cfg = json.load(f)
except (FileNotFoundError, json.JSONDecodeError):
    sys.exit(0)
servers = cfg.get("mcpServers")
if isinstance(servers, dict) and name in servers:
    shutil.copyfile(target, target + ".bak")
    del servers[name]
    with open(target, "w") as f:
        json.dump(cfg, f, indent=2)
        f.write("\n")
    print(f"[uninstall] removed '{name}' from {target} (backup: {target}.bak)")
PY
}

main() {
    printf '\n%bsemanticastindexer uninstaller%b\n\n' "$BOLD" "$NC"
    confirm
    remove_binary
    remove_skill
    log "Cleaning MCP server entries..."
    remove_from_json "$(desktop_config_path)"
    remove_from_json "$HOME/.cursor/mcp.json"
    remove_from_json "$HOME/.codeium/windsurf/mcp_config.json"
    remove_from_json "$(pwd)/.mcp.json"

    echo
    success "Done."
    echo "Left in place (remove manually if you want):"
    echo "  - per-project index: .index/  and any sai-cfg.yml"
    echo "  - Codex:   ~/.codex/config.toml  ([mcp_servers.$SERVER_NAME])"
    echo "  - Continue: ~/.continue/config.yaml"
    echo "  - any PATH line the installer added to your shell rc (~/.zshrc, ~/.bashrc, ~/.profile)"
}

main "$@"
