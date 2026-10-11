# Membrane (retained) — RightKit migration handoff

Source root: /Volumes/D/worktrees/membrane-live-diagnostics-phase1-repair

Use this audit snapshot to plan cutovers; recheck cited callers before edits because other chats may have changed source. Select verified published versions; keep source cutover, packaging & execution qualification separate.

[Full app evidence](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/membrane.json)

## rightkit-db — N

Rust Cortex opens SQLite directly; compatible RightKit DbOptions has no production route.

Route Cortex connection bootstrap through published rightkit-db after preserving Cortex schema & recovery policy.

- Local: [Connection::open_with_flags](/Volumes/D/worktrees/membrane-live-diagnostics-phase1-repair/engine/crates/cortex-store/src/memdb.rs:397) — Cortex durable store direct SQLite open.
- Owner: [lib.rs](/Volumes/D/claude/rightkit/crates/rightkit-db/src/lib.rs:139) — DbOptions controlled SQLite bootstrap API.

## rightkit-embed — N

Cortex implements local sentence embeddings with fastembed; compatible RightKit Embedder is unused.

Evaluate rightkit-embed against Cortex model selection & vector-store contract.

- Local: [FastEmbedder](/Volumes/D/worktrees/membrane-live-diagnostics-phase1-repair/engine/crates/cortex-core/src/embed.rs:162) — Local ONNX sentence embedding implementation.
- Owner: [embedder.rs](/Volumes/D/claude/rightkit/crates/rightkit-embed/src/embedder.rs:107) — EmbedderConfig/Embedder API.

## rightkit-mcp — N

Membrane owns active Rust stdio MCP server; compatible RightKit McpClient/ServerHost is unreferenced.

Evaluate ServerHost transport/admission fit; preserve Membrane protocol/tool policy.

- Local: [serve_stdio](/Volumes/D/worktrees/membrane-live-diagnostics-phase1-repair/engine/crates/membrane-mcp/src/jsonrpc.rs:60) — Local active MCP stdio server.
- Owner: [lib.rs](/Volumes/D/claude/rightkit/crates/rightkit-mcp/src/lib.rs:38) — Shared ServerHost/McpClient API.

## @rightkit/shell — N

Hub renderer invokes raw Tauri window/events commands; compatible typed shell bridge is unused.

Map Hub command/event surface to @rightkit/shell where contract matches.

- Local: [invoke/listen/getCurrentWindow](/Volumes/D/worktrees/membrane-live-diagnostics-phase1-repair/apps/membrane-hub/src/popover.mjs:78) — Local raw Tauri renderer bridge.
- Owner: [package.json](/Volumes/D/claude/rightkit/packages/shell/package.json:4) — Shared typed renderer bridge contract.

## @rightkit/tauri — N

Hub exposes raw product Tauri commands; shared typed host adapters are unused.

Compare Hub command signatures with @rightkit/tauri; adopt exact shared adapter surface only.

- Local: [tauri command](/Volumes/D/worktrees/membrane-live-diagnostics-phase1-repair/apps/membrane-hub/src-tauri/src/main.rs:473) — Local host command boundary.
- Owner: [package.json](/Volumes/D/claude/rightkit/packages/tauri/package.json:4) — Shared Tauri command-adapter contract.

Before marking a task complete: migrate matching callers, remove only proven duplicate mechanisms, preserve product-specific schemas/policy/UX, then run affected target checks. Update this inventory from fresh evidence.
