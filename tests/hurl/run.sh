#!/usr/bin/env bash
# HURL integration harness for the MCP streamable-HTTP transport (`sai mcp --http`).
#
# Builds a tiny DuckDB index over a fixed fixture (a known near-duplicate pair + a distinct
# file), then runs hurl in TWO sequential phases against the SAME index (DuckDB is
# single-process, so the servers must NOT run concurrently):
#   Phase A — a --allow-write (RW) server: every .hurl EXCEPT readonly.hurl.
#   Phase B — a read-only (RO) server:     readonly.hurl (the write-tool refusal cases).
# Both phases bind {{base}} to that phase's server URL.
#
# Assertions are model-version-stable: structural shape + cluster MEMBERSHIP (paths) +
# threshold MONOTONICITY only — never exact embedding scores.
#
#   bash tests/hurl/run.sh                       # build (if needed) + run
#   SAI_BIN=target/release/...  bash tests/hurl/run.sh
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BIN="${SAI_BIN:-$ROOT/target/debug/semanticastindexer}"
# The harness `cd`s into the (temp) fixture before invoking the binary, so a RELATIVE bin
# path — e.g. CI's `SAI_BIN=target/debug/semanticastindexer` — would not resolve from there.
# Absolutize it against the repo root (where a relative SAI_BIN is meant to be relative to).
case "$BIN" in /*) ;; *) BIN="$ROOT/$BIN" ;; esac
if [ ! -x "$BIN" ]; then
  echo "building $BIN (cargo build --features all)…"
  ( cd "$ROOT" && cargo build --features all ) || exit 1
fi
command -v hurl >/dev/null 2>&1 || { echo "ERROR: hurl is not installed (see https://hurl.dev)"; exit 127; }

FIX="$(mktemp -d)"
PID=""
cleanup() { [ -n "$PID" ] && kill "$PID" 2>/dev/null; wait 2>/dev/null || true; rm -rf "$FIX"; }
trap cleanup EXIT

# --- fixture: a near-identical pair (alpha/beta) + a distinct file (gamma) ---
mkdir -p "$FIX/src"
printf 'export function computeTotal(items){let s=0;for(const i of items)s+=i.price;return s;}\n' > "$FIX/src/alpha.ts"
printf 'export function computeSum(list){let s=0;for(const i of list)s+=i.price;return s;}\n'    > "$FIX/src/beta.ts"
printf 'export const greet=(name)=>`hello ${name}`;\n'                                            > "$FIX/src/gamma.ts"
cat > "$FIX/sai-cfg.yml" <<'CFG'
backend: duckdb
embedder: ort
prefix_style: e5
collection: hurl_test
model: intfloat/multilingual-e5-small
vector_dim: 384
duckdb:
  path: .index/code.duckdb
  model_repo: Xenova/multilingual-e5-small
ext: [ts]
root: src
CFG
# Identity is passed per-commit (`-c …`) so the harness never depends on a global git
# identity — CI runners have none, and the empty second commit would otherwise abort.
( cd "$FIX" && git init -q && git add -A \
    && git -c user.email=hurl@test -c user.name=hurl commit -qm init \
    && git -c user.email=hurl@test -c user.name=hurl commit -q --allow-empty -m second ) \
  || { echo "git fixture setup failed"; exit 1; }

echo "indexing fixture (duckdb + e5-small)…"
( cd "$FIX" && "$BIN" --config sai-cfg.yml >/dev/null ) || { echo "index failed"; exit 1; }

LOG="$FIX/srv.log"
URL=""
# Start a server (extra flags in $@), wait until it logs its ephemeral port AND answers
# `initialize` with 200. Sets the global URL. Returns 1 (and dumps the log) on failure.
start_and_wait() {
  : > "$LOG"
  ( cd "$FIX" && exec "$BIN" mcp --config sai-cfg.yml --http 127.0.0.1:0 "$@" >/dev/null 2>"$LOG" ) &
  PID=$!
  local addr="" i
  for i in $(seq 1 100); do
    addr="$(grep -oE '127\.0\.0\.1:[0-9]+' "$LOG" | head -1 || true)"
    [ -n "$addr" ] && break
    kill -0 "$PID" 2>/dev/null || { echo "server exited during startup; log:"; cat "$LOG"; return 1; }
    sleep 0.2
  done
  [ -z "$addr" ] && { echo "server never logged a bound address; log:"; cat "$LOG"; return 1; }
  URL="http://$addr/mcp"
  for i in $(seq 1 100); do
    [ "$(curl -s -o /dev/null -w '%{http_code}' \
          -H 'Content-Type: application/json' -H 'Accept: application/json, text/event-stream' \
          -d '{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"hurl","version":"1"}}}' \
          "$URL" 2>/dev/null)" = "200" ] && return 0
    sleep 0.2
  done
  echo "server at $URL never became ready"; return 1
}
stop_server() { [ -n "$PID" ] && kill "$PID" 2>/dev/null; wait "$PID" 2>/dev/null || true; PID=""; }

# --- Phase A: read-write server — everything except readonly.hurl ---
# --retry absorbs rare transient errors against the freshly-started live server: queries
# are deterministic against a built index (verified stable same-index), so a retry on the
# same index succeeds; a genuine assertion failure still fails after the retries.
HURL_RETRY=(--retry 3 --retry-interval 1000)
start_and_wait --allow-write || exit 1
echo "RW server ready at $URL"
hurl --test "${HURL_RETRY[@]}" --variable "base=$URL" \
  "$ROOT"/tests/hurl/protocol.hurl \
  "$ROOT"/tests/hurl/status_duplicates.hurl \
  "$ROOT"/tests/hurl/search_similar.hurl \
  "$ROOT"/tests/hurl/write.hurl \
  "$ROOT"/tests/hurl/setup_security.hurl
RC_A=$?
stop_server

# --- Phase B: read-only server — the write-tool refusal cases ---
start_and_wait || exit 1
echo "RO server ready at $URL"
hurl --test "${HURL_RETRY[@]}" --variable "base=$URL" "$ROOT"/tests/hurl/readonly.hurl
RC_B=$?
stop_server

[ "$RC_A" -eq 0 ] && [ "$RC_B" -eq 0 ]
