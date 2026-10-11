# ScrapeRight — RightKit migration handoff

Source root: /Volumes/D/claude/scraperight

Use this audit snapshot to plan cutovers; recheck cited callers before edits because other chats may have changed source. Select verified published versions; keep source cutover, packaging & execution qualification separate.

[Full app evidence](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/scraperight.json)

## rightkit-browser — N

Desktop article/browser extraction actively owns an agent-browser command session; no production caller uses rightkit_browser::BrowserSession::launch or new_page.

Prove BrowserSession::launch/new_page plus BrowserPage::eval/close meet article profile, resource blocking & cancellation; replace AgentBrowserSession transport after parity.

- Local: [agent browser module](/Volumes/D/claude/scraperight/crates/engine/src/browser/agent.rs:1) — Local agent-browser process/session code owns extraction
- Owner: [BrowserSession::launch](/Volumes/D/claude/rightkit/crates/rightkit-browser/src/session.rs:120) — Shared CDP session API; BrowserSession::new_page at line 330 owns page navigation.

## rightkit-http — M

fetch_article_with_fallbacks first calls fetch_article_http, whose public_web_client sends through rightkit_http::Client::stream_any; a failed direct/browser-header route still invokes local TLS-fingerprint transport before browser fallback.

Keep Client::stream_any primary; prove fingerprinted TLS behavior through a RightKit HTTP API or remove fetch_article_tls_fingerprint from fallback order.

- Local: [fetch_article_tls_fingerprint](/Volumes/D/claude/scraperight/crates/engine/src/extraction/article_tls.rs:12) — Active fallback uses local tls-client FFI/custom TLS
- Shared: [fetch_article_http](/Volumes/D/claude/scraperight/crates/engine/src/extraction/article.rs:121) — Live primary article route calls public_web_client(...).stream_any(Request::get(...)).
- Owner: [Client::stream_any](/Volumes/D/claude/rightkit/crates/rightkit-http/src/client.rs:846) — Shared streamed request API used by ScrapeRight direct article fetch.

Delivery: Root Cargo.toml [patch.crates-io] points RightKit crates at local paths; publish/version-pin before release.

## rightkit-jobs — N

Desktop engine owns durable queue.json recovery, worker admission & state transitions; no production caller constructs rightkit_jobs::JobStore or Runner.

Map Queue recovery, entitlement gates & EngineEvent framing onto JobStore::open plus Runner::new/run; replace queue.json worker only after transition parity.

- Local: [Queue](/Volumes/D/claude/scraperight/crates/engine/src/queue.rs:32) — App owns queue state, worker registry, recovery & queue.json
- Owner: [Runner::new](/Volumes/D/claude/rightkit/crates/rightkit-jobs/src/runner.rs:289) — Shared durable-job runner; JobStore::open begins at store.rs:316.

## rightkit-pdf — M

process_pdf uses app PDFium per-page text/raster/OCR whenever PDFium binds; only PDFium-bind failure reaches rightkit_pdf::extract::extract_text, so both extraction implementations remain live.

Choose rightkit_pdf::extract::extract_text as primary only after it covers per-page raster/OCR, or retain PDFium as product pipeline & remove unreachable fallback duplication.

- Local: [process_pdf](/Volumes/D/claude/scraperight/crates/engine/src/extraction_documents.rs:40) — Primary path uses app-owned PDFium per-page text/raster/OCR
- Shared: [rightkit_pdf::extract::extract_text](/Volumes/D/claude/scraperight/crates/engine/src/extraction_documents.rs:54) — PDFium-unavailable fallback calls shared PDF extractor
- Owner: [extract_text](/Volumes/D/claude/rightkit/crates/rightkit-pdf/src/extract.rs:24) — Shared PDF text extraction fallback

Delivery: Root Cargo.toml [patch.crates-io] points RightKit crates at local paths; publish/version-pin before release.

## rightkit-search — N

Desktop history search actively executes Store::search's SQLite LIKE query/result shaping; no caller builds rightkit_search::sql::Schema or invokes Schema::compile.

Define a transcript index with rightkit_search::sql::Schema::fts, then replace Store::search LIKE query after ranking/filter/result parity.

- Local: [Store::search](/Volumes/D/claude/scraperight/crates/store/src/lib.rs:316) — App owns SQLite LIKE search & result shaping
- Owner: [Schema::compile](/Volumes/D/claude/rightkit/crates/rightkit-search/src/sql.rs:134) — Shared FTS/query compiler API.

## rightkit-shell — N

Desktop Tauri shell actively owns engine supervision & window/bootstrap wiring; no production caller builds rightkit_shell::Shell or installs Shell::plugin.

Map EngineSupervisor/window protocol, native menu & lifecycle decisions onto Shell::builder(...).plugin only after feature-by-feature shell parity.

- Local: [EngineSupervisor::new](/Volumes/D/claude/scraperight/src-tauri/src/engine_supervisor.rs:343) — Tauri bootstrap/window/sidecar supervision is app-owned
- Owner: [Shell::builder](/Volumes/D/claude/rightkit/crates/rightkit-shell/src/lib.rs:195) — Shared Tauri shell builder; Shell::plugin installs it at line 455.

## rightkit-transcript — N

Desktop speech export actively renders local SRT/VTT/timing JSON via timed_exports; no caller uses rightkit_transcript::to_srt, to_vtt or to_timing_json.

Map TimedTok/TimedWord semantics to rightkit_transcript::to_srt/to_vtt/to_timing_json, then replace timed_exports only after timing/cue parity.

- Local: [render_subtitles](/Volumes/D/claude/scraperight/crates/speech/src/asr_window.rs:476) — Speech window locally renders timed subtitle/export output
- Owner: [to_srt](/Volumes/D/claude/rightkit/crates/rightkit-transcript/src/export.rs:42) — Shared SRT export; to_vtt is line 73 and to_timing_json is line 105.

## @rightkit/doc-render — N

Mounted DetailReader calls local MarkdownBody/stripMarkdown for markdown tabs; no production caller renders @rightkit/doc-render MarkdownRenderer or renderMarkdown.

Replace mounted MarkdownBody with MarkdownRenderer/renderMarkdown only after transcript markdown, link-opening & sanitization UX parity.

- Local: [MarkdownBody](/Volumes/D/claude/scraperight/src/views/DetailReader.tsx:369) — Reader owns markdown stripping/rendering/sanitization
- Owner: [MarkdownRenderer](/Volumes/D/claude/rightkit/packages/doc-render/src/renderers.tsx:103) — Shared parsed markdown React renderer; renderMarkdown wraps it at line 103.

## @rightkit/theme — N

Production main calls local applyTheme, whose paint resolves ScrapeRight presets, derives line shade & writes palette/font variables through document.documentElement.style; settings/desktop controls reuse it. @rightkit/theme supplies matching applyTokens/applyBrand APIs, but neither runtime API nor rightkit-theme generator is called by current source/scripts.

Map each locked ScrapeRight palette/font token onto applyTokens or BrandFile/applyBrand without changing design values; add a checked rightkit-theme generate route only if generated CSS becomes release-owned.

- Local: [paint](/Volumes/D/claude/scraperight/src/lib/theme.ts:107) — Live local theme runtime resolves presets, calls shade & writes root data-theme plus palette/font CSS variables.
- Local: [applyTheme(loadThemeChoice())](/Volumes/D/claude/scraperight/src/main.tsx:17) — Production bootstrap applies local palette before first React render.
- Owner: [applyTokens](/Volumes/D/claude/rightkit/packages/theme/src/tokens.ts:26) — Shared runtime API writes an app-owned CSS property record & restores previous values.
- Owner: [applyBrand](/Volumes/D/claude/rightkit/packages/theme/src/runtime.ts:22) — Shared brand runtime derives mode variables from BrandFile; optional suite aliases are explicit.

## RightKitSpeech — N

iOS MediaTranscriber actively constructs local ParakeetTranscriber/ParakeetRuntimeCache; project.yml omits RightKitSpeech, so no production caller constructs shared ParakeetEngine.

Add RightKitSpeech product to ScrapeRight target, adapt MediaTranscriber to ParakeetEngine.transcribe(fileURL:), then remove local runtime/cache after Core ML behavior parity.

- Local: [ParakeetRuntimeCache](/Volumes/D/claude/scraperight/ios-native/ScrapeRight/Extract/ParakeetTranscriber.swift:25) — iOS uses local ParakeetCoreMLRuntime/cache
- Owner: [ParakeetEngine](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitSpeech/SpeechEngines.swift:67) — Shared file/samples speech engine; transcribe(fileURL:) is line 110.

Before marking a task complete: migrate matching callers, remove only proven duplicate mechanisms, preserve product-specific schemas/policy/UX, then run affected target checks. Update this inventory from fresh evidence.
