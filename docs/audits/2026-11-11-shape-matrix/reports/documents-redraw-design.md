# Documents redraw: design note

2026-11-11, branch `claude/busy-hypatia-e30h7l`, base `65cda82`. Follows `family-documents.md` and README "Order of work" item 5 (dissolve `@rightkit/document-core`, host-configured `@rightkit/document-workspace`, one PDF viewer). Taken on the recommended option, pending Adrian's confirmation. Paths are under `packages/`; `@base` means the file as it was at `65cda82`.

## 1. Where each part of `@rightkit/document-core` goes

The import graph decides the owners. `document-workspace` imports the three editors (for example `document-workspace/src/documents/DocumentRouter.tsx:24-38` lazily loads the Markdown, MDX, HTML and DOCX panes), and every editor imports the editor base (types, bridge, drafts, diagnostics, toolbar and find primitives: 92 import sites of `documents/types` alone at `@base`). So the base cannot move into the workspace: the editors would then import the workspace, which imports them. It has no other owner either: `@rightkit/doc-render`, `platform-ui`, `react-shell` and `pdf-ui` are neutral packages in good shape, and taking ViewRight's bridge and settings store would make them worse.

So `document-core` is dissolved as a catch-all, not deleted: each part that one package owns moved to it, and what stays is the base the four packages share. This is the "small base" the review names as the best shape (`family-documents.md` section 9, point 5) and its order of work item 2 ("Reduce document-core to types, format registry, bridge interface and primitives; one app-name store"). It is not a re-export shim, because nothing in it is a copy.

| Part (at `@base`) | Lines | Owner now | Why |
|---|---|---|---|
| `documents/pdf/*` (34 files: viewer, toolbar, thumbnails, outline, OCR, inline editor, save plan, macOS native pane, bench harness) | 10,123 | `document-workspace` `documents/pdf/` | Only the workspace imports it (`DocumentRouter.tsx`, `useDocumentLoader.ts`, `markdownPaletteActions.ts`, `useClosedTabEviction.ts`, `useDocumentSaver.ts`, `useFormatDirtyState.ts`, `DocumentRouterTable.tsx`); it is a composed pane like the HTML and Markdown panes already there. Its final home is a second layer on `@rightkit/pdf-ui` once it renders through `PdfViewer` (section 3). |
| PDF-editor commands in `state/tauriBridge.ts` (`ocrPage` ... `commitPdfSave`, `@base` lines 416-631) | 216 | `document-workspace` `documents/pdf/pdfBridge.ts` | Only the PDF pane calls them; they pulled the PDF edit model's types into the shared bridge. |
| `printPdf`, `printPdfDocument` in `app/documentPrinting.ts` | 25 | `document-workspace` (`documents/pdf/pdfPrinting.ts`, lazy import in `app/useDocumentOutputActions.ts`) | They were lazy wrappers over the PDF module. |
| `settings/sections/*`, `settings/rk-settings.css` | 727 | `document-workspace` `settings/sections/` | ViewRight's settings UI; only `SettingsPanel` uses them. The settings **store** (`settings/settings.tsx`, `themes.ts`, `fonts.ts`) stays: `docx-editor` reads it. |
| `app/useCloseFindOnSurfaceChange`, `app/useEditText`, `documents/FullscreenIcon`, `documents/txt/cmScroll`, `documents/useMirroredEditText`, `documents/useSplitResize` | 182 | `document-workspace` | Only the workspace imports them. |
| `app/shortcutChords` | 14 | `html-editor` | Only `useHtmlBlockUndoRedo.ts` imports it. |
| `documents/scopedSelectAll`, `documents/mdx/mdxRuntimeScript(.ts,.txt)` | 672 | `markdown-editor` | Only the Markdown editor imports them. |
| `appName.ts` (one copy in each of the five packages) | 5 x 10 | `document-core` `host.ts`; the four other `appName.ts` re-export it | Five stores meant five setters (`family-documents.md` section 1, point 7). |
| Everything else: `documents/types`, `documentFormats` and the TSV, `state/tauriBridge` (shared part), `bridgeEvents`, `diagnostics`, `errors`, `motion`, `recentUrls`, `app/blockDrafts`, `BlockDraftContext`, `chrome`, `deferredMount`, `documentPrinting` (non-PDF), `printPipelineShared`, `nativePrintPreview`, `resourceCensus`, `dropCoordination`, `shell/docBar`, `shell/nativeMenu`, toolbar, find and preview primitives, `perfEventCounters`, `platform`, settings store | about 6,000 | stays in `document-core` | Imported by two or more of the four packages. |

New in the base: `host.ts`, one host-identity store, `configureDocumentHost({ appName, id })`. Every name the packages share with the host's native side or with each other is derived from `id` at use time: events `<id>:<name>`, schemes `<id>-stream|site|preview`, header `x-<id>-path-hex`, storage keys `<id>:<key>`, QA globals `__<camelId><Name>`. ViewRight passes `id: "viewright"` and keeps every name it has on disk and in Rust. Names private to one package (the MDX and interactive-HTML postMessage protocols, ProseMirror meta keys) are fixed neutral `rk-document:*` literals, since both ends live in the same package.

## 2. What made `@rightkit/document-workspace` ViewRight's, and the host configuration that replaces it

At `@base`:

- Legal gate hard-coded in `app/AppRoot.tsx:26-27` (960x560), `:29-32` (trust line and agreement sentence read `hostAppName()` at module load, so a statically imported workspace said "App"), `:36-47` (ViewRight's light palette, accent `#0C6AA5`, Tanker display font) and `:55` (`/fonts/Tanker-400.woff2`).
- Licence and update wiring: `state/updateLicense.ts:51` (`https://viewright.cc/pricing/`), `:54` (app key `viewright`), with `settings/publishedSections.tsx`, `settings/sections/UpdateLicenseBlock.tsx` and `settings/sections/AboutSection.tsx:13,26-30` (the "ViewRight" wordmark, `viewright.cc`, `hello@viewright.cc`); dependencies `@rightkit/license`, `@rightkit/app-update`, `@rightkit/legal-ui`, `@tauri-apps/plugin-process`.
- `settings/sections/DiagnosticsSection.tsx:33` mailed `hello@viewright.cc`.
- Events and names: `viewright:*` window and Tauri events in 18 files (for example `app/useGlobalShortcuts.ts:132-133,412-425`, `app/FindBar.tsx:174`, `app/AppLifecycle.ts:85-96`), `?viewright-print-preview` (`app/PrintPreviewWindow.tsx:8`), `__viewrightResourceCensus` and `__viewrightResourceMatrixStatus` (`app/resourceCensusDev.ts:56-69`), `viewright-native-pdf-<n>` and `viewright-pdf-bench-*.json` in the PDF pane.

Now: `configureDocumentWorkspace(config)` (`document-workspace/src/config.ts`), called once by the host before the first render, read at use time:

| Field | Replaces |
|---|---|
| `appName`, `id` | the five name stores and every `viewright` prefix (forwarded to `configureDocumentHost`) |
| `appVersion`, `supportEmail` | `APP_VERSION` from `state/updateLicense.ts` and the fixed mail address |
| `firstRun.legal` (`LegalStepConfig` from `@rightkit/onboarding`: theme, trust line, agreement sentence, wordmark font, ...), `firstRun.gateWindow` | the fixed gate constants; no `firstRun`, no gate |
| `settings.about`, `settings.sections` | `AboutSection`, `UpdatesPanel`, `UpdateLicenseBlock`, `PublishedLicense`, `LegalPanel` (the host composes `@rightkit/settings-ui`, `license`, `app-update`) |
| `backgroundUpdateCheck` | `checkForBackgroundUpdate` from `state/updateLicense.ts` |

Menus: the native menu is already host-driven (`set_native_menu_state` with the ids the active document offers; the menu fires `<id>:menu-command`). Routes: the format-to-pane table (`documents/DocumentRouter.tsx`) is still built in; taking it from the host is deferred (section 4). The legal data (`configureLegal({ manifest, contract })`) was already host-supplied and is unchanged.

The removed licence and update composition (about 830 lines) is the host's (ViewRight's live 0.1.123 build has its own). If a second app needs the same composition, its shared form belongs in `@rightkit/settings-ui` or `@rightkit/app-update`, not in a document package.

## 3. One PDF viewer: `@rightkit/pdf-ui` wins

`@rightkit/pdf-ui` is the viewer: a written, read-only API (`pdf-ui/src/types.ts:1-3`: "No app, Tauri, or file-system imports"), already used by MailRight as designed (`family-documents.md` section 6), and since 0.3.0 it has the extension points ViewRight's editor needs to sit on it: `unloadOffscreenPages` (`pdf-ui/src/PdfViewer.tsx:82`), `annotationLayer` (`:86`), `searchLayer` (`:90`), `pageLayers` for a host-owned editing layer (`:95`, context type at `:102`), `passwordPrompt` (`:99`) and the range source (`:149`, `:197`). ViewRight's viewer (`document-workspace/src/documents/pdf/PdfViewer.tsx`, 1,760 lines, entry at `:187`) is an editor that also draws pages; the draw half duplicates pdf-ui.

Done in this change (duplicates deleted, pdf-ui's copy used):

- Range transport: `pdfBenchTransport.ts` now calls `createPdfRangeTransport` and `pdfRangeDocumentInit` (`document-workspace/src/documents/pdf/pdfBenchTransport.ts:98-99`); its `HostPdfRangeTransport`, `formatPdfRangeHeader`, `parseContentRangeTotal` and length probe (about 150 lines) are gone. pdf-ui's transport is the port of that same code (`pdf-ui/src/rangeTransport.ts:1`), with the same 206 and `Content-Range` checks.
- Password dialog: `PdfViewer.tsx:1752` and `MacPdfReadPane.tsx` render pdf-ui's `PdfPasswordPrompt` (same title, copy, Enter and Escape); the local copy is deleted. It is styled by `rk-pdf-password*` and `--rk-pdf-*`, not the shell's `ActionDialog`, so it looks different until ViewRight sets those variables.
- pdf.js helpers were already pdf-ui's (`pdfShared.ts` re-exports them; find uses `findLiteralMatches`/`findRegexMatches`).

Not done (the page-drawing duplicate): rebasing ViewRight's `PdfViewer` onto pdf-ui's `PdfViewer` with `pageLayers`, then moving the editor half to `@rightkit/pdf-ui/editor` or a `@rightkit/pdf-editor` package. It changes how ViewRight's shipped editor renders, edits and saves pages (render keepalive, OCR overlay, inline editor, preview recomposition, the macOS native pane), and pdf-ui 0.3.0 itself has never been built or run against ViewRight (its CHANGELOG lists the four cases to run). It needs that ViewRight run, so it is the first deferred item.

## 4. Deferred

1. PDF: rebase the editor onto `PdfViewer` + `pageLayers`, then give it its own entry or package; remove the bench harness (`pdfBench*.ts`) from the shipped package (`family-documents.md` section 9, action 4).
2. Workspace: take the format-to-pane table from the host instead of importing the three editors (section 1, action 2); replace the generated barrels with written entry points.
3. Bridge: pair the npm bridge with a Rust command set in `rightkit-document-workspace` (fixed command names today), and fold `rightkit-docs` `files` into that crate (sections 5 and 10).
4. Names still fixed: theme ids `viewright-paper` and `viewright-night` and the settings key `vr.settings.v1` (stored user data: rename with a migration); in `html-editor` `data-viewright-href`, `viewrightTheme`, `viewright-html-inframe-style`, `open.viewright.invalid`; in `markdown-editor` `viewright-local:`, `__viewrightQaMarkdown`, `viewright.invalid`, `viewright-code-fence-highlight`. The `<!-- viewright: KIND -->` hint is a file format and stays accepted.
5. One Mermaid sanitizer (markdown-editor and doc-render, section 2) and the format table generated from `rightkit-docs` (section 9, action 5).
6. The default stylesheet tokens in `document-workspace/src/styles/index.css` (`--copper: #0c6aa5`) are overridable defaults; the Tanker wordmark is the suite face (Adrian, 2026-07-16), not ViewRight's.

## 5. DOCX (decision 7): unchanged

`@rightkit/docx-editor` keeps its dependency on the published `@eigenpal/docx-editor-core` and `-react` 1.9.0. The carried patch is a source diff and cannot apply to the published, minified `dist` (`family-documents.md` section 4), so ViewRight's toolbar does not show. The two options, for Adrian:

1. **Upstream the toolbar prop first** (a `renderToolbar` / host-toolbar context in eigenpal), then pin the release that has it; the patch stays only as the pull request text. Keeps ViewRight's toolbar.
2. **Accept eigenpal's built-in toolbar** and delete the patch and `HostDocxToolbar`. No upstream dependency; ViewRight's DOCX toolbar changes to eigenpal's.

(The review also names driving `HostDocxToolbar` from outside with `showToolbar={false}` through the editor ref, not verified to expose the formatting commands.) Nothing was changed for DOCX here beyond the shared name store.
