# ViewRight — RightKit migration handoff

Source root: /Volumes/D/claude/viewright

Use this audit snapshot to plan cutovers; recheck cited callers before edits because other chats may have changed source. Select verified published versions; keep source cutover, packaging & execution qualification separate.

[Full app evidence](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/viewright.json)

## rightkit-crash — N

N; crash capture is local; migrate only if shared reports replace policy.

N; crash capture is local; migrate only if shared reports replace policy.

- Local: [std::panic::set_hook](/Volumes/D/claude/viewright/src-tauri/src/lib.rs:179) — local panic capture
- Owner: [install_panic_hook](/Volumes/D/claude/rightkit/crates/rightkit-crash/src/lib.rs:1) — shared redacted panic API

## rightkit-framed-sidecar — N

N; OCR framing/supervision is local; migrate after protocol proof.

N; OCR framing/supervision is local; migrate after protocol proof.

- Local: [WorkerResponse](/Volumes/D/claude/viewright/src-tauri/src/ocr_worker.rs:11) — local JSON-lines protocol
- Owner: [read_json_line/write_json_line](/Volumes/D/claude/rightkit/crates/rightkit-framed-sidecar/src/lib.rs:1) — shared bounded framing

## rightkit-fs — N

N; local atomic saves overlap shared fs; preserve broker/recovery.

N; local atomic saves overlap shared fs; preserve broker/recovery.

- Local: [write_binary_with_backup](/Volumes/D/claude/viewright/src-tauri/src/documents.rs:416) — local atomic save
- Owner: [atomic_write](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/lib.rs:45) — shared filesystem

## rightkit-process — N

N; local OCR supervision; migrate only if deadlines remain.

N; local OCR supervision; migrate only if deadlines remain.

- Local: [Command::new(...).spawn](/Volumes/D/claude/viewright/src-tauri/src/ocr_worker.rs:243) — local OCR supervision
- Owner: [OwnedCommand/OwnedChild](/Volumes/D/claude/rightkit/crates/rightkit-process/src/lib.rs:1) — shared child lifecycle

## rightkit-settings — N

N; schema/migrations/mirror local; preserve vr.settings.v1.

N; schema/migrations/mirror local; preserve vr.settings.v1.

- Local: [settings_get/settings_set](/Volumes/D/claude/viewright/src-tauri/src/commands/settings.rs:13) — SQLite mirror
- Owner: [Store/read_with_options](/Volumes/D/claude/rightkit/crates/rightkit-settings/src/lib.rs:1) — shared settings

## @rightkit/pdf-ui — M

M; shared PdfViewer/search/rendering APIs cover basic read-only viewing, while ViewRight still owns overlapping pdf.js rendering/search plus edit/native/OCR policy.

M; route read-only page rendering/search/thumbnail lifecycle through PdfViewer/usePdfSearch or document a host adapter boundary; retain edit mode, annotations, OCR overlays, and macOS PDFKit/native paths where shared read-only viewer cannot apply.

- Local: [renderPage](/Volumes/D/claude/viewright/src/documents/pdf/PdfViewer.tsx:886) — app-owned basic page render lifecycle
- Local: [page.render](/Volumes/D/claude/viewright/src/documents/pdf/PdfViewer.tsx:1036) — app-owned pdf.js page rasterization
- Local: [page.render](/Volumes/D/claude/viewright/src/documents/pdf/PdfViewer.tsx:1153) — second app-owned raster path for display/bench render
- Local: [usePdfSearch](/Volumes/D/claude/viewright/src/documents/pdf/PdfViewer.tsx:771) — app-owned search hook overlaps shared viewer search
- Local: [findLiteralMatches](/Volumes/D/claude/viewright/src/documents/pdf/usePdfSearch.ts:145) — app-owned text matching/search model
- Shared: [thumbnailWindow](/Volumes/D/claude/viewright/src/documents/pdf/PdfThumbnails.tsx:35) — thumbnail rail calls shared virtualization range
- Shared: [boundedCanvasDpr](/Volumes/D/claude/viewright/src/documents/pdf/PdfThumbnails.tsx:103) — thumbnail canvas uses shared DPR helper via pdfShared
- Shared: [pdfWasmUrl](/Volumes/D/claude/viewright/src/documents/pdf/usePdfDocumentLoader.ts:164) — PDF loader calls shared decoder asset URL
- Owner: [PdfViewer/usePdfSearch](/Volumes/D/claude/rightkit/packages/pdf-ui/src/index.ts:1) — shared reader and search exports
- Owner: [renderPage](/Volumes/D/claude/rightkit/packages/pdf-ui/src/PdfViewer.tsx:261) — shared page rasterization/text layer implementation
- Owner: [find](/Volumes/D/claude/rightkit/packages/pdf-ui/src/PdfViewer.tsx:330) — shared basic find implementation
- Owner: [PdfSearchProvider](/Volumes/D/claude/rightkit/packages/pdf-ui/src/types.ts:46) — host adapter contract for custom find UI
- Owner: [description](/Volumes/D/claude/rightkit/packages/pdf-ui/package.json:4) — owner explicitly scopes package to brand-neutral read-only pdf.js viewer

## @rightkit/qa — N

N; active browser QA uses ViewRight-owned raw CDP/session helpers and fake Tauri host rather than @rightkit/qa's browser session, evidence, and workspace APIs.

Route generic CDP/session, workspace, and evidence lifecycle through @rightkit/qa; retain ViewRight-only fake Tauri IPC behavior as an adapter.

- Local: [connect](/Volumes/D/claude/viewright/scripts/qa-browser-functional-helpers.mjs:91) — local raw-CDP connection/session harness used by browser QA
- Local: [HOST_SOURCE built-in commands](/Volumes/D/claude/viewright/scripts/qa-native-host.mjs:78) — local fake Tauri IPC host drives browser QA journeys
- Owner: [page/workspace/evidence exports](/Volumes/D/claude/rightkit/packages/qa/src/index.ts:12) — owner exports browser session, managed workspace, and evidence APIs
- Owner: [QaBrowserSession](/Volumes/D/claude/rightkit/packages/qa/src/page.ts:43) — shared CDP browser-session owner overlapping local connect helper

## @rightkit/settings-ui — N

N; ViewRight owns active modal settings frame/navigation and Section/Segmented/Toggle controls; these overlap shared settings panel/group/row composition while app-specific sections, schema, and file-association policy stay app-owned.

Adopt shared settings frame/group/row where compatible; retain ViewRight schema, migrations, sections, and platform file-association policy.

- Local: [SettingsPanel](/Volumes/D/claude/viewright/src/settings/SettingsPanel.tsx:35) — active app-owned modal frame, navigation, and section selection
- Local: [Section](/Volumes/D/claude/viewright/src/settings/SettingsPanelControls.tsx:3) — active local titled settings group primitive
- Local: [Segmented](/Volumes/D/claude/viewright/src/settings/SettingsPanelControls.tsx:23) — active local settings control used by editor, privacy, and typography sections
- Owner: [SettingsGroup/SettingRow exports](/Volumes/D/claude/rightkit/packages/settings-ui/src/index.ts:4) — owner exports shared settings composition primitives matching local settings grouping
- Owner: [SettingsGroup](/Volumes/D/claude/rightkit/packages/settings-ui/src/primitives.tsx:6) — shared titled settings-group primitive overlaps local Section
- Owner: [SettingRow](/Volumes/D/claude/rightkit/packages/settings-ui/src/primitives.tsx:27) — shared label/description/control-row primitive available for local settings sections

## @rightkit/shell — N

N; direct menu bridge local; compare semantics before adoption.

N; direct menu bridge local; compare semantics before adoption.

- Local: [useNativeDocumentMenu](/Volumes/D/claude/viewright/src/app/shell/nativeMenu.ts:18) — local menu policy
- Owner: [@rightkit/shell](/Volumes/D/claude/rightkit/packages/shell/package.json:1) — typed renderer bridge

Before marking a task complete: migrate matching callers, remove only proven duplicate mechanisms, preserve product-specific schemas/policy/UX, then run affected target checks. Update this inventory from fresh evidence.
