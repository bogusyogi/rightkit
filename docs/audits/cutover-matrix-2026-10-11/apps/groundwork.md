# Groundwork — fresh cutover review

Reviewed Node stdio MCP source at `/Volumes/D/claude/mcps/groundwork/server.js`; `test.mjs` is excluded from production evidence. All 97 rows are `NA`: Groundwork exposes only bounded thought state plus Context7 library search/retrieval, with no RightKit import, production manifest, Rust/Swift target, FFI, sidecar, service bridge, UI, release, or QA host.

Closest reuse candidates are `cargo:rightkit-mcp` and `cargo:rightkit-http`: Groundwork directly implements Node MCP dispatch and native `fetch`, while both owners expose Rust APIs. No Node bridge exists, so neither supports a cutover task or N/M classification. `npm:@rightkit/agent-room-mcp` and `npm:@rightkit/ax` are Node-adjacent but own different room/conformance features. No Y, N, or M rows; no product change is supported.
