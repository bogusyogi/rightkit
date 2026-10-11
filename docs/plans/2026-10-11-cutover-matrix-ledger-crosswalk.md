# Cutover matrix x gap ledger crosswalk (2026-10-11)

Read-only analysis. Inputs: `docs/audits/cutover-matrix-2026-10-11/` (matrix.csv, handoffs/<app>.md), `docs/audits/2026-10-10-rightkit-gap-ledger.md` (rows 1-115), `docs/PLATFORM-MATRIX.md`, and `docs/plans/` rulings. Nothing else was edited or committed. The audit directory is another author's uncommitted work and was read only.

## Scope and method

- **Apps:** CodeRight, HeardRight, ViewRight, MailRight, ScrapeRight, CutRight, GenRight, VoiceRight, WorkRight, Pulse, PostRight, Ember, Legion, RustRight. SellRight and RightSites are skipped by owner instruction; SwiftRight is not in the requested list and is not analysed.
- **Cells:** 146 N/M cells from `matrix.csv`. Each matched a handoff section `handoffs/<app>.md`, and all 146 codes agree with the handoff heading.
- **Stated migration work:** first clause of the handoff's action line, shortened.
- **Ledger matching:** exact component token (crate, npm package, or Swift product name). Only rows still open or in progress are cited as blockers. Closed or answered rows are cited only as evidence (#79, #98). Rows are cited at component level: a row that names the component blocks that component's cells in every app.
- **READY:** PLATFORM-MATRIX registry version equals tree version (or the ledger states the version is published), and no ledger row names the component.
- **BLOCKED-RIGHTKIT:** an open row names the component, or the needed version is private, never published, or tree-only (0.x additions the ledger places in tree or staged).
- **RULED-LOCAL:** an explicit ruling keeps the cell's stated work in the app. Two found: #96 (locked-UI rule, applied to CodeRight `@rightkit/app-shell` with #101 item 1) and #42 (PDFium stays app-side, applied to ScrapeRight `rightkit-pdf`).
- **UNKNOWN:** no open row, and the registry version is older than the tree. The documents do not show whether the registry version carries the audited API.
- The audit's Y/N/M codes are not reclassified here. Contradictions are in section 3.

## Cell table (146 N/M cells)

| App | Component | Code | Stated migration work | Blocker class | Ledger rows | Note (registry/tree; basis) |
|---|---|---|---|---|---|---|
| HeardRight | `rightkit-align` | N | Migrate terminal-command alignment caller to rightkit_align::align after mapping timed words | BLOCKED | #38 | private (publish=false), tree 0.1.0. tree-only, unpublished |
| HeardRight | `rightkit-asr` | M | Move CoreMlParakeet decode/display/command-alignment callers onto rightkit_asr::CoreMlParakeet APIs | BLOCKED | #24, #32, #78, #83 | registry 0.0.2, tree 0.2.0, published 0.1.0 per lead (feed stale). needed 0.2.0 is tree-only |
| GenRight | `rightkit-asr` | N | Route transcribe through rightkit_asr::Whisper, preserve GenRight transcript/caption policy, then... | BLOCKED | #24, #32, #78, #83 | registry 0.0.2, tree 0.2.0, published 0.1.0 per lead (feed stale). needed 0.2.0 is tree-only |
| GenRight | `rightkit-audio` | N | Replace compose's untyped narration/music filter construction with rightkit-audio primitives where... | READY | #98 (publication note) | registry 0.2.0, tree 0.2.1, published 0.2.1 per lead. no gap row; #98 states audio 0.2.1 live |
| VoiceRight | `rightkit-audio` | N | Route WAV parsing/writing through rightkit-audio or rightkit-tts | READY | #98 (publication note) | registry 0.2.0, tree 0.2.1, published 0.2.1 per lead. no gap row; #98 states audio 0.2.1 live |
| ScrapeRight | `rightkit-browser` | N | Prove BrowserSession::launch/new_page plus BrowserPage::eval/close meet article profile, resource... | BLOCKED | #25, #26, #40 | registry 0.2.2, tree 0.2.4 (staged 0.2.4). open cookie, runtime and address-policy gaps |
| Legion | `rightkit-browser` | N | Route visual script browser sessions through rightkit-browser | BLOCKED | #25, #26, #40 | registry 0.2.2, tree 0.2.4 (staged 0.2.4). open cookie, runtime and address-policy gaps |
| Ember | `rightkit-cache` | N | Compare preview::DiskCache JPEG encoding, key shape, budget/eviction and rebuild behavior with... | BLOCKED | #33, #66, #108 | registry 0.1.0, tree 0.2.0. needed streaming/private APIs are tree-only |
| PostRight | `rightkit-channel-memory` | N | Map fixture input to ChannelMemory & invoke shared parse/write rules before local persistence | BLOCKED | #8, #89 | private (publish=false), tree 0.1.0. tree-only, unpublished |
| CutRight | `rightkit-compose` | N | Adapt compatible timeline video/still/title timing & transform data into RightKit Composition,... | BLOCKED | #36 | private (publish=false), tree 0.1.0. tree-only, unpublished |
| HeardRight | `rightkit-control` | N | Replace local macos_input/windows_uia target probes with rightkit_control adapters | BLOCKED | #56, #74, #92 | registry 0.1.11, tree 0.2.0 (staged 0.2.0). Windows WebView2 gaps open |
| ViewRight | `rightkit-crash` | N | crash capture is local | BLOCKED | #108 | registry 0.2.0, tree 0.2.1 (staged 0.2.1). flag-reset and owner-only report gaps open |
| Ember | `rightkit-crash` | N | Route logging::record_panics through rightkit-crash::install_panic_hook/record_report | BLOCKED | #108 | registry 0.2.0, tree 0.2.1 (staged 0.2.1). flag-reset and owner-only report gaps open |
| HeardRight | `rightkit-db` | N | Replace local SQLCipher bootstrap/migration/recovery with rightkit_db | BLOCKED | #45, #95 | registry 0.3.2, tree 0.4.0 (staged 0.3.2). legacy version-table adoption open |
| WorkRight | `rightkit-db` | N | Adopt rightkit-db for local-store and backend connections with SQLCipher/key policy | BLOCKED | #45, #95 | registry 0.3.2, tree 0.4.0 (staged 0.3.2). legacy version-table adoption open |
| Legion | `rightkit-db` | N | Adopt rightkit-db bootstrap beneath Legion’s decision schema & migrations | BLOCKED | #45, #95 | registry 0.3.2, tree 0.4.0 (staged 0.3.2). legacy version-table adoption open |
| CodeRight | `rightkit-embed` | N | Compare local rightkit-embed with owner API | UNKNOWN | none | registry 0.3.1, tree 0.3.2 (staged 0.3.2). no ledger row; tree newer, docs do not show 0.3.1 owner API |
| ViewRight | `rightkit-framed-sidecar` | N | OCR framing/supervision is local | BLOCKED | #60, #72, #95 | registry 0.1.0, tree 0.2.0 (staged 0.1.0). deadline I/O and framing options are tree-only (0.2.0) |
| GenRight | `rightkit-framed-sidecar` | N | Adopt rightkit-framed-sidecar wire/deadline APIs in engine supervisor & stdio engine while preserving... | BLOCKED | #60, #72, #95 | registry 0.1.0, tree 0.2.0 (staged 0.1.0). deadline I/O and framing options are tree-only (0.2.0) |
| HeardRight | `rightkit-fs` | N | Route settings/history state files through rightkit_fs atomic/lock primitives | BLOCKED | #33, #73, #95, #97, #106, #111 | registry 0.3.3, tree 0.3.4 (staged 0.3.3). 0.3.4 additions (ReplaceFileW, no-replace publish, incremental SHA) open |
| ViewRight | `rightkit-fs` | N | local atomic saves overlap shared fs | BLOCKED | #33, #73, #95, #97, #106, #111 | registry 0.3.3, tree 0.3.4 (staged 0.3.3). 0.3.4 additions (ReplaceFileW, no-replace publish, incremental SHA) open |
| VoiceRight | `rightkit-fs` | N | Adopt rightkit-fs for app-data/staged/generated/voice writes | BLOCKED | #33, #73, #95, #97, #106, #111 | registry 0.3.3, tree 0.3.4 (staged 0.3.3). 0.3.4 additions (ReplaceFileW, no-replace publish, incremental SHA) open |
| Pulse | `rightkit-fs` | N | Adopt cargo:rightkit-fs at owning runtime boundary, preserve Pulse policy/UI, route callers through... | BLOCKED | #33, #73, #95, #97, #106, #111 | registry 0.3.3, tree 0.3.4 (staged 0.3.3). 0.3.4 additions (ReplaceFileW, no-replace publish, incremental SHA) open |
| Ember | `rightkit-fs` | N | Map safe_file::write_atomic and LibraryLock to rightkit-fs atomic_write/FileLock | BLOCKED | #33, #73, #95, #97, #106, #111 | registry 0.3.3, tree 0.3.4 (staged 0.3.3). 0.3.4 additions (ReplaceFileW, no-replace publish, incremental SHA) open |
| HeardRight | `rightkit-hotkeys` | N | Route shortcut parsing, registration, conflict checks, & PTT hooks through rightkit_hotkeys | BLOCKED | #22 | never published, tree 0.1.0 (staged publishing). unpublished; Windows backend untested |
| Pulse | `rightkit-hotkeys` | N | Adopt cargo:rightkit-hotkeys at owning runtime boundary, preserve Pulse policy/UI, route callers... | BLOCKED | #22 | never published, tree 0.1.0 (staged publishing). unpublished; Windows backend untested |
| CodeRight | `rightkit-http` | M | Route OpenAiCompatClient::stream_once request construction through rightkit_http::AsyncClient::stream... | BLOCKED | #4, #12, #27, #40, #49, #83, #86, #110, #115 | registry 0.2.4, tree 0.3.0, staged 0.2.5. tree-only 0.3.0; adversarial-read fixes open (#86) |
| HeardRight | `rightkit-http` | N | Replace local lease_post/ureq transport with rightkit_http::Client | BLOCKED | #4, #12, #27, #40, #49, #83, #86, #110, #115 | registry 0.2.4, tree 0.3.0, staged 0.2.5. tree-only 0.3.0; adversarial-read fixes open (#86) |
| MailRight | `rightkit-http` | M | Route direct Google/profile/AI/remote-image/translation callers through rightkit-http async client | BLOCKED | #4, #12, #27, #40, #49, #83, #86, #110, #115 | registry 0.2.4, tree 0.3.0, staged 0.2.5. tree-only 0.3.0; adversarial-read fixes open (#86) |
| ScrapeRight | `rightkit-http` | M | Keep Client::stream_any primary | BLOCKED | #4, #12, #27, #40, #49, #83, #86, #110, #115 | registry 0.2.4, tree 0.3.0, staged 0.2.5. tree-only 0.3.0; adversarial-read fixes open (#86) |
| Pulse | `rightkit-http` | N | Adopt cargo:rightkit-http at owning runtime boundary, preserve Pulse policy/UI, route callers through... | BLOCKED | #4, #12, #27, #40, #49, #83, #86, #110, #115 | registry 0.2.4, tree 0.3.0, staged 0.2.5. tree-only 0.3.0; adversarial-read fixes open (#86) |
| Ember | `rightkit-http` | N | Adapt photo-ai get/post_json to rightkit-http::Client | BLOCKED | #4, #12, #27, #40, #49, #83, #86, #110, #115 | registry 0.2.4, tree 0.3.0, staged 0.2.5. tree-only 0.3.0; adversarial-read fixes open (#86) |
| Legion | `rightkit-http` | N | Replace direct OAuth/SEO HTTP transport with rightkit-http while preserving Google-specific request... | BLOCKED | #4, #12, #27, #40, #49, #83, #86, #110, #115 | registry 0.2.4, tree 0.3.0, staged 0.2.5. tree-only 0.3.0; adversarial-read fixes open (#86) |
| ScrapeRight | `rightkit-jobs` | N | Map Queue recovery, entitlement gates & EngineEvent framing onto JobStore::open plus Runner::new/run | BLOCKED | #31, #39, #66, #84, #112 | registry 0.2.6, tree 0.2.8, staged 0.2.7. 0.2.8 additions open |
| RustRight | `rightkit-jobs` | N | Map build-control wire/state requirements to rightkit-jobs APIs, then replace local generic lifecycle... | BLOCKED | #31, #39, #66, #84, #112 | registry 0.2.6, tree 0.2.8, staged 0.2.7. 0.2.8 additions open |
| HeardRight | `rightkit-license` | M | Delete local verify_lease_envelope_signature and call rightkit_license::verify_lease_envelope | BLOCKED | #47, #54, #77 | registry 0.1.3, tree 0.2.0 (staged 0.1.3). verify_lease_envelope and lifecycle clock are tree-only (0.2.0) |
| CodeRight | `rightkit-llm` | N | Compare local rightkit-llm with owner API | BLOCKED | #10, #16 | never published, tree 0.2.0 (staged 0.2.0). unpublished; no CLI provider |
| MailRight | `rightkit-llm` | N | Route commands/pro.rs & ai.rs through rightkit-llm | BLOCKED | #10, #16 | never published, tree 0.2.0 (staged 0.2.0). unpublished; no CLI provider |
| Ember | `rightkit-llm` | N | Adapt OpenRouter request_body/parse_response to rightkit-llm::openai::OpenAiCompatProvider | BLOCKED | #10, #16 | never published, tree 0.2.0 (staged 0.2.0). unpublished; no CLI provider |
| CutRight | `rightkit-logs` | N | Add rightkit-logs JsonlStore/RedactionPolicy sink for stderr & protocol events, then route... | BLOCKED | #41, #77, #83, #84, #108 | registry 0.2.0, tree 0.2.2 (staged 0.2.1). tracing feature and checked init open |
| WorkRight | `rightkit-logs` | N | Route agent and backend diagnostics through rightkit-logs with WorkRight redaction and consent policy | BLOCKED | #41, #77, #83, #84, #108 | registry 0.2.0, tree 0.2.2 (staged 0.2.1). tracing feature and checked init open |
| Pulse | `rightkit-logs` | N | Adopt cargo:rightkit-logs at owning runtime boundary, preserve Pulse policy/UI, route callers through... | BLOCKED | #41, #77, #83, #84, #108 | registry 0.2.0, tree 0.2.2 (staged 0.2.1). tracing feature and checked init open |
| Ember | `rightkit-logs` | N | Map AppLogger install/record_panics to rightkit-logs JsonlStore/tracing APIs | BLOCKED | #41, #77, #83, #84, #108 | registry 0.2.0, tree 0.2.2 (staged 0.2.1). tracing feature and checked init open |
| RustRight | `rightkit-logs` | N | Adapt broker event schema to rightkit-logs JsonlStore/AsyncWriter while preserving broker lifecycle... | BLOCKED | #41, #77, #83, #84, #108 | registry 0.2.0, tree 0.2.2 (staged 0.2.1). tracing feature and checked init open |
| MailRight | `rightkit-mcp` | N | Adopt rightkit-mcp for loopback transport/auth & tool registration | BLOCKED | #51, #85 | registry 0.5.2, tree 0.5.3 (published 0.5.3 per lead); HTTP route hook is 0.6.0. 0.6.0 (async HTTP, route mount, raw JSON) is tree-only |
| Ember | `rightkit-mcp` | N | Adapt crates/mcp Server to rightkit-mcp ServerHost | BLOCKED | #51, #85 | registry 0.5.2, tree 0.5.3 (published 0.5.3 per lead); HTTP route hook is 0.6.0. 0.6.0 (async HTTP, route mount, raw JSON) is tree-only |
| Legion | `rightkit-mcp` | N | Host Legion tools through rightkit-mcp stdio server adapter | BLOCKED | #51, #85 | registry 0.5.2, tree 0.5.3 (published 0.5.3 per lead); HTTP route hook is 0.6.0. 0.6.0 (async HTTP, route mount, raw JSON) is tree-only |
| CodeRight | `rightkit-media` | N | Compare local rightkit-media with owner API | BLOCKED | #17, #20, #27, #28, #29, #32, #35, #65, #67, #75, #91, #95 | registry 0.2.4, tree 0.2.6 (staged 0.2.5). 0.2.6 additions and CutRight licence decision open |
| CodeRight | `rightkit-models` | N | Compare local rightkit-models with owner API | BLOCKED | #24, #27, #40, #78, #83, #99 | registry 0.2.7, tree 0.2.9 (staged 0.2.8). Whisper catalog entry and suite comments open; grant needs Adrian |
| HeardRight | `rightkit-models` | M | Replace model_fetch ensure/download/manifest flow with rightkit_models... | BLOCKED | #24, #27, #40, #78, #83, #99 | registry 0.2.7, tree 0.2.9 (staged 0.2.8). Whisper catalog entry and suite comments open; grant needs Adrian |
| GenRight | `rightkit-models` | M | Express Whisper artifacts as a rightkit-models ModelSpec/Store install route, migrate existing files,... | BLOCKED | #24, #27, #40, #78, #83, #99 | registry 0.2.7, tree 0.2.9 (staged 0.2.8). Whisper catalog entry and suite comments open; grant needs Adrian |
| VoiceRight | `rightkit-models` | N | Adopt rightkit-models manifest/verification/activation for QTS model and ORT provisioning | BLOCKED | #24, #27, #40, #78, #83, #99 | registry 0.2.7, tree 0.2.9 (staged 0.2.8). Whisper catalog entry and suite comments open; grant needs Adrian |
| Ember | `rightkit-models` | N | Map Segmenter downloader to rightkit-models Store/InstallOptions | BLOCKED | #24, #27, #40, #78, #83, #99 | registry 0.2.7, tree 0.2.9 (staged 0.2.8). Whisper catalog entry and suite comments open; grant needs Adrian |
| CodeRight | `rightkit-oauth` | M | Build OpenAI OAuthConfig::openai with CodeRight redirect, scopes, and originator authorization parameter | UNKNOWN | none | registry 0.3.0, tree 0.3.1 (staged 0.3.1). no ledger row; tree newer, docs do not show 0.3.0 OpenAI/loopback API |
| Legion | `rightkit-oauth` | N | Move Google OAuth transport/state mechanics to rightkit-oauth | UNKNOWN | none | registry 0.3.0, tree 0.3.1 (staged 0.3.1). no ledger row; tree newer, docs do not show 0.3.0 OpenAI/loopback API |
| HeardRight | `rightkit-ort` | N | Replace local ORT discovery/init/session setup with rightkit_ort | BLOCKED | #35, #103 | registry 0.2.2, tree 0.2.3. explicit-path and telemetry-off initialiser open (#103) |
| VoiceRight | `rightkit-ort` | N | Route ORT setup through rightkit-ort | BLOCKED | #35, #103 | registry 0.2.2, tree 0.2.3. explicit-path and telemetry-off initialiser open (#103) |
| HeardRight | `rightkit-overlay` | N | Replace local pill hitbox/hover/placement primitives with rightkit_overlay | BLOCKED | #23, #64 | never published, tree 0.1.1 (staged publishing). unpublished; macOS backend and Windows layered helper open |
| Pulse | `rightkit-overlay` | N | Adopt cargo:rightkit-overlay at owning runtime boundary, preserve Pulse policy/UI, route callers... | BLOCKED | #23, #64 | never published, tree 0.1.1 (staged publishing). unpublished; macOS backend and Windows layered helper open |
| ScrapeRight | `rightkit-pdf` | M | Choose rightkit_pdf::extract::extract_text as primary only after it covers per-page raster/OCR, or... | RULED-LOCAL | #42 | registry 0.2.0, tree 0.2.1 (staged 0.2.0). PDFium rendering stays app-side until rightkit-pdfium (#42); extract_text swap not ruled. |
| CodeRight | `rightkit-process` | M | Replace only cli_presence::spawn_detached_daemon with OwnedCommand::spawn_uncontained_detached after... | BLOCKED | #16, #52, #83, #101, #107, #112 | registry 0.3.3, tree 0.3.4 (staged 0.3.3). 0.3.4 safety release and parent-death tie open |
| ViewRight | `rightkit-process` | N | local OCR supervision | BLOCKED | #16, #52, #83, #101, #107, #112 | registry 0.3.3, tree 0.3.4 (staged 0.3.3). 0.3.4 safety release and parent-death tie open |
| Pulse | `rightkit-process` | N | Adopt cargo:rightkit-process at owning runtime boundary, preserve Pulse policy/UI, route callers... | BLOCKED | #16, #52, #83, #101, #107, #112 | registry 0.3.3, tree 0.3.4 (staged 0.3.3). 0.3.4 safety release and parent-death tie open |
| Legion | `rightkit-process` | N | Adopt rightkit-process child containment beneath Legion effect policy & receipts | BLOCKED | #16, #52, #83, #101, #107, #112 | registry 0.3.3, tree 0.3.4 (staged 0.3.3). 0.3.4 safety release and parent-death tie open |
| MailRight | `rightkit-qa` | N | Wire MailRight frontend/native suites into rightkit-qa harness, preserving hidden QA data-dir... | BLOCKED | #18, #19, #56, #74, #84, #91, #92 | registry 0.2.14, tree 0.2.16 (staged 0.2.15). 0.2.16 regression fix and bin distribution open |
| Legion | `rightkit-qa` | N | Port Legion browser-QA runner onto rightkit-qa | BLOCKED | #18, #19, #56, #74, #84, #91, #92 | registry 0.2.14, tree 0.2.16 (staged 0.2.15). 0.2.16 regression fix and bin distribution open |
| VoiceRight | `rightkit-qts` | N | Replace eval/qts-upstream with published rightkit-qts | UNKNOWN | none | registry 0.2.4, tree 0.2.6 (staged 0.2.5); aarch64-apple-darwin only per PLATFORM-MATRIX. no ledger row; tree newer; x86_64 macOS bindings missing (PLATFORM-MATRIX) |
| VoiceRight | `rightkit-qts-ggml` | N | After qts adoption, remove local qts_ggml path and verify Vulkan/CPU behavior | UNKNOWN | none | registry 0.1.1, tree 0.1.2 (staged 0.1.2). no ledger row; depends on rightkit-qts adoption |
| VoiceRight | `rightkit-qts-ggml-sys` | N | After qts adoption, remove local qts_ggml_sys path and verify native linkage | UNKNOWN | none | registry 0.1.1, tree 0.1.2 (staged 0.1.2). no ledger row; depends on rightkit-qts adoption |
| Legion | `rightkit-sandbox` | N | Use rightkit-sandbox for process isolation | UNKNOWN | none | registry 0.3.3, tree 0.3.4 (staged 0.3.4). no ledger row; spawn_pty with policy unsupported (PLATFORM-MATRIX) |
| RustRight | `rightkit-sandbox` | N | Evaluate mapping compile-unit scheduler to rightkit-sandbox ResourceAuthority | UNKNOWN | none | registry 0.3.3, tree 0.3.4 (staged 0.3.4). no ledger row; spawn_pty with policy unsupported (PLATFORM-MATRIX) |
| CodeRight | `rightkit-search` | N | Compare local rightkit-search with owner API | BLOCKED | #50, #101 | registry 0.2.0 = tree 0.2.0. 0.3.0 subquery/ordering and public fts_term open |
| MailRight | `rightkit-search` | N | Use rightkit-search lexer/AST/predicate compiler behind MailRight Gmail field mapping | BLOCKED | #50, #101 | registry 0.2.0 = tree 0.2.0. 0.3.0 subquery/ordering and public fts_term open |
| ScrapeRight | `rightkit-search` | N | Define a transcript index with rightkit_search::sql::Schema::fts, then replace Store::search LIKE... | BLOCKED | #50, #101 | registry 0.2.0 = tree 0.2.0. 0.3.0 subquery/ordering and public fts_term open |
| HeardRight | `rightkit-secrets` | N | Route license/device/DB-key persistence through rightkit_secrets::Store | BLOCKED | #58, #82, #103 | registry 0.1.2, tree 0.1.4 (HeardRight runs 0.1.3 per #79). 0.1.4 default-keychain fix held for Adrian |
| Legion | `rightkit-secrets` | N | Store OAuth refresh tokens via rightkit-secrets | BLOCKED | #58, #82, #103 | registry 0.1.2, tree 0.1.4 (HeardRight runs 0.1.3 per #79). 0.1.4 default-keychain fix held for Adrian |
| Pulse | `rightkit-send` | N | Adopt cargo:rightkit-send at owning runtime boundary, preserve Pulse policy/UI, route callers through... | BLOCKED | #14, #63, #97, #100, #106 | private (publish=false), tree 0.1.0. tree-only, unpublished; partial-file and trust-store gaps |
| RustRight | `rightkit-service` | N | Assess adapting broker transport to rightkit-service while preserving immutable v2 protocol, endpoint... | BLOCKED | #59, #66, #113 | registry 0.1.3, tree 0.1.5 (staged 0.1.4). 0.1.5 peer verification and pipe limits open |
| CodeRight | `rightkit-settings` | N | Compare local rightkit-settings with owner API | BLOCKED | #33, #53, #73, #77 | registry 0.1.0, tree 0.2.0 (published 0.1.1 per lead). 0.2.0 additions (open_path, flat mode, read-only) open |
| HeardRight | `rightkit-settings` | N | Map SettingsBlob schema & migrations to rightkit_settings::Store | BLOCKED | #33, #53, #73, #77 | registry 0.1.0, tree 0.2.0 (published 0.1.1 per lead). 0.2.0 additions (open_path, flat mode, read-only) open |
| ViewRight | `rightkit-settings` | N | schema/migrations/mirror local | BLOCKED | #33, #53, #73, #77 | registry 0.1.0, tree 0.2.0 (published 0.1.1 per lead). 0.2.0 additions (open_path, flat mode, read-only) open |
| MailRight | `rightkit-settings` | N | Adopt rightkit-settings migration/default/change-event core behind MailRight SQLCipher adapter | BLOCKED | #33, #53, #73, #77 | registry 0.1.0, tree 0.2.0 (published 0.1.1 per lead). 0.2.0 additions (open_path, flat mode, read-only) open |
| Pulse | `rightkit-settings` | N | Adopt cargo:rightkit-settings at owning runtime boundary, preserve Pulse policy/UI, route callers... | BLOCKED | #33, #53, #73, #77 | registry 0.1.0, tree 0.2.0 (published 0.1.1 per lead). 0.2.0 additions (open_path, flat mode, read-only) open |
| Ember | `rightkit-settings` | N | Map Preferences load/patch/quarantine to rightkit-settings Store/FileBackend | BLOCKED | #33, #53, #73, #77 | registry 0.1.0, tree 0.2.0 (published 0.1.1 per lead). 0.2.0 additions (open_path, flat mode, read-only) open |
| HeardRight | `rightkit-shell` | N | Replace local Tauri startup/tray/single-instance/window shell with rightkit_shell::ShellBuilder/modules | BLOCKED | #31, #39, #44, #51, #62, #73, #95, #101, #113, #115 | registry 0.1.4, tree 0.3.0 (staged 0.2.0). 0.3.0 additions open; tray gaps (#101) |
| ScrapeRight | `rightkit-shell` | N | Map EngineSupervisor/window protocol, native menu & lifecycle decisions onto... | BLOCKED | #31, #39, #44, #51, #62, #73, #95, #101, #113, #115 | registry 0.1.4, tree 0.3.0 (staged 0.2.0). 0.3.0 additions open; tray gaps (#101) |
| CutRight | `rightkit-shell` | N | Add shared rightkit-shell host handler for this three-action window subset, then remove direct... | BLOCKED | #31, #39, #44, #51, #62, #73, #95, #101, #113, #115 | registry 0.1.4, tree 0.3.0 (staged 0.2.0). 0.3.0 additions open; tray gaps (#101) |
| GenRight | `rightkit-shell` | N | Adopt rightkit-shell for hardened Tauri/window/menu/engine plumbing, preserve GenRight commands &... | BLOCKED | #31, #39, #44, #51, #62, #73, #95, #101, #113, #115 | registry 0.1.4, tree 0.3.0 (staged 0.2.0). 0.3.0 additions open; tray gaps (#101) |
| VoiceRight | `rightkit-shell` | N | Adopt rightkit-shell for Tauri shell wiring | BLOCKED | #31, #39, #44, #51, #62, #73, #95, #101, #113, #115 | registry 0.1.4, tree 0.3.0 (staged 0.2.0). 0.3.0 additions open; tray gaps (#101) |
| HeardRight | `rightkit-sync` | N | Replace local cloud sync ledger/pull/push engine with rightkit_sync adapter | BLOCKED | #43, #71 | never published, tree 0.1.1 (staged 0.1.0). unpublished; upload ordering and account-scope guard open |
| HeardRight | `rightkit-transcript` | N | Replace local history_export formatters with rightkit_transcript export/parse APIs | BLOCKED | #30, #73, #95 | registry 0.2.0, tree 0.3.0 (staged 0.2.0). 0.3.0 SourceBoundary and strict parse open |
| ScrapeRight | `rightkit-transcript` | N | Map TimedTok/TimedWord semantics to rightkit_transcript::to_srt/to_vtt/to_timing_json, then replace... | BLOCKED | #30, #73, #95 | registry 0.2.0, tree 0.3.0 (staged 0.2.0). 0.3.0 SourceBoundary and strict parse open |
| GenRight | `rightkit-transcript` | N | Use rightkit-transcript for SRT/VTT parsing/export & timing, preserve caption policy/FFmpeg... | BLOCKED | #30, #73, #95 | registry 0.2.0, tree 0.3.0 (staged 0.2.0). 0.3.0 SourceBoundary and strict parse open |
| VoiceRight | `rightkit-tts` | N | Adopt rightkit-tts or document retained product-specific adapter | BLOCKED | #5, #27, #88 | registry 0.1.0, tree 0.2.0 (published 0.1.1 per lead). 0.2.0 voice stack is tree-only |
| HeardRight | `rightkit-vad` | N | Map SpeechVad thresholds/frame carry/backend adapter to rightkit_vad::detect | BLOCKED | #32, #73, #95 | registry 0.0.2, tree 0.1.0 (staged 0.0.3). 0.1.0 Silero-from-bytes and CoreML scorer open |
| Pulse | `rightkit-watch` | N | Adopt cargo:rightkit-watch at owning runtime boundary, preserve Pulse policy/UI, route callers... | BLOCKED | #109 | registry 0.3.1, tree 0.3.3 (staged 0.3.2). Pulse acceptance list open |
| Ember | `rightkit-watch` | N | Replace AutoImport::poll with rightkit-watch FileSetWatcher only after preserving generation... | BLOCKED | #109 | registry 0.3.1, tree 0.3.3 (staged 0.3.2). Pulse acceptance list open |
| CodeRight | `@rightkit/app-shell` | N | Compare local @rightkit/app-shell with owner API | RULED-LOCAL | #96, #101 | registry = tree 0.6.2. Locked-UI ruling: keep own markup, behaviour via platform-ui headless hooks (#96); #101 item 1 follows it. |
| MailRight | `@rightkit/app-shell` | N | Migrate frame/navigation/shortcut/toast primitives to app-shell | BLOCKED | #39, #48, #55, #90, #94, #96, #97, #98, #101, #104 | registry = tree 0.6.2. next-minor caption/layer/slot items open |
| VoiceRight | `@rightkit/app-shell` | N | Adopt app-shell primitives | BLOCKED | #39, #48, #55, #90, #94, #96, #97, #98, #101, #104 | registry = tree 0.6.2. next-minor caption/layer/slot items open |
| WorkRight | `@rightkit/app-shell` | N | Adopt @rightkit/app-shell where shared frame primitives fit | BLOCKED | #39, #48, #55, #90, #94, #96, #97, #98, #101, #104 | registry = tree 0.6.2. next-minor caption/layer/slot items open |
| Legion | `@rightkit/ax` | N | Call @rightkit/ax portable-core from release assembly or isolate a versioned bridge | READY | none | registry = tree 0.2.4. no ledger row |
| MailRight | `@rightkit/doc-render` | N | Use HtmlRenderer/sanitizeHtml for untrusted email content | READY | none | registry = tree 0.1.6. no ledger row |
| ScrapeRight | `@rightkit/doc-render` | N | Replace mounted MarkdownBody with MarkdownRenderer/renderMarkdown only after transcript markdown,... | READY | none | registry = tree 0.1.6. no ledger row |
| CodeRight | `@rightkit/i18n` | N | Compare local @rightkit/i18n with owner API | READY | none | registry = tree 0.1.0. no ledger row |
| Ember | `@rightkit/i18n` | N | Replace i18n.ts normalizeLocale/translate with @rightkit/i18n createI18n | READY | none | registry = tree 0.1.0. no ledger row |
| CodeRight | `@rightkit/legal-ui` | N | Compare local @rightkit/legal-ui with owner API | BLOCKED | #55, #85, #101 | registry = tree 0.2.2. visible legend and eligibility options open |
| MailRight | `@rightkit/legal-ui` | N | Migrate gate/acceptance visuals to legal-ui | BLOCKED | #55, #85, #101 | registry = tree 0.2.2. visible legend and eligibility options open |
| MailRight | `@rightkit/media-ui` | N | Adopt shared image/attachment viewer surface | BLOCKED | #97 | registry = tree 0.3.4. image stage focus open (U01) |
| VoiceRight | `@rightkit/media-ui` | N | Adopt media-ui player for WAV preview | BLOCKED | #97 | registry = tree 0.3.4. image stage focus open (U01) |
| Ember | `@rightkit/media-ui` | N | Evaluate an adapter for PhotoPreview's base viewing/handoff behavior only | BLOCKED | #97 | registry = tree 0.3.4. image stage focus open (U01) |
| CodeRight | `@rightkit/onboarding` | N | Compare local @rightkit/onboarding with owner API | READY | none | registry = tree 0.3.2. no ledger row |
| ViewRight | `@rightkit/pdf-ui` | M | route read-only page rendering/search/thumbnail lifecycle through PdfViewer/usePdfSearch or document... | BLOCKED | #55, #85 | registry = tree 0.1.5. surface CSS variables, wasmUrl, truncation notice open |
| MailRight | `@rightkit/pdf-ui` | N | Adopt PdfViewer worker/password/limit configuration | BLOCKED | #55, #85 | registry = tree 0.1.5. surface CSS variables, wasmUrl, truncation notice open |
| HeardRight | `@rightkit/platform-ui` | M | Remove local getPlatform/formatChord implementations & route shortcut display, platform capabilities,... | BLOCKED | #48, #85, #89, #96, #97, #101, #104 | registry = tree 0.2.6. headless focus-trap options and plugin-os lazy load open |
| MailRight | `@rightkit/platform-ui` | N | Map shared platform/shortcut primitives around MailRight Gmail action registry | BLOCKED | #48, #85, #89, #96, #97, #101, #104 | registry = tree 0.2.6. headless focus-trap options and plugin-os lazy load open |
| VoiceRight | `@rightkit/platform-ui` | N | Adopt platform-ui detection/window controls | BLOCKED | #48, #85, #89, #96, #97, #101, #104 | registry = tree 0.2.6. headless focus-trap options and plugin-os lazy load open |
| HeardRight | `@rightkit/qa` | N | Replace local CDP driver, journey plumbing, ownership/cleanup, & native QA guards with @rightkit/qa APIs | BLOCKED | #18, #105, #114 | registry = tree 0.4.2. capture-comparison gate and installed-app capture open |
| ViewRight | `@rightkit/qa` | N | Route generic CDP/session, workspace, and evidence lifecycle through @rightkit/qa | BLOCKED | #18, #105, #114 | registry = tree 0.4.2. capture-comparison gate and installed-app capture open |
| MailRight | `@rightkit/qa` | N | Use shared QA loopback/evidence/guards from MailRight scripts | BLOCKED | #18, #105, #114 | registry = tree 0.4.2. capture-comparison gate and installed-app capture open |
| CodeRight | `@rightkit/settings-ui` | N | Compare rendered SettingsModal/SettingsPaneContent sections with SettingsPanel, preserve... | BLOCKED | #48, #90, #97, #104, #115 | registry = tree 0.4.0. search context, keepMounted, reload guard open |
| ViewRight | `@rightkit/settings-ui` | N | Adopt shared settings frame/group/row where compatible | BLOCKED | #48, #90, #97, #104, #115 | registry = tree 0.4.0. search context, keepMounted, reload guard open |
| MailRight | `@rightkit/settings-ui` | N | Adopt SettingsPanel for generic appearance/updates/license/legal/diagnostics | BLOCKED | #48, #90, #97, #104, #115 | registry = tree 0.4.0. search context, keepMounted, reload guard open |
| Pulse | `@rightkit/settings-ui` | N | Adopt npm:@rightkit/settings-ui at owning runtime boundary, preserve Pulse policy/UI, route callers... | BLOCKED | #48, #90, #97, #104, #115 | registry = tree 0.4.0. search context, keepMounted, reload guard open |
| HeardRight | `@rightkit/shell` | N | Replace local tauriBridge transport/window commands with @rightkit/shell createShell | BLOCKED | #48 | registry = tree 0.1.0. settings-ui adapter needs shell (ViewRight) |
| ViewRight | `@rightkit/shell` | N | direct menu bridge local | BLOCKED | #48 | registry = tree 0.1.0. settings-ui adapter needs shell (ViewRight) |
| MailRight | `@rightkit/shell` | N | Migrate TitleBar & local shell IPC to createShell | BLOCKED | #48 | registry = tree 0.1.0. settings-ui adapter needs shell (ViewRight) |
| CutRight | `@rightkit/shell` | N | Construct @rightkit/shell createShell from shared host bridge, route AppShell caption actions through... | BLOCKED | #48 | registry = tree 0.1.0. settings-ui adapter needs shell (ViewRight) |
| VoiceRight | `@rightkit/shell` | N | Adopt @rightkit/shell typed bridge | BLOCKED | #48 | registry = tree 0.1.0. settings-ui adapter needs shell (ViewRight) |
| CodeRight | `@rightkit/theme` | N | Compare local @rightkit/theme with owner API | BLOCKED | #48, #55, #98 | registry = tree 0.2.3. suite-alias opt-out and store hook open |
| MailRight | `@rightkit/theme` | N | Feed MailRight brand presets into shared theme runtime/checkers | BLOCKED | #48, #55, #98 | registry = tree 0.2.3. suite-alias opt-out and store hook open |
| ScrapeRight | `@rightkit/theme` | N | Map each locked ScrapeRight palette/font token onto applyTokens or BrandFile/applyBrand without... | BLOCKED | #48, #55, #98 | registry = tree 0.2.3. suite-alias opt-out and store hook open |
| Pulse | `@rightkit/theme` | N | Adopt npm:@rightkit/theme at owning runtime boundary, preserve Pulse policy/UI, route callers through... | BLOCKED | #48, #55, #98 | registry = tree 0.2.3. suite-alias opt-out and store hook open |
| Ember | `@rightkit/theme` | N | Adopt @rightkit/theme token/runtime APIs for Ember colors and dark mode or document product-owned... | BLOCKED | #48, #55, #98 | registry = tree 0.2.3. suite-alias opt-out and store hook open |
| Pulse | `RightKitDiagnostics` | N | Adopt swift:RightKitDiagnostics at owning runtime boundary, preserve Pulse policy/UI, route callers... | BLOCKED | #63, #101 | published 0.4.0 (SwiftPM), tree 0.6.0 untagged. RightKitSwift access (#63); redactor mode (#101 item 6) |
| HeardRight | `RightKitArtifacts` | N | Replace ModelArtifactInstaller calls with RightKitArtifacts.ArtifactInstaller | BLOCKED | #75 | published 0.4.0 (SwiftPM), tree 0.6.0 untagged. ArtifactInstaller fail-closed and recovery open (0.6.0) |
| Legion | `RightKitArtifacts` | N | Package assembled payload as digest-pinned `.tar.gz` with one stable top-level directory | BLOCKED | #75 | published 0.4.0 (SwiftPM), tree 0.6.0 untagged. ArtifactInstaller fail-closed and recovery open (0.6.0) |
| CodeRight | `RightKitImportQueue` | N | Compare local RightKitImportQueue with owner API | UNKNOWN | none | published 0.4.0 (SwiftPM), tree 0.6.0 untagged. no ledger row; tree 0.6.0 untagged |
| CodeRight | `RightKitLiveActivity` | N | Compare local RightKitLiveActivity with owner API | BLOCKED | #101 | published 0.4.0 (SwiftPM), tree 0.6.0 untagged. LiveActivityHost.start ends other activities (#101 item 7) |
| CodeRight | `RightKitSpeech` | N | Compare local RightKitSpeech with owner API | UNKNOWN | none | published 0.4.0 (SwiftPM), tree 0.6.0 untagged. no ledger row; tree 0.6.0 untagged |
| ScrapeRight | `RightKitSpeech` | N | Add RightKitSpeech product to ScrapeRight target, adapt MediaTranscriber to... | UNKNOWN | none | published 0.4.0 (SwiftPM), tree 0.6.0 untagged. no ledger row; tree 0.6.0 untagged |
| CodeRight | `RightKitAppleCredentials` | N | Compare local RightKitAppleCredentials with owner API | BLOCKED | #63 | published 0.4.0 (SwiftPM), tree 0.6.0 untagged. RightKitSwift mirror private; credentials gaps open |
| Pulse | `RightKitAppleCredentials` | N | Adopt swift:RightKitAppleCredentials at owning runtime boundary, preserve Pulse policy/UI, route... | BLOCKED | #63 | published 0.4.0 (SwiftPM), tree 0.6.0 untagged. RightKitSwift mirror private; credentials gaps open |
| Pulse | `RightKitOverlay` | N | Adopt swift:RightKitOverlay at owning runtime boundary, preserve Pulse policy/UI, route callers... | BLOCKED | #63 | published 0.4.0 (SwiftPM), tree 0.6.0 untagged. RightKitSwift access and per-rect click-through (#63) |
| Pulse | `RightKitWatch` | N | Adopt swift:RightKitWatch at owning runtime boundary, preserve Pulse policy/UI, route callers through... | BLOCKED | #63 | published 0.4.0 (SwiftPM), tree 0.6.0 untagged. RightKitSwift access; FSEvents product (#63) |
| Pulse | `RightKitUI` | N | Adopt swift:RightKitUI at owning runtime boundary, preserve Pulse policy/UI, route callers through... | BLOCKED | #63 | published 0.4.0 (SwiftPM), tree 0.6.0 untagged. RightKitSwift private mirror (#63) |

## 1. Per component: blocker counts and the unblocking action

Counts are cells across apps. The action is the single RightKit step that clears the component's open rows.

| Component | Cells | READY | BLOCKED | RULED-LOCAL | UNKNOWN | Single RightKit action |
|---|---:|---:|---:|---:|---:|---|
| `rightkit-http` | 7 | 0 | 7 | 0 | 0 | Land rightkit-http 0.3.0 P1 fixes (#86), then publish multipart/resume, address policy, proxy (#4, #40, #49). |
| `rightkit-settings` | 6 | 0 | 6 | 0 | 0 | Publish rightkit-settings 0.2.0 (#33, #53, #73, #77). |
| `rightkit-fs` | 5 | 0 | 5 | 0 | 0 | Publish rightkit-fs 0.3.4 additions (#73, #106, #111). |
| `rightkit-logs` | 5 | 0 | 5 | 0 | 0 | Publish rightkit-logs 0.2.2 tracing feature and checked init (#41, #77, #108). |
| `rightkit-models` | 5 | 0 | 5 | 0 | 0 | Ship rightkit-models Whisper entry and suite comment fixes (#27, #78, #99); grant decision is Adrian's (#99). |
| `rightkit-shell` | 5 | 0 | 5 | 0 | 0 | Publish rightkit-shell 0.3.0 second scheme, typed single-instance, lifecycle entry point (#44, #62, #113). |
| `@rightkit/shell` | 5 | 0 | 5 | 0 | 0 | Ship settings-ui adapter interface without @rightkit/shell (#48). |
| `@rightkit/theme` | 5 | 0 | 5 | 0 | 0 | Ship theme opt-out and store legacy-migration hook (#48, #98). |
| `rightkit-process` | 4 | 0 | 4 | 0 | 0 | Publish rightkit-process 0.3.4 safety release (#107) and parent-death tie (#52, #83). |
| `@rightkit/app-shell` | 4 | 0 | 3 | 1 | 0 | Ship app-shell next minor: lights slot, caption options, layered stylesheet (#90, #94, #98). |
| `@rightkit/settings-ui` | 4 | 0 | 4 | 0 | 0 | Ship settings-ui search context and keepMounted (#104, #90, #115). |
| `rightkit-db` | 3 | 0 | 3 | 0 | 0 | Publish rightkit-db 0.4.0 with legacy version-table import (#45, #95). |
| `rightkit-llm` | 3 | 0 | 3 | 0 | 0 | Publish rightkit-llm 0.2.0 with dated prices and CLI provider (#10, #16). |
| `rightkit-mcp` | 3 | 0 | 3 | 0 | 0 | Publish rightkit-mcp 0.6.0 (#51, #85). Stdio server adapter is not named by these rows. |
| `rightkit-search` | 3 | 0 | 3 | 0 | 0 | Ship rightkit-search 0.3.0 (#50) and public term sanitiser (#101). |
| `rightkit-transcript` | 3 | 0 | 3 | 0 | 0 | Publish rightkit-transcript 0.3.0 with SourceBoundary and strict-parse option (#30, #73). |
| `@rightkit/media-ui` | 3 | 0 | 3 | 0 | 0 | Fix image stage focusability and publish (#97). |
| `@rightkit/platform-ui` | 3 | 0 | 3 | 0 | 0 | Ship platform-ui headless options and lazy plugin-os (#48, #89, #96). |
| `@rightkit/qa` | 3 | 0 | 3 | 0 | 0 | Ship @rightkit/qa capture-comparison gate and installed-app capture (#105, #114). |
| `rightkit-asr` | 2 | 0 | 2 | 0 | 0 | Ship rightkit-asr 0.2.0 (detect_language, word timing, u64 timing, Whisper options; #24, #32, #78). |
| `rightkit-audio` | 2 | 2 | 0 | 0 | 0 | None; migrate now. |
| `rightkit-browser` | 2 | 0 | 2 | 0 | 0 | Ship rightkit-browser 0.3.0 cookie/UA/header API and pinned Chrome artifact (#25, #26). |
| `rightkit-crash` | 2 | 0 | 2 | 0 | 0 | Publish rightkit-crash 0.2.1 with guard reset and owner-only reports (#108). |
| `rightkit-framed-sidecar` | 2 | 0 | 2 | 0 | 0 | Publish framed-sidecar 0.2.0 deadline I/O and framing options (#60, #72, #95). |
| `rightkit-hotkeys` | 2 | 0 | 2 | 0 | 0 | Publish rightkit-hotkeys with the Windows backend compiled and tested (#22). |
| `rightkit-jobs` | 2 | 0 | 2 | 0 | 0 | Publish rightkit-jobs 0.2.8 request id, contract gate, non-blocking start (#31, #66, #112). |
| `rightkit-oauth` | 2 | 0 | 0 | 0 | 2 | Confirm registry 0.3.0 exposes OpenAI config and loopback flow, or publish 0.3.1. |
| `rightkit-ort` | 2 | 0 | 2 | 0 | 0 | Publish rightkit-ort 0.2.3 explicit-path candidate, env-only initialiser, DirectML knobs (#103, #35). |
| `rightkit-overlay` | 2 | 0 | 2 | 0 | 0 | Publish rightkit-overlay with macOS backend and layered-window helper (#23, #64). |
| `rightkit-qa` | 2 | 0 | 2 | 0 | 0 | Ship rightkit-qa 0.2.16 regression fix (#74), sidecar contract (#84), and bin distribution (#18). |
| `rightkit-sandbox` | 2 | 0 | 0 | 0 | 2 | Confirm registry 0.3.3 covers the audit's isolation API, or publish 0.3.4. |
| `rightkit-secrets` | 2 | 0 | 2 | 0 | 0 | Publish rightkit-secrets 0.1.4 default-keychain fix (#103) and Persist option (#82). |
| `rightkit-watch` | 2 | 0 | 2 | 0 | 0 | Land rightkit-watch 0.3.x Pulse acceptance items (#109). |
| `@rightkit/doc-render` | 2 | 2 | 0 | 0 | 0 | None; migrate now. |
| `@rightkit/i18n` | 2 | 2 | 0 | 0 | 0 | None; migrate now. |
| `@rightkit/legal-ui` | 2 | 0 | 2 | 0 | 0 | Ship legal-ui visible legend and onQuit optional (#85, #101 item 9). |
| `@rightkit/pdf-ui` | 2 | 0 | 2 | 0 | 0 | Ship pdf-ui CSS variables, wasmUrl pass-through, truncation notice (#55, #85). |
| `RightKitArtifacts` | 2 | 0 | 2 | 0 | 0 | Ship RightKitSwift 0.6.0 ArtifactInstaller fail-closed and recovery (#75). |
| `RightKitSpeech` | 2 | 0 | 0 | 0 | 2 | Confirm 0.4.0 exposes ParakeetEngine.transcribe(fileURL:) or publish 0.6.0. |
| `RightKitAppleCredentials` | 2 | 0 | 2 | 0 | 0 | Resolve RightKitSwift access for Pulse CI and foreign keychain reader (#63). |
| `rightkit-align` | 1 | 0 | 1 | 0 | 0 | Publish rightkit-align after CutRight RK4 items land (#38). |
| `rightkit-cache` | 1 | 0 | 1 | 0 | 0 | Ship rightkit-cache 0.2.0 (streaming put, private mode, scope for thumbnails; #33, #108). |
| `rightkit-channel-memory` | 1 | 0 | 1 | 0 | 0 | Publish after RK6 schema revision (#8) and record-level entry points (#89). |
| `rightkit-compose` | 1 | 0 | 1 | 0 | 0 | Land RK9 d/e (Sage design with CutRight) and publish (#36). |
| `rightkit-control` | 1 | 0 | 1 | 0 | 0 | Publish rightkit-control 0.2.0 and close Windows findings (#56, #74, #92). |
| `rightkit-embed` | 1 | 0 | 0 | 0 | 1 | Confirm registry 0.3.1 exposes the owner API cited by the audit or publish 0.3.2. |
| `rightkit-license` | 1 | 0 | 1 | 0 | 0 | Publish rightkit-license 0.2.0 after Adrian review (#47, #54, #77). |
| `rightkit-media` | 1 | 0 | 1 | 0 | 0 | Publish rightkit-media 0.2.6 with signature-independent digests and verified-resolver gaps (#67, #17, #27, #29); CutRight licence per Adrian (#28). |
| `rightkit-pdf` | 1 | 0 | 0 | 1 | 0 | Publish rightkit-pdf 0.2.1 (#21, #93); PDFium binding is a separate crate (#42). |
| `rightkit-qts` | 1 | 0 | 0 | 0 | 1 | Confirm registry 0.2.4 covers the audit's API and platform, or publish 0.2.6. |
| `rightkit-qts-ggml` | 1 | 0 | 0 | 0 | 1 | Follows rightkit-qts; no separate ledger action. |
| `rightkit-qts-ggml-sys` | 1 | 0 | 0 | 0 | 1 | Follows rightkit-qts; no separate ledger action. |
| `rightkit-send` | 1 | 0 | 1 | 0 | 0 | Publish rightkit-send after partial-file fix and trust-store overlap (#14, #97). |
| `rightkit-service` | 1 | 0 | 1 | 0 | 0 | Publish rightkit-service 0.1.5 peer verification and include_incomplete (#59, #66). |
| `rightkit-sync` | 1 | 0 | 1 | 0 | 0 | Publish rightkit-sync with depends_on hint and origin-scope guard (#43, #71). |
| `rightkit-tts` | 1 | 0 | 1 | 0 | 0 | Ship rightkit-tts 0.2.0 voice stack (#5, #88). |
| `rightkit-vad` | 1 | 0 | 1 | 0 | 0 | Publish rightkit-vad 0.1.0 Silero from bytes, pool/EP options, CoreML scorer (#32). |
| `@rightkit/ax` | 1 | 1 | 0 | 0 | 0 | None; migrate now. |
| `@rightkit/onboarding` | 1 | 1 | 0 | 0 | 0 | None; migrate now. |
| `RightKitDiagnostics` | 1 | 0 | 1 | 0 | 0 | Resolve RightKitSwift access (#63); add redactor debug mode (#101). |
| `RightKitImportQueue` | 1 | 0 | 0 | 0 | 1 | Confirm 0.4.0 exposes the import queue API the audit cites. |
| `RightKitLiveActivity` | 1 | 0 | 1 | 0 | 0 | Fix LiveActivityHost.start behaviour (#101 item 7). |
| `RightKitOverlay` | 1 | 0 | 1 | 0 | 0 | Resolve RightKitSwift access and per-rect click-through (#63). |
| `RightKitWatch` | 1 | 0 | 1 | 0 | 0 | Resolve RightKitSwift access and publish RightKitWatch FSEvents product (#63). |
| `RightKitUI` | 1 | 0 | 1 | 0 | 0 | Resolve RightKitSwift access for Pulse CI (#63). |

## 2. N/M cells with no ledger row at all

Cells whose component is named by no ledger row, open or closed. `rightkit-audio` is excluded: row #98 names it only as a publication note.

| Component | Cells | Apps (code) |
|---|---:|---|
| `rightkit-oauth` | 2 | CodeRight M, Legion N |
| `rightkit-sandbox` | 2 | Legion N, RustRight N |
| `@rightkit/doc-render` | 2 | MailRight N, ScrapeRight N |
| `@rightkit/i18n` | 2 | CodeRight N, Ember N |
| `RightKitSpeech` | 2 | CodeRight N, ScrapeRight N |
| `rightkit-embed` | 1 | CodeRight N |
| `rightkit-qts` | 1 | VoiceRight N |
| `rightkit-qts-ggml` | 1 | VoiceRight N |
| `rightkit-qts-ggml-sys` | 1 | VoiceRight N |
| `@rightkit/ax` | 1 | Legion N |
| `@rightkit/onboarding` | 1 | CodeRight N |
| `RightKitImportQueue` | 1 | CodeRight N |

Total: 17 cells across 12 components. Six of the 17 are READY (`@rightkit/ax`, `@rightkit/doc-render`, `@rightkit/i18n`, `@rightkit/onboarding`); the other 11 are UNKNOWN. No component has more than two such cells: five have two and seven have one. All 12 are listed.

## 3. Ledger rows the audit contradicts

Section A: audit says Y, ledger says the app could not adopt (or a gap blocks it).

| # | App | Component | Audit citation | Ledger citation | Strength |
|---|---|---|---|---|---|
| A1 | ViewRight | `@rightkit/app-shell` | matrix.csv Y; handoffs/viewright.md | #94: "None of ContextMenu, ModalShell/Dialog, CommandPalette, ToastViewport can be adopted with identical output… ViewRight keeps the four local"; #96 header: "ToastViewport adopted, three families not" | strong |
| A2 | ViewRight | `rightkit-license` | matrix.csv Y | #47 (in progress): "no shared owner for lifecycle classification… anti-rollback clock… refresh-and-reanchor" | medium |
| A3 | ScrapeRight | `rightkit-logs` | matrix.csv Y | #41 (in progress): "apps' tracing + tracing-appender logging has nothing to move to" | medium-strong |
| A4 | ScrapeRight | `rightkit-settings` | matrix.csv Y | #53 (in progress): "cannot open a Store at an explicit file path"; #77: "first save skips the write… apps need the file created" | medium |
| A5 | ScrapeRight | `rightkit-license` | matrix.csv Y | #54 (in progress): "no lease-envelope verify primitive… legacy v1 verify stays app-side" | medium |
| A6 | ScrapeRight | `rightkit-asr` | matrix.csv Y | #24 (in progress): "detect_language=false hard-coded"; #78 (in progress): "decode params hard-coded… transcripts differ" | medium |
| A7 | CutRight | `rightkit-settings` | matrix.csv Y | #33 (open): "namespaced envelope vs CutRight's flat settings.json read directly by framing-approval and captions; patch rejects open keys" | strong |
| A8 | CutRight | `rightkit-media` | matrix.csv Y | #28 (Adrian decision): "Blocks the runner/probe swap"; #29 (in progress): "probe parses ffmpeg -i stderr; CutRight needs ffprobe JSON" | strong |
| A9 | CutRight | `rightkit-asr` | matrix.csv Y | #32 (open): "times are f32 seconds… CutRight needs u64 samples for word-edge cuts" | medium-strong |
| A10 | CutRight | `rightkit-vad` | matrix.csv Y | #32 (open): "Silero loads from a path only (CutRight needs embedded bytes, own pool, CPU EP)" | medium |
| A11 | CutRight | `rightkit-ort` | matrix.csv Y | #35 (in progress): "lacks directml_sequential / directml_memory_pattern" | medium |
| A12 | CutRight | `rightkit-transcript` | matrix.csv Y | #30 (in progress): "EndSource lacks SourceBoundary… existing CutRight projects fail to parse" | strong |
| A13 | CutRight | `rightkit-cache` | matrix.csv Y | #33 (open): "rightkit-cache is disk bytes only (not thumbnails/frame cache/storage status)" | medium |
| A14 | CutRight | `rightkit-jobs` | matrix.csv Y | #31 (in progress): "on_event gives no request id… no contract_version gate"; #66: "supervisor port regressions" | medium |
| A15 | GenRight | `rightkit-tts` | matrix.csv Y | #88 (open): "rightkit-tts 0.2.0 cannot carry GenRight's voice stack"; #5 (in progress): "wire lacks mode/instruct/speaker" | strong |
| A16 | GenRight | `rightkit-media` | matrix.csv Y | #17 (open): "rejects the bundled ffmpeg… every installed Mac build reports ffmpeg missing"; #27 (open): "verified resolver gaps" | strong |
| A17 | GenRight | `rightkit-http` | matrix.csv Y | #4 (in progress): "lacks multipart builders and Range/resumable download"; #27 (open): "no cancellation hook" | medium |
| A18 | CodeRight | `rightkit-shell` | matrix.csv Y | #101 item 2 (open): "tray install/update only work inside the Shell plugin… Quit calls lifecycle::quit directly" | medium |

Section B: audit says N or M, ledger says the app adopted the component.

| # | App | Component | Audit citation | Ledger citation | Strength |
|---|---|---|---|---|---|
| B1 | HeardRight | `rightkit-db` | matrix.csv N; handoffs/heardright.md | #79 (closed, HeardRight confirmed): "installed build opens the existing SQLCipher history through rightkit-db 0.3.2" | strong |
| B2 | HeardRight | `rightkit-secrets` | matrix.csv N | #79: "with the key from rightkit-secrets 0.1.3"; #103 (open): first-wave port "rightkit-secrets 0.1.3 on macOS passes a null keychain" | strong |
| B3 | HeardRight | `rightkit-ort` | matrix.csv N | #103 (open): first-wave port; "(2) rightkit-ort 0.2.2 configure_runtime_with_options rejects relative paths" | medium |
| B4 | Pulse | `rightkit-fs` | matrix.csv N; handoffs/pulse.md | #106 (open): "adopted rightkit-fs 0.3.3 at about 20 atomic-write sites" (partial adoption) | strong |
| B5 | Pulse | `rightkit-logs` | matrix.csv N | #108 (open): "adopted rightkit-crash 0.2.1 panic hook and rightkit-logs 0.2.1 JsonlStore" | strong |
| B6 | MailRight | `@rightkit/settings-ui` | matrix.csv N; handoffs/mailright.md | #90 header: "settings frame on settings-ui 0.3.4 landed, 4a5df8e" | strong |
| B7 | MailRight | `@rightkit/app-shell` | matrix.csv N | #90 header: "Titlebar on app-shell 0.5.1 landed, 4a5df8e" (partial: Titlebar only) | strong |
| B8 | MailRight | `@rightkit/pdf-ui` | matrix.csv N | #85 header: "pdf-ui 0.1.5 and legal-ui 0.2.2 landed, 18e0cc5" | medium |
| B9 | MailRight | `@rightkit/legal-ui` | matrix.csv N | #85 header (as B8) | medium |
| B10 | CodeRight | `RightKitLiveActivity` | matrix.csv N; handoffs/coderight.md | #101 header: "Added after adoption (… iOS … on RightKitSwift 0.4.0)", item (7) on `LiveActivityHost.start` (implied use) | medium |

Not strict pairs, listed for follow-up: Pulse `rightkit-crash` is NA while #108 says adopted; HeardRight `rightkit-media` is NA while #75 raises its probe and decode needs; ViewRight `rightkit-db` is NA while #45 raises its migrate() adoption path.

## Limits

- Registry-version API presence is not verifiable from the documents for tree-newer crates. Those cells are UNKNOWN, not READY, and the note names the missing fact.
- Component-level citation means a row raised by one app blocks the same component for others. The note says who raised it.
- Swift products (RightKitSwift 0.4.0 published, 0.6.0 tree and untagged): the docs do not show which API the 0.4.0 feed carries, except where a ledger row says so.
- Ledger row #92 is partly closed; its open items are cited.
