# sai_index_status (metadata) + sai_find_duplicates (membership + threshold monotonicity).
# Scores are NEVER asserted (model/FP-dependent) — only structure, cluster membership, and
# the subset relation between thresholds.

# --- index_status: the fixture has exactly 3 chunks on the duckdb backend ---
POST {{base}}
Content-Type: application/json
Accept: application/json, text/event-stream
{"jsonrpc":"2.0","id":10,"method":"tools/call","params":{"name":"sai_index_status","arguments":{}}}
HTTP 200
[Asserts]
jsonpath "$.result.isError" == false
jsonpath "$.result.structuredContent.backend" == "duckdb"
jsonpath "$.result.structuredContent.chunk_count" == 3
jsonpath "$.result.structuredContent.vector_dim" == 384
jsonpath "$.result.structuredContent.model" exists

# --- duplicates @ min_score 0.95: ONE cluster = exactly {alpha, beta}; gamma EXCLUDED ---
POST {{base}}
Content-Type: application/json
Accept: application/json, text/event-stream
{"jsonrpc":"2.0","id":11,"method":"tools/call","params":{"name":"sai_find_duplicates","arguments":{"min_score":0.95,"min_cluster_size":2}}}
HTTP 200
[Asserts]
jsonpath "$.result.isError" == false
jsonpath "$.result.structuredContent.clusters" count == 1
jsonpath "$.result.structuredContent.clusters[0].members[*].path" includes "src/alpha.ts"
jsonpath "$.result.structuredContent.clusters[0].members[*].path" includes "src/beta.ts"
jsonpath "$.result.structuredContent.clusters[0].members[*].path" not includes "src/gamma.ts"

# --- duplicates @ min_score 0.5: members are a SUPERSET (gamma now joins) — monotonicity ---
POST {{base}}
Content-Type: application/json
Accept: application/json, text/event-stream
{"jsonrpc":"2.0","id":12,"method":"tools/call","params":{"name":"sai_find_duplicates","arguments":{"min_score":0.5,"min_cluster_size":2}}}
HTTP 200
[Asserts]
jsonpath "$.result.isError" == false
jsonpath "$.result.structuredContent.clusters[0].members[*].path" includes "src/alpha.ts"
jsonpath "$.result.structuredContent.clusters[0].members[*].path" includes "src/beta.ts"
jsonpath "$.result.structuredContent.clusters[0].members[*].path" includes "src/gamma.ts"

# --- duplicates with a path_glob matching nothing → no clusters ---
POST {{base}}
Content-Type: application/json
Accept: application/json, text/event-stream
{"jsonrpc":"2.0","id":13,"method":"tools/call","params":{"name":"sai_find_duplicates","arguments":{"min_score":0.5,"min_cluster_size":2,"path_glob":"nonexistent/**"}}}
HTTP 200
[Asserts]
jsonpath "$.result.isError" == false
jsonpath "$.result.structuredContent.clusters" count == 0
