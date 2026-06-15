#!/usr/bin/env bash
#
# Tests for the semanticastindexer MCP setup tooling:
#   - shared lib snippet generators (lib/mcp-config.sh)
#   - install.sh <-> lib snippet PARITY (the curl-piped installer can't source the lib,
#     so it keeps an inline copy — this asserts the two never drift)
#   - setup.sh single-source config generation (copies+patches templates/sai-cfg.yml)
#   - generated MCP example artifacts
#   - skill + subagent install paths (into a temp HOME)
#   - the Rust sai_prepare_mcp_setup correctness fixes (structural guards)
#
# Run: bash mcp-setup/tests/test_setup.sh   (no build required)
#
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
LIB="$REPO_ROOT/mcp-setup/lib/mcp-config.sh"
SETUP="$REPO_ROOT/mcp-setup/setup.sh"
INSTALL="$REPO_ROOT/docs/install.sh"
MCP_RS="$REPO_ROOT/src/mcp.rs"

PASS=0; FAIL=0
ok()  { PASS=$((PASS+1)); printf '  ok   %s\n' "$1"; }
bad() { FAIL=$((FAIL+1)); printf '  FAIL %s\n' "$1"; }
has() { case "$1" in *"$2"*) ok "$3" ;; *) bad "$3 (missing: $2)" ;; esac; }
hasnt() { case "$1" in *"$2"*) bad "$3 (should not contain: $2)" ;; *) ok "$3" ;; esac; }
eq() { if [ "$1" = "$2" ]; then ok "$3"; else bad "$3"; fi; }
isfile() { if [ -f "$1" ]; then ok "$2"; else bad "$2 (no file: $1)"; fi; }

# Emit the four shared snippets with fixed inputs from a sourced script, so the lib and
# install.sh can be compared byte-for-byte. Sources in a subshell; overrides the contract
# vars AFTER sourcing (install.sh sets its own PROJECT_DIR=pwd at load time).
emit_snippets() { # $1 = file to source
  local f="$1"
  (
    set --   # clear positional args so the sourced script's CLI parser sees nothing
    log(){ :; }; success(){ :; }; warn(){ :; }; error(){ :; }
    # shellcheck disable=SC1090
    . "$f" >/dev/null 2>&1
    set +eu +o pipefail
    SERVER_NAME=sai; PROJECT_DIR=/proj
    mcp_args_json; echo
    json_snippet /bin/sai
    toml_snippet /bin/sai
    yaml_snippet /bin/sai
  )
}

echo "== shared lib snippet generators =="
LIB_OUT="$(emit_snippets "$LIB")"
has "$LIB_OUT" '"sai"'                    "json: server key"
has "$LIB_OUT" '"command": "/bin/sai"'    "json: command path"
has "$LIB_OUT" '"cwd": "/proj"'           "json: cwd"
has "$LIB_OUT" '"--config", "sai-cfg.yml"' "json: args"
has "$LIB_OUT" '[mcp_servers.sai]'        "toml: table header"
has "$LIB_OUT" 'name: sai'               "yaml: server name"

echo "== install.sh <-> lib snippet parity (drift guard) =="
INSTALL_OUT="$(SAI_INSTALL_SH_NO_MAIN=1 emit_snippets "$INSTALL")"
if [ -z "$INSTALL_OUT" ]; then
  bad "install.sh produced no snippet output (source failed?)"
else
  eq "$LIB_OUT" "$INSTALL_OUT" "install.sh generators match lib byte-for-byte"
fi

echo "== setup.sh single-source config + artifacts =="
export SAI_SETUP_SH_NO_MAIN=1
set --   # clear positional args so setup.sh's CLI parser sees nothing
# shellcheck disable=SC1090
. "$SETUP"
set +eu +o pipefail   # restore lenient mode for assertions (setup.sh enables set -e)

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# Default flags: should reproduce the canonical template (duckdb/ort/jina-768 + tuned thresholds)
BACKEND=duckdb; EMBEDDER=ort; COLLECTION=source_code; NON_INTERACTIVE=true
PROJECT_DIR="$TMP/default"; mkdir -p "$PROJECT_DIR"
create_indexer_config "$PROJECT_DIR" >/dev/null 2>&1
CFG="$(cat "$PROJECT_DIR/sai-cfg.yml" 2>/dev/null || true)"
has "$CFG" 'backend: duckdb'                            "config(default): backend"
has "$CFG" 'embedder: ort'                              "config(default): embedder"
has "$CFG" 'collection: source_code'                    "config(default): collection"
has "$CFG" 'model: jinaai/jina-embeddings-v2-base-code' "config(default): canonical model (from template)"
has "$CFG" 'duplicate_min_score: 0.88'                  "config(default): tuned thresholds (from template)"

# Patched flags: backend/embedder/collection follow the chosen values
BACKEND=qdrant; EMBEDDER=ollama; COLLECTION=mycode
PROJECT_DIR="$TMP/patched"; mkdir -p "$PROJECT_DIR"
create_indexer_config "$PROJECT_DIR" >/dev/null 2>&1
CFG2="$(cat "$PROJECT_DIR/sai-cfg.yml" 2>/dev/null || true)"
has "$CFG2" 'backend: qdrant'    "config(patched): backend"
has "$CFG2" 'embedder: ollama'   "config(patched): embedder"
has "$CFG2" 'collection: mycode' "config(patched): collection"

# MCP example artifacts come from the shared json_snippet
PROJECT_DIR="$TMP/default"
generate_mcp_configs /bin/sai "$PROJECT_DIR" >/dev/null 2>&1
isfile "$PROJECT_DIR/.mcp.json.example"                 "artifact: .mcp.json.example"
isfile "$PROJECT_DIR/claude-desktop-config.example.json" "artifact: claude-desktop-config.example.json"
if command -v python3 >/dev/null 2>&1; then
  if python3 -m json.tool "$PROJECT_DIR/.mcp.json.example" >/dev/null 2>&1; then ok "artifact: valid JSON"; else bad "artifact: invalid JSON"; fi
fi
has "$(cat "$PROJECT_DIR/.mcp.json.example")" '/bin/sai' "artifact: embeds binary path"

echo "== skill + subagent install (temp HOME) =="
(
  export HOME="$TMP/home"; unset _SAI_SKILLS_INSTALLED
  install_agent_skill >/dev/null 2>&1
  install_claude_agents >/dev/null 2>&1
  install_agent_skill >/dev/null 2>&1   # idempotent second call must not error
)
isfile "$TMP/home/.claude/skills/sai/SKILL.md"          "install: sai skill"
isfile "$TMP/home/.claude/skills/sai-deslop/SKILL.md"   "install: sai-deslop skill"
isfile "$TMP/home/.claude/agents/dedup-auditor.md"      "install: dedup-auditor subagent"

echo "== Rust sai_prepare_mcp_setup fixes (structural) =="
RS="$(cat "$MCP_RS")"
has   "$RS" '--target-dir'              "rust: recommended_command includes --target-dir"
hasnt "$RS" '_features_str'             "rust: dead _features_str removed"
has   "$RS" 'from_source'               "rust: release-path fallback flag present"
hasnt "$RS" 'mcp,duckdb,ollama,ast'     "rust: hardcoded feature string removed"

echo
echo "== $PASS passed, $FAIL failed =="
[ "$FAIL" -eq 0 ]
