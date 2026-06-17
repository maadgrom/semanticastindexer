# Read-only (RO) server: the write tools must REFUSE with invalid params -32602. This is the
# safety gate — a server started without --allow-write cannot mutate the index.

# --- sai_refresh refused on the read-only server ---
POST {{base}}
Content-Type: application/json
Accept: application/json, text/event-stream
{"jsonrpc":"2.0","id":40,"method":"tools/call","params":{"name":"sai_refresh","arguments":{"paths":["src/alpha.ts"]}}}
HTTP 200
[Asserts]
jsonpath "$.error.code" == -32602
jsonpath "$.error.message" contains "read-only"

# --- sai_sync refused on the read-only server ---
POST {{base}}
Content-Type: application/json
Accept: application/json, text/event-stream
{"jsonrpc":"2.0","id":41,"method":"tools/call","params":{"name":"sai_sync","arguments":{}}}
HTTP 200
[Asserts]
jsonpath "$.error.code" == -32602
jsonpath "$.error.message" contains "read-only"

# --- read tools still work on the read-only server ---
POST {{base}}
Content-Type: application/json
Accept: application/json, text/event-stream
{"jsonrpc":"2.0","id":42,"method":"tools/call","params":{"name":"sai_index_status","arguments":{}}}
HTTP 200
[Asserts]
jsonpath "$.result.isError" == false
jsonpath "$.result.structuredContent.chunk_count" == 3
