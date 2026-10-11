# LightCraft — RightKit migration handoff

Source root: /Volumes/D/claude/workflows/lightcraft

Use this audit snapshot to plan cutovers; recheck cited callers before edits because other chats may have changed source. Select verified published versions; keep source cutover, packaging & execution qualification separate.

[Full app evidence](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/lightcraft.json)

## rightkit-cache — N

Native & web thumbnail/source caches use LightCraft DiskCache/Lru; no RightKit cache route.

Adapt rightkit-cache for native cache metadata only; retain LightCraft image encoding, keys & web OPFS policy.

- Local: [DiskCache](/Volumes/D/claude/workflows/lightcraft/crates/preview/src/disk.rs:20) — active thumbnail cache uses bounded disk LRU, temp+rename, eviction
- Owner: [DiskCache](/Volumes/D/claude/rightkit/crates/rightkit-cache/src/lib.rs:18) — shared size-capped atomic disk cache

## rightkit-crash — N

Native command guard & panic hook are local; no RightKit crash capture route.

Wire shared crash capture around existing command guard; retain LightCraft recovery semantics.

- Local: [catch / install_hook](/Volumes/D/claude/workflows/lightcraft/crates/engine/src/guard.rs:21) — converts command panic to error & appends plaintext panic log
- Owner: [install_panic_hook](/Volumes/D/claude/rightkit/crates/rightkit-crash/src/lib.rs:30) — shared redacted crash report & pending-report API

## rightkit-framed-sidecar — N

LightCraft MCP owns newline JSON-RPC framing without RightKit bounded-frame transport.

Adopt bounded framing below LightCraft MCP schema; retain all tool/resource semantics.

- Local: [Server](/Volumes/D/claude/workflows/lightcraft/crates/mcp/src/server.rs:36) — reads & writes its MCP JSON-RPC stream
- Owner: [read_json_line / write_json_line](/Volumes/D/claude/rightkit/crates/rightkit-framed-sidecar/src/lib.rs:8) — bounded, resynchronising JSON-lines transport

## rightkit-fs — N

LightCraft implements atomic durable writes & library locks locally; no RightKit fs route.

Map native catalog/sidecar persistence to rightkit-fs; retain photo-specific durability classes & web store.

- Local: [write_atomic](/Volumes/D/claude/workflows/lightcraft/crates/catalog/src/safe_file.rs:153) — temp+sync+rename protects exports, sidecars & settings
- Owner: [atomic_write / FileLock](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/lib.rs:45) — shared atomic write & cross-process lock API

## rightkit-mcp — N

CLI exposes a hand-written LightCraft MCP server over stdio; no shared MCP host route.

Adapt LightCraft tool catalog to rightkit-mcp server host; retain photo command schema & resources.

- Local: [Server](/Volumes/D/claude/workflows/lightcraft/crates/mcp/src/server.rs:36) — serves LightCraft tools/resources over JSON-RPC stdio
- Owner: [ServerHost / ToolProvider](/Volumes/D/claude/rightkit/crates/rightkit-mcp/src/lib.rs:38) — shared MCP server-host API

## rightkit-qa — N

Custom CI plus deterministic headless egui driver remain local; no RightKit QA harness route.

Use rightkit-qa around control/CLI qualification; retain headless renderer & image assertions.

- Local: [Headless](/Volumes/D/claude/workflows/lightcraft/crates/ui-egui/src/headless.rs:105) — production CLI/tests drive app logic into CPU-rendered frames
- Owner: [Harness::new](/Volumes/D/claude/rightkit/crates/rightkit-qa/src/harness.rs:99) — shared scenario, control & evidence harness

## rightkit-search — N

Catalog owns live free-text/filter query evaluation; no RightKit query route.

Introduce rightkit-search only for reusable text/operator parsing; retain photo filter rules & catalog ranking.

- Local: [Filter::matches / Catalog::query](/Volumes/D/claude/workflows/lightcraft/crates/catalog/src/query.rs:19) — searches photo metadata & applies catalog filters
- Owner: [Query / Schema](/Volumes/D/claude/rightkit/crates/rightkit-search/src/lib.rs:20) — shared query parse & compile APIs

## rightkit-settings — N

Library preference files have local corruption preservation, load & save flow; no RightKit settings route.

Adapt rightkit-settings behind library preference files; retain library-scoped schemas & corruption UX.

- Local: [SettingsLoad](/Volumes/D/claude/workflows/lightcraft/crates/engine/src/library.rs:201) — loads prefs/view/presets, quarantines corrupt files & blocks unsafe overwrite
- Owner: [Store / Schema](/Volumes/D/claude/rightkit/crates/rightkit-settings/src/lib.rs:16) — versioned settings, migrations & change API

Before marking a task complete: migrate matching callers, remove only proven duplicate mechanisms, preserve product-specific schemas/policy/UX, then run affected target checks. Update this inventory from fresh evidence.
