# Write tools against the --allow-write (RW) server: sai_refresh + sai_sync success/edge.

# --- refresh an existing file → success, refreshed list non-empty ---
POST {{base}}
Content-Type: application/json
Accept: application/json, text/event-stream
{"jsonrpc":"2.0","id":30,"method":"tools/call","params":{"name":"sai_refresh","arguments":{"paths":["src/alpha.ts"]}}}
HTTP 200
[Asserts]
jsonpath "$.result.isError" == false
jsonpath "$.result.structuredContent.refreshed" count >= 1

# --- refresh with empty paths → invalid params -32602 ---
POST {{base}}
Content-Type: application/json
Accept: application/json, text/event-stream
{"jsonrpc":"2.0","id":31,"method":"tools/call","params":{"name":"sai_refresh","arguments":{"paths":[]}}}
HTTP 200
[Asserts]
jsonpath "$.error.code" == -32602

# --- sync with a clean tree (default since HEAD~1, no diff) → success, "no changed files" ---
POST {{base}}
Content-Type: application/json
Accept: application/json, text/event-stream
{"jsonrpc":"2.0","id":32,"method":"tools/call","params":{"name":"sai_sync","arguments":{}}}
HTTP 200
[Asserts]
jsonpath "$.result.isError" == false
jsonpath "$.result.structuredContent.note" exists
