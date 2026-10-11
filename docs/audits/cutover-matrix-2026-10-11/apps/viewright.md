# ViewRight RightKit cutover matrix

Fresh audit at 2026-10-11T02:54:31Z, source ea6bc90f27f277ffd063c42b605c599b902db374.

ViewRight is desktop Tauri/React for macOS & Windows. Coverage: **24 Y**, **8 N**, **1 M**, **64 NA** across all 97 catalog rows.

## Shared routes

- **PDF backend — Y:** pdf_adapter.rs:19 calls rightkit_pdf inspect/compose/save APIs. App retains path authorization & atomic commit policy.
- **PDF reader — M for @rightkit/pdf-ui:** PdfThumbnails.tsx:35, :103 & usePdfDocumentLoader.ts:164 call shared thumbnail/DPR/decoder APIs, but app PdfViewer.tsx:886, :1036, :1153 still owns page rendering, while PdfViewer.tsx:771 & usePdfSearch.ts:145 own overlapping search. Shared PdfViewer/PdfSearch APIs are read-only (owner PdfViewer.tsx:261, :330); migrate basic reader/search/thumbnail lifecycle or document host adapter boundaries. Keep ViewRight edit mode, annotations, OCR overlays, and macOS PDFKit/native reader paths app-owned.
- **OCR — Y:** ocr_backend.rs:23 reaches native rightkit-ocr backends; worker code is process/lifecycle wrapping.
- **Desktop services — Y:** shell, logs, license, secrets, HTTP, tauri, and watch callers are in current Rust source; typed renderer adapters are in updateLicense.ts, diagnostics.ts, and tauriBridge.ts.
- **QA — Y:** qa-e2e/pdf_edit.rs imports rightkit_qa Control, Harness, and workspace APIs, then runs its real-app PDF-edit scenario through Harness::new. qa-chrome remains live browser-launch tooling.
- **UI/tooling — Y:** ToastViewport mounts shared app-shell ToastViewport with platform-ui defaultToastBus; doc-render, legal-ui, media-ui, theme, updates, legal, and release have current callers.

## Open cutover gaps

- cargo:rightkit-fs: migrate local atomic save implementation in documents.rs:416 while preserving broker/recovery policy.
- cargo:rightkit-framed-sidecar + cargo:rightkit-process: assess OCR worker framing/supervision migration; preserve bounded stdin/stdout, deadline, and one-shot Vision behavior.
- cargo:rightkit-settings + npm:@rightkit/settings-ui: assess shared persistence/frame adoption only if vr.settings.v1, migrations, file associations, and ViewRight sections remain intact.
- cargo:rightkit-crash: compare shared panic report API with local std::panic::set_hook before migration.
- npm:@rightkit/qa: migrate generic raw-CDP/session, workspace, and evidence lifecycle from local browser helpers; retain fake Tauri IPC behavior as a ViewRight adapter.
- npm:@rightkit/shell: compare typed renderer bridge with direct native-menu policy before adoption.
- npm:@rightkit/pdf-ui: migrate overlapping basic read-only rendering/search/thumbnail paths to shared PdfViewer/PdfSearch, retaining edit/native/OCR policy paths that shared package cannot support.

## Delivery flags

Some shared pins remain behind catalog versions for legal-ui, license, release, and several Rust crates. Exact rows & versions are in JSON delivery_flags; these are qualification/update work and do not alter status.

Full 97-row coverage, evidence paths, owner APIs, searches, and adversarial review are in viewright.json.

