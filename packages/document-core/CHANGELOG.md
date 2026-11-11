# Changelog

## 0.2.0 (unpublished)

Shape review 2026-11-11 (`docs/audits/2026-11-11-shape-matrix/reports/family-documents.md`; design in `reports/documents-redraw-design.md`). Taken on the recommended option, pending the owner's confirmation.

The review judged this package the wrong shape: a catch-all holding ViewRight's whole PDF editor, its settings sections and workspace-only hooks beside the editor base, with five separate product-name stores across the document packages and names fixed to one app's prefix. It is dissolved as a catch-all: every part with an owner moved to it. What stays is the editor base that `@rightkit/html-editor`, `@rightkit/markdown-editor`, `@rightkit/docx-editor` and `@rightkit/document-workspace` all import; it cannot live in the workspace (the workspace imports the editors, so the editors importing the workspace would be a cycle), so it is not a re-export shim.

Moved out (breaking: the subpaths are gone from this package):
- To `@rightkit/document-workspace`: the PDF editor, `documents/pdf/*` (viewer, toolbar, thumbnails, outline, OCR, inline text editing, save plan, macOS native read pane, benchmark harness); the PDF-editor native commands of `state/tauriBridge` (`ocrPage`, `ocrAvailable`, `ocrBackendMissing`, `inspectPdfPage`, `listPdfFonts`, `composePdfPreview`, `preparePdfSave`, `commitPdfSave`, now `documents/pdf/pdfBridge`); `printPdf` and `printPdfDocument` of `app/documentPrinting` (now `documents/pdf/pdfPrinting`); `settings/sections/*` and `settings/rk-settings.css`; `app/useCloseFindOnSurfaceChange`, `app/useEditText`, `documents/FullscreenIcon`, `documents/txt/cmScroll`, `documents/useMirroredEditText`, `documents/useSplitResize`.
- To `@rightkit/html-editor`: `app/shortcutChords`.
- To `@rightkit/markdown-editor`: `documents/scopedSelectAll`, `documents/mdx/mdxRuntimeScript` (and its `.txt`).
- `styles.css` no longer imports pdf.js's `pdf_viewer.css` (the workspace's does). Dependencies `pdfjs-dist` and `@codemirror/view` removed.

Added:
- `host` (also on the root): `configureDocumentHost({ appName?, id? })`, the one host-identity store for every document package, read at use time. `hostAppName`, `setHostAppName` (now a shorthand), `hostId`, `documentEvent(name)` = `<id>:<name>`, `hostScheme(suffix)` = `<id>-<suffix>`, `hostHeader(suffix)` = `x-<id>-<suffix>`, `hostStorageKey(key)` = `<id>:<key>`, `hostQaGlobal(name)` = `__<camelId><Name>`. Default `id` is `"rk-document"`; a host whose native side uses `viewright:*` events and `viewright-*` schemes passes `id: "viewright"` and every name is as before.
- `state/tauriBridge` exports `byteArrayToBase64` (used by the PDF bridge).

Changed (breaking):
- `appName` module removed; import `hostAppName`/`setHostAppName` from the root or `host`.
- Names shared with the host's native side now come from `id`: the stream scheme (`<id>-stream`, was `viewright-stream`), the `write_document_base64` path header (`x-<id>-path-hex`), the native menu event (`menuCommandEvent()` replaces the constant `MENU_COMMAND_EVENT`), the Markdown format event (`markdownFormatEvent()` replaces `MARKDOWN_FORMAT_EVENT`), the Tauri events `<id>:open-url` (`onAppEvent("open-url")`, was `onAppEvent("viewright:open-url")`) and `<id>:context-*`, the preview scheme (`previewScheme()` replaces the constant `PREVIEW_SCHEME`), the recent-URL storage key `<id>:recent-urls` and the QA global `__<camelId>QaPerf`.
- The find-highlight style element id is `rk-document-search-highlight-styles` (was `viewright-search-highlight-styles`).

Not changed here, recorded for a later step: the theme ids `viewright-paper` and `viewright-night` and the settings key `vr.settings.v1` are stored user data, so renaming them needs a migration; the bridge still calls fixed native command names (the paired Rust command set in `rightkit-document-workspace` is the report's step 3).

## 0.1.2 (unpublished)

- Removed (breaking, Adrian, 2026-11-03: "no dummy data, no fucking made up shit. real data."): the browser-QA fixture layer. `BROWSER_QA_BUILD`, `isBrowserQaMode()`, `qaReloadPdfBytes()`, every fixture branch of `state/tauriBridge` and the `state/browserQa`, `browserQaText`, `browserQaDocx` and `browserQaPdf` modules (and their `index` exports) are gone. Outside the Tauri webview the bridge keeps its plain no-op returns.
- Removed (breaking, Adrian, 2026-11-03: no dummy data): the in-memory browser-QA licence client. `browserQaLicenseClient()` is gone from `state/tauriBridge`, `state/browserQaLicense` is deleted, and the `@rightkit/license` dependency with it (`@rightkit/license/testing` no longer exists).
- `@rightkit/license` is a dependency again: the settings sections `document-core` exports come from `@rightkit/settings-ui`, whose `sections` entry imports its peer `@rightkit/license`; without it an installed `document-core` failed to import.

## 0.1.1

- Ask 1: the host bridge (`state/tauriBridge`), diagnostics (`state/diagnostics`) and platform modules are the single copy the document editors import. Hosts delete their own copies. Host-facing changes a host must adopt:
  - Browser-QA build switch is `VITE_RIGHTKIT_BROWSER_QA=1` (was the host-named variable). A host QA build sets this variable; without it the QA fixtures are dropped.
  - `parseViewRightError` is now `parseHostError`.
  - The file-association field `viewrightIsPreferred` is now `hostIsPreferred`.
  - Diagnostics allowlist adds `pdf-editor:viewer-load`.
- Ask 2 and 3 are in `@rightkit/markdown-editor` 0.1.1; ask 4 pins are in `@rightkit/docx-editor` 0.1.1 and `@rightkit/html-editor` 0.1.1.
- The accent preset `blue` reads its name from `hostAppName()` when displayed, so `setHostAppName()` at startup takes effect.
- Wording fix in the toolbar primitives comment; no behaviour change.

## 0.1.0

- First release. Shared base for the document editors: platform bridge, settings, state, PDF helpers and the format registry.
