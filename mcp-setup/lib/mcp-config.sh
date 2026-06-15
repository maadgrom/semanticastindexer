# shellcheck shell=bash
#
# mcp-config.sh — shared MCP client-wiring library for semanticastindexer.
#
# CANONICAL SOURCE for the MCP config snippets and per-client wiring. It is sourced
# directly by `mcp-setup/setup.sh` (which always runs from a checkout). The one-line
# installer `docs/install.sh` is distributed via `curl | bash` and therefore CANNOT
# source a sibling file — it keeps a byte-identical inline copy of these generators,
# and `mcp-setup/tests/test_setup.sh` asserts the two never drift.
#
# Sourcing this file has NO side effects (defines functions only).
#
# CONTRACT — the caller must provide before invoking these functions:
#   - SERVER_NAME   : MCP server key (e.g. "sai")
#   - PROJECT_DIR   : project root that becomes the server's `cwd`
#   - WRITE         : "true" | "false" — merge into client config vs. print snippet
#   - BOLD, NC      : terminal style escapes (may be empty)
#   - log/success/warn/error : logging helpers
# Optionally:
#   - install_agent_skill : if defined, configure_platform calls it for `claude-code`
#                            so each script controls how the skill is delivered
#                            (local copy vs. remote download).

# Args passed to the server binary in every client config.
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

desktop_config_path() {
    case "$(uname -s)" in
        Darwin) printf '%s' "$HOME/Library/Application Support/Claude/claude_desktop_config.json" ;;
        Linux)  printf '%s' "$HOME/.config/Claude/claude_desktop_config.json" ;;
        *)      printf '%s' "${APPDATA:-$HOME}/Claude/claude_desktop_config.json" ;;
    esac
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
print(f"[setup] merged MCP server '{os.environ['SERVER_NAME']}' into {target}")
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

# Wire up one client. $1 = platform id, $2 = binary path.
configure_platform() {
    local id="$1" bin="$2"
    printf '\n%b• %s%b\n' "$BOLD" "$id" "$NC"
    case "$id" in
        claude-code)
            # Each caller decides how the agent skill is delivered (local copy vs. remote).
            command -v install_agent_skill >/dev/null 2>&1 && install_agent_skill
            if [ "$WRITE" = true ]; then
                # Prefer the official Claude Code CLI (writes a project-scoped .mcp.json);
                # fall back to a hand-merged JSON config if `claude` is absent or errors.
                if command -v claude >/dev/null 2>&1 \
                    && ( cd "$PROJECT_DIR" && claude mcp add "$SERVER_NAME" --scope project --transport stdio -- "$bin" mcp --config sai-cfg.yml ) >/dev/null 2>&1; then
                    success "Registered '$SERVER_NAME' via 'claude mcp add' → ${PROJECT_DIR}/.mcp.json"
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
