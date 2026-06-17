# sai_prepare_mcp_setup (the setup helper) + the loopback DNS-rebinding guard.

# --- prepare_mcp_setup (no execute) → returns ready-to-run command + config, no error ---
POST {{base}}
Content-Type: application/json
Accept: application/json, text/event-stream
{"jsonrpc":"2.0","id":50,"method":"tools/call","params":{"name":"sai_prepare_mcp_setup","arguments":{"backend":"duckdb","embedder":"ort"}}}
HTTP 200
[Asserts]
jsonpath "$.result.isError" == false
jsonpath "$.result.structuredContent.recommended_command" exists
jsonpath "$.result.structuredContent.mcp_server_config_example" exists

# --- prepare_mcp_setup with execute:true but the server lacks --allow-setup → blocked ---
# (layout-independent: with no --allow-setup, execution_blocked is set whether or not the
#  setup script is found next to the binary.)
POST {{base}}
Content-Type: application/json
Accept: application/json, text/event-stream
{"jsonrpc":"2.0","id":51,"method":"tools/call","params":{"name":"sai_prepare_mcp_setup","arguments":{"backend":"duckdb","embedder":"ort","execute":true}}}
HTTP 200
[Asserts]
jsonpath "$.result.isError" == false
jsonpath "$.result.structuredContent.execution_blocked" == true

# --- DNS-rebinding guard: a non-loopback Host header is rejected with HTTP 403 ---
POST {{base}}
Host: evil.com
Content-Type: application/json
Accept: application/json, text/event-stream
{"jsonrpc":"2.0","id":52,"method":"tools/list"}
HTTP 403
