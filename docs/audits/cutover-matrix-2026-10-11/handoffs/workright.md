# WorkRight — RightKit migration handoff

Source root: /Volumes/D/claude/workright

Use this audit snapshot to plan cutovers; recheck cited callers before edits because other chats may have changed source. Select verified published versions; keep source cutover, packaging & execution qualification separate.

[Full app evidence](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/workright.json)

## rightkit-db — N

WorkRight local-store and backend DbPool directly open rusqlite databases, transactions, migrations, and tenant tables; rightkit-db owns shared SQLite/SQLCipher bootstrap.

Adopt rightkit-db for local-store and backend connections with SQLCipher/key policy; preserve WorkRight schemas, tenant boundary, and migration semantics; remove direct rusqlite bootstrap.

- Local: [LocalStore::open](/Volumes/D/claude/workright/crates/local-store/src/lib.rs:168) — opens rusqlite path and applies WAL pragmas
- Local: [DbPool::open](/Volumes/D/claude/workright/backend/server/src/db.rs:397) — opens backend rusqlite database and runs local schema migration
- Owner: [lib.rs](/Volumes/D/claude/rightkit/crates/rightkit-db/src/lib.rs:237) — DbOptions::open_writer provides keyed SQLite/SQLCipher connection bootstrap

Delivery: No RightKit import or published dependency is present; local route is active.

## rightkit-logs — N

Core service, session helper, and backend initialize tracing subscribers for production diagnostics; this is local logging infrastructure overlapping rightkit-logs.

Route agent and backend diagnostics through rightkit-logs with WorkRight redaction and consent policy; preserve security/event audit records separately; remove direct tracing subscriber setup.

- Local: [init_tracing](/Volumes/D/claude/workright/crates/core-service/src/main.rs:196) — installs tracing-subscriber formatter for core diagnostics
- Local: [init_tracing](/Volumes/D/claude/workright/backend/server/src/main.rs:85) — installs backend tracing output
- Owner: [tracing_file.rs](/Volumes/D/claude/rightkit/crates/rightkit-logs/src/tracing_file.rs:154) — install_tracing writes privacy-safe daily JSONL diagnostics

Delivery: No RightKit import or published dependency is present; local route is active.

## @rightkit/app-shell — N

Dashboard and agent UI each implement local React frame/header/navigation/status shells; rightkit app-shell provides shared React frame and command primitives.

Adopt @rightkit/app-shell where shared frame primitives fit; preserve WorkRight dashboard routes and employee disclosure content; remove duplicate frame/navigation code after visual and route parity.

- Local: [App](/Volumes/D/claude/workright/apps/dashboard/src/App.tsx:32) — owns dashboard header, nav, route frame, and org input
- Local: [App](/Volumes/D/claude/workright/apps/agent-ui/src/App.tsx:13) — owns Tauri agent status frame and disclosure
- Owner: [layout.tsx](/Volumes/D/claude/rightkit/packages/app-shell/src/react/layout.tsx:518) — AppShell composes shared React frame, navigation, titlebar, command palette, shortcuts, and shell provider

Delivery: No RightKit import or published dependency is present; local route is active.

Before marking a task complete: migrate matching callers, remove only proven duplicate mechanisms, preserve product-specific schemas/policy/UX, then run affected target checks. Update this inventory from fresh evidence.
