# Family review: Documents

Read-only review, 2026-11-11. RightKit at `/home/user/rightkit` (head `d17fd41`). Paths under `S/survey/...` are the surveyed app trees; `RK/` is `/home/user/rightkit`.

Marks: **[V]** verified by reading the cited code; **[S]** suspicion, not verified.

## Starting facts that apply to the whole family

1. **The five npm document packages are a path-preserving copy of the surveyed ViewRight tree, not a designed API.** [V] File-by-file comparison of each package's `src/` against `S/survey/viewright/src/` at the same relative path (non-test `.ts/.tsx/.css`, line similarity weighted by size):

   | Package | Files | Found at same path in surveyed ViewRight | Mean similarity to surveyed (0.1.165) | Mean similarity to live Mac (0.1.123) | Files absent from live |
   |---|---|---|---|---|---|
   | `@rightkit/document-core` | 95 | 89 | 0.98 | 0.77 | 19 |
   | `@rightkit/document-workspace` | 131 | 127 | 0.97 | 0.75 | 51 |
   | `@rightkit/markdown-editor` | 142 | 137 | 0.98 | 0.90 | 15 |
   | `@rightkit/html-editor` | 58 | 55 | 0.99 | 0.95 | 20 |
   | `@rightkit/docx-editor` | 17 | 12 | 0.89 | 0.85 | 7 |

   The files not found in ViewRight are the generated `index.ts`, `env.d.ts`, `appName.ts`, `styles.css`, and (docx) the renamed toolbar.
2. **The basis is the unreleased port tree, not the shipped build.** [V] `S/survey/viewright/state.md:5` records the last published builds as macOS 0.1.111 and Windows 0.1.43; the live tree given for this review is Mac 0.1.123; the packages were cut from 0.1.165, which `S/survey/viewright/STATUS.md` describes as "port is not complete ... no installed-package acceptance exists". So the packages carry about 42 versions of unshipped change on top of what users have. I found no shipped capability missing from the surveyed tree: the live-only files are the image editor (now `@rightkit/media-ui`), licence and update code (now `rightkit-license`, `@rightkit/app-update`), file associations (now `rightkit-tauri-shell`), the collapsed sidebar rail (replaced by `SidebarRail`), and Markdown reader pieces now in `@rightkit/doc-render`. This is a file-level check, not a behaviour test.
3. **Every package exports every module.** [V] `RK/packages/document-workspace/src/index.ts:1-2` ("Generated entry: every module whose exports do not collide"), plus a subpath export per source file in each `package.json`. The public API is the whole internal file layout, so any internal rename is a breaking change.
4. **No app consumes the five packages or the Rust workspace crate yet.** [V] No surveyed `package.json` or `Cargo.toml` pins them.

### ViewRight: RightKit packages pinned, live build versus surveyed tree [V]

| | Live Mac 0.1.123 (`S/survey/viewright-live`) | Surveyed 0.1.165 (`S/survey/viewright`) |
|---|---|---|
| npm, runtime | `@rightkit/legal-ui` 0.1.1, `license` 0.1.7, `logs` 0.1.4, `tauri` 0.1.1, `updates` 0.2.4 | `app-update` 0.4.2, `doc-render` 0.2.1, `legal` 0.6.3, `legal-ui` 0.4.1, `license` 0.3.0, `logs` 0.1.6, `media-ui` 0.3.4, `onboarding` 0.6.3, `pdf-ui` 0.2.0, `platform-ui` 0.4.0, `react-shell` 0.7.2, `settings-ui` 0.7.0, `tauri-shell` 0.1.3, `theme` 0.3.1 |
| npm, dev | `@rightkit/legal` 0.3.1, `release` 0.2.104 | `qa` 0.6.1, `qa-chrome` 0.1.11, `release` 0.6.6 |
| cargo | `rightkit-logs` =0.1.0, `rightkit-license` =0.1.3, `rightkit-tauri` =0.1.1 | `rightkit-fs` 0.4.7, `db` 0.5.2, `crash` 0.3.6, `file-watch` 0.4.1, `tauri-shell` 0.6.0 (with `pdf-host`), `logs` 0.3.5, `app-settings` 0.3.0, `daemon` 0.4.0, `license` 0.2.5, `child-process` 0.5.6, `device` 0.4.0, `credentials` 0.4.2, `crypto` 0.1.0, `ocr` 0.2.3, `pdf` 0.3.3 (with `host`), `qa` 0.9.0 (optional) |
| Documents family in use | none (own PDF viewer, own Markdown reader pieces, own image editor) | `pdf-ui` 0.2.0 (headless pieces only), `doc-render` 0.2.1, `rightkit-pdf`, `rightkit-ocr` |
| DOCX | eigenpal 1.9.0 vendored, `file:vendor/docx-editor/packages/*` | the same vendored fork, unchanged |

Neither tree pins `@rightkit/document-core`, `document-workspace`, `markdown-editor`, `html-editor`, `docx-editor`, `rightkit-document-workspace` or `rightkit-docs`.

---

## 1. `@rightkit/document-workspace` (npm)

1. **Exists:** `RK/packages/document-workspace`, 0.1.2 unpublished.
2. **Feeds:** ViewRight 31,171 lines (170 files).
3. **Basis:** surveyed ViewRight `src/app`, `src/settings`, `src/state`, `src/documents/{txt,web,html,...}` copied at the same paths.
4. **Side by side:** one app only. Against the live build it differs most of all the packages (0.75, 51 new files).
5. **Best shape:** a workspace shell that a host composes: tabs, sidebar and explorer, document router, loaders and savers, drafts, autosave, find, command palette, print, taking from the host (a) the format-to-pane table, (b) a bridge object, (c) the app name, (d) slots for settings sections. Legal gate, licence, updates and onboarding are the host's composition of `@rightkit/onboarding`, `legal-ui`, `license`, `app-update`. **Today's package is not that shape.** It is the whole ViewRight application:
   - `app/AppRoot.tsx:19-27,36-47` hard-codes the first-run legal gate: a fixed 960x560 window, ViewRight's light palette with accent `#0C6AA5`, the Tanker wordmark font at `/fonts/Tanker-400.woff2`, and the list of four legal documents in the agreement sentence. [V]
   - `package.json` depends on `@rightkit/legal`, `legal-ui`, `license`, `app-update`, `onboarding`, `settings-ui`; `settings/publishedSections.tsx` and `state/updateLicense.ts` build the updates and licence settings. [V]
   - `app/useDocumentLoader.ts:97` states the supported format list in user copy. [V]
6. **Per app:** ViewRight **gaps** (it is its own code, but see the name bug below and the unshipped basis). Others n/a today; a second app would be **wrong** because it inherits ViewRight's legal gate, licence and update UI, and format set.
7. **Boundary:**
   - Product decisions listed in point 5.
   - **Name bug.** [V] `AppRoot.tsx:29-32,50` read `hostAppName()` at module load to build the trust line, the agreement sentence and `LEGAL_STEP.appName`. A host's static import evaluates this module before the host can call `setHostAppName()`, so the legal gate says "App". The README's claim that order matters "only for values the host reads at import" is wrong for these three.
   - **Five separate name stores.** [V] `appName.ts` is duplicated in all five packages, each with its own `current` variable; a host must call five setters. It belongs once, in `document-core`.
   - Window event names are `viewright:*` throughout (`app/useGlobalShortcuts.ts:132-133,412-425`, `app/FindBar.tsx:174`); they are the wiring between this package and the three editors. [V]
   - `documents/ImageDocument.tsx` is the host adapter for `@rightkit/media-ui`; correct place.
8. **Actions before publication:**
   1. Split out the app layer: `AppRoot` (legal gate), licence and update sections, `PrintPreviewWindow` entry and About become either ViewRight shell code or props.
   2. Take the format table and pane components as a registry argument instead of importing the three editors directly.
   3. Replace the generated barrel with a written entry point; stop exporting every hook.
   4. One `setHostAppName` in `document-core`; no module-level reads.
   5. Rename the `viewright:*` events to a neutral prefix in one change across the five packages.

Depth: deep (entry, root, dependency and bridge files read; the 70 hooks under `app/` not read individually). Confidence: high on the boundary findings.

## 2. `@rightkit/markdown-editor` (npm)

1. **Exists:** `RK/packages/markdown-editor`, 0.1.1.
2. **Feeds:** ViewRight 27,905 lines (143 files).
3. **Basis:** surveyed ViewRight `src/documents/markdown` and `src/documents/mdx`.
4. **Side by side:** one app. Closest to live after html-editor (0.90). The reader pieces the live build held locally (`remarkDefListAbbr`, detectors, citations, wiki links) are now imported from `@rightkit/doc-render` (12 import sites), which is the right direction. [V]
5. **Best shape:** `MarkdownEditor` (hybrid ProseMirror and source), MDX pane, properties panel, export, with host ports for asset save, link opening and file reads. The module split (core, projection, blockEditing, tables, references, export) is sound. **Not yet the shape** because of the items in 7.
6. **Per app:** ViewRight **gaps**. Others n/a.
7. **Boundary:**
   - Host wiring by global events and names: links open by dispatching `viewright:open-url-tab` on `window` (`renderNodes.tsx:195,448`); undo and redo by `viewright:undo` (`MarkdownEditor.tsx:587`); local images use the scheme `viewright-local:` (`security/sanitizeMarkdownHtml.ts:8`); semantic hints are `<!-- viewright: KIND -->` (`semanticBlocks.ts:9`). [V] The hint prefix is a file format users have on disk, so it must stay accepted; the rest should be props or a neutral prefix.
   - Depends on `@rightkit/document-core` for the Tauri bridge, so the editor cannot be used without ViewRight's command set (see package 9).
   - Two Mermaid sanitizers with different policy: `markdown/mermaidSvgSanitize.ts` (263 lines, admits HTML and MathML labels) and `doc-render` `sanitizeMermaidSvg` (`RK/packages/doc-render/src/htmlProfiles.ts:152`, no `foreignObject`). [V] that both exist; [S] that ViewRight's is the one that must win (it is the reference app's behaviour).
   - QA globals in shipped code: `window.__viewrightQaMarkdown` (`qaCoreObservation.ts:19-31`). [V]
8. **Actions:** link, undo and asset ports as props; one Mermaid sanitizer (ViewRight's policy, placed in doc-render); neutral scheme and event names with the old hint prefix kept as an accepted alias; written entry point.

Depth: deep on boundaries and imports, light on editor internals (25k lines not read). Confidence: medium.

## 3. `@rightkit/html-editor` (npm)

1. **Exists:** `RK/packages/html-editor`, 0.1.1.
2. **Feeds:** ViewRight 15,898 lines (61 files).
3. **Basis:** surveyed ViewRight `src/documents/html` (0.99); 0.95 against live.
4. **Side by side:** one app.
5. **Best shape:** viewer, safe block editor and source map, with the site-root and preview schemes supplied by the host. `rightkit-tauri-shell` already lets the app choose scheme names (`RK/crates/rightkit-tauri-shell/src/site.rs:1`, `preview.rs:22`). **The package does not follow that**: it hard-codes them.
6. **Per app:** ViewRight **gaps**. Others n/a.
7. **Boundary:**
   - `htmlInline.ts:157` builds `viewright-site://localhost/<token>/`; the iframe CSP at `htmlInteractive.ts:180` names `viewright-site:`; `HtmlDocumentView.tsx:29` assumes the `viewright-preview` protocol. [V] A second app must register schemes called `viewright-*`.
   - Same global-event wiring as Markdown (`useHtmlBlockUndoRedo.ts:159-160`, `useHtmlReload.ts:69`, `useHtmlPreviewControls.ts:66,116`). [V]
   - Own sanitizer `htmlVisualSanitize.ts` (386 lines, DOMPurify and parse5) beside doc-render's `sanitizeFullHtmlDocument`. [V] both exist; they serve different jobs (editable projection versus display), so this is not a duplicate to remove, only one to document.
8. **Actions:** scheme names and CSP sources from host configuration; event wiring as props; written entry point.

Depth: deep on boundaries, light on internals. Confidence: medium.

## 4. `@rightkit/docx-editor` (npm)

1. **Exists:** `RK/packages/docx-editor`, 0.1.1, depending on published `@eigenpal/docx-editor-core` and `-react` 1.9.0.
2. **Feeds:** ViewRight 1,834 lines (13 files).
3. **Basis:** surveyed ViewRight `src/documents/docx`; `eigenpalReact.ts` rewritten to import the published package instead of the vendored source; the toolbar renamed `HostDocxToolbar.tsx`.
4. **Side by side:** one app. ViewRight live and surveyed both compile the vendored **source** (`S/survey/viewright/src/documents/docx/eigenpalReact.ts` imports `vendor/docx-editor/packages/react/src/viewright`).
5. **Best shape:** the owner's decision: published eigenpal packages plus a small host-toolbar patch carried here. The package's design follows the decision. **It does not yet deliver it:**
   - The patch (`patches/docx-editor-react-host-toolbar.patch`, 79 lines) is a source diff against `packages/react/src/components/DocxEditor.tsx` with zero-context hunks and "line numbers are indicative". [V]
   - The published `@eigenpal/docx-editor-react@1.9.0` contains only `dist/`, and `dist/index.mjs` is minified (30 lines, 200 KB). `renderToolbar` and `DocxHostToolbarContext` occur nowhere in it. [V] A pnpm `patchedDependencies` entry cannot apply this patch to the published package.
   - The patch is not registered in `RK/pnpm-workspace.yaml` (no `patchedDependencies` key). [V] The README says so.
   - Result: against the published editor `renderToolbar` is ignored and users see eigenpal's built-in toolbar, not ViewRight's. That is a visible regression from the live app.
   - The other three bridge props ViewRight uses (`onEditorViewReady`, `disableFindReplaceShortcuts`, `showOutline`) are in the published dist, so the toolbar is the only delta. [V] This confirms the decision is workable.
6. **Per app:** ViewRight **gaps** (toolbar). Others n/a.
7. **Boundary:** none of note; no ViewRight names left in source. Depends on `document-core` for dialogs and bridge.
8. **Actions:**
   1. Make the toolbar work on the published artefact. Options, in order of preference: (a) drive `HostDocxToolbar` from outside the editor with `showToolbar={false}`, `DocxEditorRef` and `onSelectionChange`/`onEditorViewReady`, which needs no patch if the ref exposes the commands ([S]: the ref has `save`, `setZoom`, `focus`, `getEditorRef`; formatting commands were not checked); (b) a real pnpm patch generated against `dist/index.mjs`, `index.js` and `index.d.ts` of 1.9.0, registered in the workspace root and repeated in each consuming app, because a published package cannot carry a pnpm patch into its consumer; (c) the upstream pull request, then pin the release that contains it.
   2. Keep the source patch only as the upstream PR text.
   3. ViewRight then deletes `vendor/docx-editor` (the unused `agents` package included).
   4. Note for later: upstream has moved to `@docx-editor.dev/*` 2.7.0 (`S/survey/viewright/docs/EIGENPAL_UPGRADE.md`); 1.9.0 is the legacy line.

Depth: deep. Confidence: high.

## 5. `rightkit-document-workspace` (cargo)

1. **Exists:** `RK/crates/rightkit-document-workspace`, 0.1.4 unpublished, 2,614 lines.
2. **Feeds:** ViewRight 3,147 lines (10 files).
3. **Basis:** ViewRight `src-tauri/src/persistence.rs` and the bodies of `commands/{library,document_io,explorer,markdown_index,markdown_resources,settings}.rs`, reshaped behind three host traits. Measured overlap: persistence 251 of 318 distinctive lines, explorer 229 of 311, markdown_resources 313 of 439.
4. **Side by side:** one app.
5. **Best shape:** what it is: a store (`Db`) plus synchronous command bodies behind `PathGrants`, `DocumentBackend`, `WorkspaceWatcher`, with the app name passed in (`explorer.rs:86`). This is the best-designed piece of the family. Two gaps keep it from "best".
6. **Per app:** ViewRight **gaps**. Others n/a.
7. **Boundary:**
   - **Duplicate types.** [V] `RawDocument`, `FileFingerprint`, `WriteDocumentResult` are defined in `src/documents.rs:7-41` and again, field for field, in `RK/crates/rightkit-docs/src/files/mod.rs:112-151`. Every host must write the same adapter from one to the other to implement `DocumentBackend`.
   - **No IPC layer anywhere.** [V] The npm side calls 57 fixed Tauri command names (`read_document`, `write_document`, `list_recent`, `prepare_pdf_save`, `ocr_available`, ...) from `document-core/src/state/tauriBridge.ts`. This crate has no `tauri` dependency and no RightKit crate registers those commands, so a second app must copy ViewRight's `src-tauri/src/commands/*` wrappers (about 3,000 lines) and keep the names in step by hand.
   - `app_paths.rs` (265 lines) is mapped here but is not in the crate (11 of 151 lines found anywhere); it is app directory policy and belongs to `rightkit-fs` `AppDirs` or the shell. [V] on absence, [S] on destination.
8. **Actions:** depend on `rightkit-docs` (feature `files`) or absorb that module, and delete the duplicate types and the `DocumentBackend` seam; add an optional `tauri` feature (or a small `rightkit-document-workspace-tauri` crate) that registers the command set the npm bridge calls, generic over `PathGrants`; publish.

Depth: deep (public surface, traits, README and changelog read; function bodies sampled). Confidence: high.

## 6. `@rightkit/pdf-ui` (npm)

1. **Exists:** `RK/packages/pdf-ui`, 0.3.0 unpublished (0.2.0 published and pinned by ViewRight and MailRight).
2. **Feeds:** ViewRight 10,411 (38 files, EXTEND), MailRight 144 (2 files, ADOPT).
3. **Basis:** a written, read-only pdf.js viewer with a documented API (`PdfViewer`, `PdfSource`, find, password flow, range transport, headless pieces). 0.3.0 added what ViewRight's viewer needed to sit on top of it: `pageLayers`, `unloadOffscreenPages`, `annotationLayer`, `searchLayer`, `passwordPrompt` (CHANGELOG 0.3.0).
4. **Side by side:** [V]
   - **MailRight** uses the component as designed: `PdfViewer` with `source` bytes, `maxRenderPages=30`, `onPassword` and its own password form (`S/survey/mailright/src/components/AttachmentPdfPreview.tsx:106-116`).
   - **ViewRight (surveyed)** does not use `PdfViewer`. It keeps its own 1,760-line viewer and imports only helpers from pdf-ui (`pdfDocumentInit`, `pdfWasmUrl`, `thumbnailWindow`, `highlightSpan`, find functions): 11 import sites.
   - **ViewRight (live)** has no pdf-ui at all; `pdfLinkService.ts`, `pdfRenderKeepalive.ts`, `pdfThumbnailWindow.ts` were local and were moved into pdf-ui after the live build.
   - ViewRight's planned 10,411 lines did **not** land here. Overlap of those files with pdf-ui is 182 of 6,727 distinctive lines; they are in `@rightkit/document-core/src/documents/pdf` (34 files).
5. **Best shape:** two layers. `@rightkit/pdf-ui` stays the read-only viewer. ViewRight's editor (text edit, OCR overlay, save plan, native macOS pane) becomes a second entry built on `PdfViewer` through `pageLayers`: either `@rightkit/pdf-ui/editor` or a new `@rightkit/pdf-editor`. **pdf-ui itself is already the right shape for the viewer half**; the family as a whole is not, because the editor half sits in the wrong package and duplicates the viewer.
6. **Per app:** MailRight **fits** (already on 0.2.0). ViewRight **gaps**: its editor still runs its own viewer; adoption of `PdfViewer` with the 0.3.0 props is designed but, by the changelog's own statement, "no build, typecheck or browser run was made for this change".
7. **Boundary:** duplicates between pdf-ui and document-core [V]:
   - range transport: `pdf-ui/src/rangeTransport.ts:106` `PdfHttpRangeTransport` and `document-core/src/documents/pdf/pdfBenchTransport.ts:106` `HostPdfRangeTransport`;
   - `PdfPasswordPrompt` in both;
   - a full `PdfViewer` in both (709 and 1,760 lines).
8. **Actions:** build and run 0.3.0 once against ViewRight's four cases named in its changelog; move ViewRight's PDF editor out of document-core onto `PdfViewer` + `pageLayers`; delete the duplicate transport and prompt; publish 0.3.0.

Depth: deep. Confidence: high.

## 7. `rightkit-pdf` (cargo)

1. **Exists:** `RK/crates/rightkit-pdf`, 0.3.5, 16,795 lines; features `merge`, `edit`, `fonts`, `extract`, `raster`, `redact`, `forms`, `optimize`, `host`.
2. **Feeds:** ViewRight 503 (ADOPT: `pdf_adapter.rs`, `system_fonts.rs`, `native_pdf.rs`), ScrapeRight 129 (EXTEND: `extraction/pdf.rs`).
3. **Basis:** ViewRight's PDF editor core moved earlier (STATUS.md: "PDF engine ... local code deleted"), plus Legion merge and ScrapeRight text extraction donors (Cargo.toml comments).
4. **Side by side:** [V]
   - ViewRight consumes 0.3.3 with `host`; its remaining 503 lines are Tauri command wrappers. It rasterises pages for OCR in the webview with pdf.js and posts the image to `ocr_page`.
   - ScrapeRight has no webview. `S/survey/scraperight/crates/engine/src/extraction/pdf.rs:22-45` renders pages with PDFium (`pdfium-render` 0.9.4, target width 2000) and `:47-80` binds the library only from inside the install bundle. rightkit-pdf's `raster` feature "draws image XObjects only (no PDFium)" (Cargo.toml), so a page with vector or mixed content cannot be rendered.
5. **Best shape:** today's feature layout, plus one optional full-page render feature for headless callers (PDFium bound at run time with ScrapeRight's bundle-only loader rule). **Not yet that shape** for ScrapeRight.
6. **Per app:** ViewRight **fits**. ScrapeRight **gaps** (page render).
7. **Boundary:** `rightkit-docs` re-exposes PDF read and edit as a facade over this crate; acceptable, one implementation. No other problem found.
8. **Actions:** add a `render` feature (PDFium, dynamic, bundle-restricted) or record the decision that scanned-page rendering stays in ScrapeRight; ViewRight moves its pin from 0.3.3 to the current version.

Depth: deep on the two apps' use, light on the crate's 16k lines (it has ten earlier dedicated reviews under `RK/docs/audits/2026-10-1*-astra-pdf-*`). Confidence: medium.

## 8. `@rightkit/doc-render` (npm)

1. **Exists:** `RK/packages/doc-render`, 0.2.2 unpublished (0.2.1 pinned by apps).
2. **Feeds:** none in the plan.
3. **Basis:** ViewRight's Markdown reader pieces (the live build still has `remarkDefListAbbr.ts`, `detectors/*`, `citations/*`, `remarkWikiLink.ts` locally) behind a written API: `MarkdownRenderer`, `HtmlRenderer`, `parseToTree`, sanitizer profiles, Shiki.
4. **Side by side:** consumers are ViewRight, CodeRight and ScrapeRight (all 0.2.1) and RightKit's `agent-ui-*`, `library-ui`, `markdown-editor`. [V] from manifests; their call sites were not compared.
5. **Best shape:** today's. Options default off, policies are explicit, no host or Tauri import.
6. **Per app:** ViewRight, CodeRight, ScrapeRight **fits** (already pinned). 
7. **Boundary:** the second Mermaid sanitizer noted under package 2.
8. **Actions:** settle the Mermaid policy with markdown-editor; publish 0.2.2.

Depth: light. Confidence: medium.

## 9. `@rightkit/document-core` (npm)

1. **Exists:** `RK/packages/document-core`, 0.1.2 unpublished, about 18,000 lines. **Not in the master plan**; it was created to break the import cycle between the workspace and the editors.
2. **Feeds:** no planned lines of its own. It holds ViewRight code the plan assigned to `@rightkit/pdf-ui` (the 34 files of `documents/pdf`, about 10,000 lines) and to `@rightkit/document-workspace` (bridge, settings, chrome, printing).
3. **Basis:** surveyed ViewRight, 0.98 similar.
4. **Side by side:** one app.
5. **Best shape:** a small base: document types and the format registry, the host bridge **interface**, toolbar and dialog primitives, find helpers, the app-name store. **Today's package is a catch-all**, so its boundaries should be redrawn:
   - `state/tauriBridge.ts` (919 lines): ViewRight's concrete Tauri bridge, 57 fixed command names, stream scheme `viewright-stream` (`:136`), header `x-viewright-path-hex` (`:376`). [V]
   - `documents/pdf/*`: the whole PDF editor, including a benchmark harness (`pdfBench.ts` 688, `pdfBenchAutopilot.ts` 183, `pdfBenchTransport.ts` 284) that the plan mapped to `rightkit-bench`. [V]
   - `settings/*`: ViewRight's settings store, theme list with `viewright-paper` as default (`themes.ts:44,178`) and migration of old ViewRight theme ids (`settings.tsx:129-132`), file-association UI. [V]
   - `state/recentUrls.ts:6` storage key `viewright:recent-urls`; `app/shell/nativeMenu.ts:16,21` event names; `state/bridgeEvents.ts:7,26-40`. [V]
   - `documents/perfEventCounters.ts:101-119` installs `window.__viewrightQaPerf`. [V]
6. **Per app:** ViewRight **gaps** (works for it by construction once names are settled). Any other app: **wrong**, because adopting any editor pulls in ViewRight's bridge, settings schema and PDF editor.
7. **Boundary:** as above; plus the format table exists three times with no check that they agree: `RK/crates/rightkit-docs/data/document_formats.tsv`, `document-core/src/shared/document_formats.tsv` (byte-identical today) and the generated `documentFormatsTsv.ts`. [V] ViewRight had one file read by both sides.
8. **Actions:**
   1. Move `documents/pdf` out (package 6).
   2. Turn `tauriBridge` into an interface with a default Tauri implementation whose command and scheme names come from one configuration object; pair it with the Rust command set (package 5).
   3. Move themes, settings sections and file associations to the workspace package or to ViewRight; keep only the settings port here.
   4. Remove the benchmark harness and QA globals from the shipped package.
   5. Generate the TypeScript format table from the `rightkit-docs` file at build time, or add a gate check.
   6. Add it to the plan.

Depth: deep. Confidence: high.

## 10. `rightkit-docs` (cargo)

1. **Exists:** `RK/crates/rightkit-docs`, 0.1.4, 4,992 lines.
2. **Feeds:** ViewRight 695 (EXTEND: `documents.rs`, 363 of 373 distinctive lines present).
3. **Basis:** a written DOCX/XLSX/PDF read and write library (typed ops, caps, loss receipts), plus feature `files` which is ViewRight's `documents.rs` moved in.
4. **Side by side:** [V] Pulse uses the core (`S/survey/pulse/core/src/remote/docs.rs:412` `rightkit_docs::read`, `docs_write.rs:106` `write`, pinned 0.1.2). ViewRight needs only `files`. The two halves share nothing but the crate name.
5. **Best shape:** core as it is. The `files` module is viewer file IO; its `RawDocument` carries `last_view`, `view_meta` and `stream_token` (`files/mod.rs:112-138`), which are workspace concepts. It belongs with `rightkit-document-workspace`, which already redeclares the same types.
6. **Per app:** Pulse **fits**. ViewRight **gaps** (the duplicate types and seam).
7. **Boundary:** `files` in the wrong crate; duplicate types (package 5); format table copies (package 9).
8. **Actions:** move `files` into `rightkit-document-workspace` (or make that crate depend on it and delete its copies); keep the format table in one place.

Depth: deep on `files` and consumers, light on DOCX/XLSX internals. Confidence: high on the boundary.

## 11. `rightkit-typeset` (cargo)

1. **Exists:** `RK/crates/rightkit-typeset`, 0.1.2, 593 lines.
2. **Feeds:** CutRight 6,243 lines, all VENDOR-KEEP (the `citationberg` fork), no app code.
3. **Basis:** fresh small wrapper over Typst and mitex.
4. **Side by side:** one consumer: `S/survey/cutright/crates/cutright-captions/src/lib.rs:18` re-exports `rightkit_typeset::font`, pinned 0.1.0. [V]
5. **Best shape:** today's (`Typesetter`, `Request`, `Source`, `Output`, explicit fonts, no file access).
6. **Per app:** CutRight **fits**.
7. **Boundary:** the `citationberg` fork exists in `RK/vendor/citationberg` and again in `S/survey/cutright/vendor/citationberg`; every consuming app must write its own `[patch.crates-io]` (Cargo.toml `consumer_patches`). [V] That is duplicate vendored code until upstream fixes the advisories.
8. **Actions:** give apps one source for the fork (a git patch pointing at the RightKit repository path) so CutRight deletes its copy; otherwise none.

Depth: light. Confidence: medium.

## 12. `rightkit-ocr` (cargo)

1. **Exists:** `RK/crates/rightkit-ocr`, 0.2.4, 3,313 lines; features `native`, `onnx`; worker child process support.
2. **Feeds:** ScrapeRight 586 (ADOPT), ViewRight 401 (ADOPT).
3. **Basis:** written crate; both apps' own engines were already replaced by it.
4. **Side by side:** [V] four apps pin 0.2.3:
   - ViewRight: `native` only; recognition runs in the crate's isolated worker (`ocr_worker.rs:44` `WorkerCommand::current_exe`), with app policy on top in `commands/ocr.rs` (one probe at a time `:210`, macOS canary after each page `:126-142`, bounded waits).
   - ScrapeRight: `native` everywhere, `onnx` on Windows only; app-owned parts are the model location, model lease and verification (`crates/ocr/src/lib.rs:115-143`) passed through the crate's `ModelLeaseHook`.
   - CutRight: `Ocr` with `OnnxConfig` (`cutright-engine/src/moments_cmd.rs:22`). CodeRight: `native` (`engine/crates/media/src/lib.rs:473`).
   The remaining app lines are policy and wiring, not second implementations.
5. **Best shape:** today's. Feature split, lease hook and worker cover all four uses.
6. **Per app:** ViewRight, ScrapeRight, CutRight, CodeRight **fits**.
7. **Boundary:** none. [S] ViewRight's single-probe and canary policy could become a helper here if a second desktop app needs it; not needed now.
8. **Actions:** none; apps move from 0.2.3 to the current version.

Depth: deep on app use, light on crate internals. Confidence: high.

---

## Family summary

| Package | Kind | Shape | One line |
|---|---|---|---|
| `@rightkit/document-workspace` | npm | wrong | It is the ViewRight app, legal gate and licence UI included |
| `@rightkit/document-core` | npm | wrong | Catch-all: ViewRight's bridge, settings, themes and the whole PDF editor; not in the plan |
| `@rightkit/markdown-editor` | npm | gaps | Sound modules; host wiring by `viewright:*` globals |
| `@rightkit/html-editor` | npm | gaps | Hard-coded `viewright-site` and `viewright-preview` schemes |
| `@rightkit/docx-editor` | npm | gaps | Follows the owner's decision, but the carried patch cannot apply to the published editor |
| `@rightkit/pdf-ui` | npm | gaps | Viewer is right and MailRight fits; ViewRight's editor landed in document-core with duplicates |
| `rightkit-document-workspace` | cargo | gaps | Good trait design; duplicate types with rightkit-docs; no command layer |
| `rightkit-pdf` | cargo | gaps | No full-page render for ScrapeRight |
| `rightkit-docs` | cargo | gaps | `files` feature is workspace code in the wrong crate |
| `@rightkit/doc-render` | npm | best | Designed API, three apps already on it |
| `rightkit-typeset` | cargo | best | Small and neutral; fork needs one source |
| `rightkit-ocr` | cargo | best | Four apps on it with only policy left app-side |

Counts: best 3, gaps 7, wrong 2, missing 0.

The order of work that removes the most duplication:

1. Decide the PDF editor's home and move it out of `document-core` onto `pdf-ui`'s `PdfViewer`.
2. Reduce `document-core` to types, format registry, bridge interface and primitives; one app-name store.
3. Pair the bridge with a Rust command set in `rightkit-document-workspace`, and fold `rightkit-docs` `files` into it.
4. Make the DOCX toolbar work on the published eigenpal package, then delete ViewRight's vendored fork.
5. Take the legal, licence and update layer out of `document-workspace`.
6. Only then cut ViewRight over, and test against the live 0.1.123 behaviour, since the packages come from a tree that was never released.

Not available or not checked: CodeRight source was present and used only for manifest pins; Legion source is not available; nothing was built or run.
