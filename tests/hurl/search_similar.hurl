# sai_search_code (semantic search + filters) and sai_find_similar (by location / by code /
# error branches). Membership + shape only — no exact scores.

# --- search_code: query hits the compute-* functions; alpha is among them ---
POST {{base}}
Content-Type: application/json
Accept: application/json, text/event-stream
{"jsonrpc":"2.0","id":20,"method":"tools/call","params":{"name":"sai_search_code","arguments":{"query":"compute total price","limit":3}}}
HTTP 200
[Asserts]
jsonpath "$.result.isError" == false
jsonpath "$.result.structuredContent.hits" count >= 1
jsonpath "$.result.structuredContent.hits[*].path" includes "src/alpha.ts"
jsonpath "$.result.structuredContent.hits[0].snippet" exists

# --- search_code with a path_glob that matches nothing → empty hits ---
POST {{base}}
Content-Type: application/json
Accept: application/json, text/event-stream
{"jsonrpc":"2.0","id":21,"method":"tools/call","params":{"name":"sai_search_code","arguments":{"query":"compute total","limit":5,"path_glob":"nonexistent/**"}}}
HTTP 200
[Asserts]
jsonpath "$.result.isError" == false
jsonpath "$.result.structuredContent.hits" count == 0

# --- find_similar by LOCATION (alpha:1): returns neighbours, SELF-EXCLUDED (no alpha) ---
POST {{base}}
Content-Type: application/json
Accept: application/json, text/event-stream
{"jsonrpc":"2.0","id":22,"method":"tools/call","params":{"name":"sai_find_similar","arguments":{"path":"src/alpha.ts","line":1,"min_score":0.0}}}
HTTP 200
[Asserts]
jsonpath "$.result.isError" == false
jsonpath "$.result.structuredContent.hits[*].path" not includes "src/alpha.ts"

# --- find_similar by CODE: a compute-style snippet returns hits ---
POST {{base}}
Content-Type: application/json
Accept: application/json, text/event-stream
{"jsonrpc":"2.0","id":23,"method":"tools/call","params":{"name":"sai_find_similar","arguments":{"code":"function computeAmount(items){let s=0;for(const i of items)s+=i.price;return s;}","min_score":0.0}}}
HTTP 200
[Asserts]
jsonpath "$.result.isError" == false
jsonpath "$.result.structuredContent.hits" count >= 1

# --- find_similar with NEITHER code nor path+line → invalid params -32602 ---
POST {{base}}
Content-Type: application/json
Accept: application/json, text/event-stream
{"jsonrpc":"2.0","id":24,"method":"tools/call","params":{"name":"sai_find_similar","arguments":{}}}
HTTP 200
[Asserts]
jsonpath "$.error.code" == -32602

# --- find_similar at a non-existent location → invalid params -32602 ("no indexed chunk") ---
POST {{base}}
Content-Type: application/json
Accept: application/json, text/event-stream
{"jsonrpc":"2.0","id":25,"method":"tools/call","params":{"name":"sai_find_similar","arguments":{"path":"src/nope.ts","line":99}}}
HTTP 200
[Asserts]
jsonpath "$.error.code" == -32602

# NOTE: the find_similar{code} "requires a local embedder" error is qdrant-server-only and is
# unreachable in this duckdb+ort fixture — intentionally not asserted here.
