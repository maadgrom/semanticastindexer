# MCP protocol surface: initialize handshake, the tools/list GOLDEN contract (the exact
# 7-tool set + schemas that dedup-auditor + .mcp.json depend on), and a protocol error.

# --- initialize ---
POST {{base}}
Content-Type: application/json
Accept: application/json, text/event-stream
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"hurl","version":"1"}}}
HTTP 200
[Asserts]
jsonpath "$.result.serverInfo.name" == "rmcp"
jsonpath "$.result.capabilities.tools" exists
jsonpath "$.result.instructions" contains "sai_search_code"

# --- tools/list GOLDEN: exactly the 7 sai_ tools, each with an inputSchema ---
POST {{base}}
Content-Type: application/json
Accept: application/json, text/event-stream
{"jsonrpc":"2.0","id":2,"method":"tools/list"}
HTTP 200
[Asserts]
jsonpath "$.result.tools" count == 7
jsonpath "$.result.tools[*].name" includes "sai_search_code"
jsonpath "$.result.tools[*].name" includes "sai_find_similar"
jsonpath "$.result.tools[*].name" includes "sai_find_duplicates"
jsonpath "$.result.tools[*].name" includes "sai_index_status"
jsonpath "$.result.tools[*].name" includes "sai_prepare_mcp_setup"
jsonpath "$.result.tools[*].name" includes "sai_refresh"
jsonpath "$.result.tools[*].name" includes "sai_sync"
jsonpath "$.result.tools[0].inputSchema" exists

# --- unknown JSON-RPC method → protocol error -32601 ---
POST {{base}}
Content-Type: application/json
Accept: application/json, text/event-stream
{"jsonrpc":"2.0","id":3,"method":"nonsense/method"}
HTTP 200
[Asserts]
jsonpath "$.error.code" == -32601
