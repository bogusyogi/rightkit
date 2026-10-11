# Ember — RightKit migration handoff

Source root: /Volumes/D/claude/ember

Use this audit snapshot to plan cutovers; recheck cited callers before edits because other chats may have changed source. Select verified published versions; keep source cutover, packaging & execution qualification separate.

[Full app evidence](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/ember.json)

## rightkit-cache — N

Preview::DiskCache and Lru are live Ember thumbnail caches; no rightkit-cache caller reaches production.

Compare preview::DiskCache JPEG encoding, key shape, budget/eviction and rebuild behavior with rightkit-cache::DiskCache; migrate only after parity, then remove local cache.

- Local: [preview::DiskCache](/Volumes/D/claude/ember/crates/preview/src/disk.rs:23) — live thumbnail cache uses app-owned disk cache with temp+rename, budget and LRU eviction
- Owner: [lib.rs](/Volumes/D/claude/rightkit/crates/rightkit-cache/src/lib.rs:3) — DiskCache provides restart-safe byte cache with LRU, atomic writes and invalidation

## rightkit-crash — N

AppLogger::record_panics installs Ember panic capture; no rightkit-crash hook is wired.

Route logging::record_panics through rightkit-crash::install_panic_hook/record_report; preserve Ember redaction, rotation and pending-file policy before deleting local hook.

- Local: [logging::record_panics](/Volumes/D/claude/ember/apps/lightcraft/src/main.rs:605) — running egui app installs app-owned panic recording
- Owner: [lib.rs](/Volumes/D/claude/rightkit/crates/rightkit-crash/src/lib.rs:3) — install_panic_hook writes redacted structured panic reports

## rightkit-fs — N

Ember safe_file::write_atomic and LibraryLock own durable writes/locking; no rightkit-fs caller is wired.

Map safe_file::write_atomic and LibraryLock to rightkit-fs atomic_write/FileLock; preserve export durability plus network-filesystem fallback before deleting local primitives.

- Local: [write_atomic](/Volumes/D/claude/ember/crates/catalog/src/safe_file.rs:155) — catalog/export code uses app-owned atomic temp+rename writer; catalog lock is app-owned at lock.rs:96
- Owner: [lib.rs](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/lib.rs:45) — atomic_write plus FileLock provide shared crash-safe filesystem state

## rightkit-http — N

photo-ai calls Ember lightcraft_fetch get/post_json directly for OpenRouter; no rightkit-http caller is wired.

Adapt photo-ai get/post_json to rightkit-http::Client; preserve OpenRouter endpoint policy, response caps and cancellation before deleting lightcraft_fetch HTTP.

- Local: [lightcraft_fetch::http::{get,post_json}](/Volumes/D/claude/ember/crates/photo-ai/src/network.rs:5) — live OpenRouter model and completion calls use Ember-owned HTTP client
- Owner: [lib.rs](/Volumes/D/claude/rightkit/crates/rightkit-http/src/lib.rs:3) — Client and download_to_path provide bounded/retried HTTP, cancellation and verified downloads

## rightkit-llm — N

photo-ai owns OpenRouter request construction, auth, response limits and parsing; no rightkit-llm provider caller is wired.

Adapt OpenRouter request_body/parse_response to rightkit-llm::openai::OpenAiCompatProvider; preserve Ember prompt, receipt and model-selection policy before removing transport code.

- Local: [OpenRouter::request path](/Volumes/D/claude/ember/crates/photo-ai/src/network.rs:10) — photo AI sends provider-specific OpenRouter requests and parses responses locally
- Owner: [lib.rs](/Volumes/D/claude/rightkit/crates/rightkit-llm/src/lib.rs:3) — OpenAiCompatProvider and AnthropicProvider provide provider clients, streaming and failover

## rightkit-logs — N

Ember AppLogger owns filters, rotating files and panic records; no rightkit-logs writer/tracing caller is wired.

Map AppLogger install/record_panics to rightkit-logs JsonlStore/tracing APIs; preserve Ember filters, rotation and stderr behavior before removal.

- Local: [AppLogger](/Volumes/D/claude/ember/apps/lightcraft/src/logging.rs:241) — running egui app owns filtering, rotating file sink and panic records
- Owner: [lib.rs](/Volumes/D/claude/rightkit/crates/rightkit-logs/src/lib.rs:20) — JsonlStore, diagnostics collection, redaction and tracing installation are shared APIs

## rightkit-mcp — N

Ember crates/mcp Server owns JSON-RPC lifecycle, tools/list and tools/call; no rightkit-mcp ServerHost caller is wired.

Adapt crates/mcp Server to rightkit-mcp ServerHost; preserve Ember tool schemas, resource URIs and local admission policy before removing JSON-RPC server code.

- Local: [Server](/Volumes/D/claude/ember/crates/mcp/src/server.rs:36) — production MCP server owns JSON-RPC lifecycle, tools/list and tools/call
- Owner: [lib.rs](/Volumes/D/claude/rightkit/crates/rightkit-mcp/src/lib.rs:38) — ServerHost/McpClient provide shared MCP server/client lifecycle

## rightkit-models — N

Segmenter::start_download uses Ember SAM3 downloader, mirrors and activation; no rightkit-models Store caller is wired.

Map Segmenter downloader to rightkit-models Store/InstallOptions; preserve SAM3 mirror acknowledgment, license UX and model path before deleting local installer.

- Local: [Segmenter::start_download](/Volumes/D/claude/ember/crates/engine/src/segment/mod.rs:295) — AI mask model download runs through Ember-owned downloader, mirror policy and local activation
- Owner: [lib.rs](/Volumes/D/claude/rightkit/crates/rightkit-models/src/lib.rs:3) — Store and InstallOptions provide revisioned verified model install/activation

## rightkit-settings — N

Preferences::patch owns Ember JSON merge, corruption quarantine and atomic persistence; no rightkit-settings Store caller is wired.

Map Preferences load/patch/quarantine to rightkit-settings Store/FileBackend; preserve unknown keys and blocked-session behavior before deleting local persistence.

- Local: [Preferences::patch](/Volumes/D/claude/ember/apps/lightcraft-desktop/src/preferences.rs:54) — desktop preferences merge bounded JSON patches and persist with app-owned atomic writer
- Owner: [lib.rs](/Volumes/D/claude/rightkit/crates/rightkit-settings/src/lib.rs:1) — versioned Store/FileBackend provide settings persistence, migrations and subscriptions

## rightkit-watch — N

AutoImport::poll owns Ember folder polling, worker cancellation and import queueing; no rightkit-watch FileSetWatcher caller is wired.

Replace AutoImport::poll with rightkit-watch FileSetWatcher only after preserving generation cancellation, rename handling and import queue semantics.

- Local: [AutoImport::poll](/Volumes/D/claude/ember/crates/desktop-host/src/auto_import.rs:54) — live auto-import detects folder changes by app-owned polling worker, cancellation and listing consumption
- Owner: [lib.rs](/Volumes/D/claude/rightkit/crates/rightkit-watch/src/lib.rs:6) — FileSetWatcher provides debounced OS file-set watching with rename handling

## @rightkit/i18n — N

Ember i18n.ts owns locale normalization, imported catalogs and translation fallback; no @rightkit/i18n createI18n caller is wired.

Replace i18n.ts normalizeLocale/translate with @rightkit/i18n createI18n; preserve Ember locale catalogs and fallback behavior before deleting local provider.

- Local: [translate](/Volumes/D/claude/ember/apps/lightcraft-desktop/web/src/i18n.ts:29) — renderer translates against app-owned locale catalogs imported from ui-egui JSON
- Owner: [index.tsx](/Volumes/D/claude/rightkit/packages/i18n/src/index.tsx:35) — createI18n owns catalog loading, locale state, RTL direction and provider hooks

## @rightkit/media-ui — N

PhotoPreview is a live app-owned base image-viewing route with native preview-handle conversion, layered handoff and retry state; no @rightkit/media-ui ImageViewer/ImageEditor caller is wired. StageWorkspace mask geometry and engine commands are Ember policy outside generic viewer/editor ownership.

Evaluate an adapter for PhotoPreview's base viewing/handoff behavior only; retain StageWorkspace mask geometry, mask.add/mask.update commands and photo-develop policy.

- Local: [PhotoPreview](/Volumes/D/claude/ember/apps/lightcraft-desktop/web/src/preview/PhotoPreview.tsx:24) — renderer owns layered photo preview loading, retry/error state and native URL conversion
- Owner: [ImageViewer](/Volumes/D/claude/rightkit/packages/media-ui/src/image/ImageViewer.tsx:97) — shared base viewer owns image source, transform and generic edit recipe; ImageEditor is its editable alias at lines 745-750

## @rightkit/theme — N

Ember app.css owns local token overrides and stage surfaces; @rightkit/theme has no production import.

Adopt @rightkit/theme token/runtime APIs for Ember colors and dark mode or document product-owned overrides; remove duplicate token definitions only after rendered parity.

- Local: [app.css](/Volumes/D/claude/ember/apps/lightcraft-desktop/web/src/app.css:1) — renderer owns Ember CSS variables/layout; @rightkit/theme is only a dev dependency and has no production import
- Owner: [index.ts](/Volumes/D/claude/rightkit/packages/theme/src/index.ts:1) — theme exports color, brand, CSS runtime and token APIs

Delivery: @rightkit/theme is pinned in devDependencies but no production import was found.

Before marking a task complete: migrate matching callers, remove only proven duplicate mechanisms, preserve product-specific schemas/policy/UX, then run affected target checks. Update this inventory from fresh evidence.
