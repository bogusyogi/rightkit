# Changelog

## 0.2.0 (unpublished)

Shape review 2026-11-11 (`docs/audits/2026-11-11-shape-matrix/reports/family-documents.md`; design in `reports/documents-redraw-design.md`). Taken on the recommended option, pending the owner's confirmation.

- Changed (breaking): window events shared with the workspace are `<id>:undo`, `<id>:redo`, `<id>:print`, `<id>:open-find`, `<id>:open-url-tab` and the Markdown format event `markdownFormatEvent()` (`documentEvent` from `@rightkit/document-core/host`; were `viewright:*`). ViewRight keeps its names with `id: "viewright"`.
- Changed (breaking): names private to this package are fixed and neutral: the MDX postMessage protocol `rk-document:mdx-*`, and the ProseMirror meta keys `rk-document:markdown-intent`, `rk-document:markdown-find-replace` and `rk-document:code-fence-highlight` (were `viewright:*`).
- `appName` re-exports the one store in `@rightkit/document-core/host` (was a separate copy).
- `documents/scopedSelectAll` and `documents/mdx/mdxRuntimeScript` (with its `.txt`) moved here from `@rightkit/document-core` (their only user).
- Still `viewright`-named, for this package's own step: the local image scheme `viewright-local:`, the `__viewrightQaMarkdown` QA global, the `viewright.invalid` MDX base URL and the `viewright-code-fence-highlight` style id. The `<!-- viewright: KIND -->` hint prefix is a file format on users' disks and stays accepted.

## 0.1.1

- Ask 2: adds `markdownReading.css` (reading column typography and measure: `.reading`, `.vr-markdown-document-geometry`, `.vr-reading-measure`, `.vr-reading-container`, breakout rules and the `.vr-pm-markdown-doc` base). Driven by the host's `--doc-font`, `--doc-size`, `--doc-leading`, `--doc-fg` and `--doc-measure` (set by `@rightkit/document-core` from Settings). New export `./documents/markdown/markdownReading.css`, and it is imported from `styles.css`.
- Ask 3: the projection cache (`markdownToPmDoc`) is now a 24 MiB byte-bounded LRU instead of a 4-entry map. Entries are charged an estimate per source unit; documents over the budget are projected but not cached.
- Ask 4: pins `prosemirror-view` 1.42.8 and `prosemirror-transform` 1.12.4, matching the host, so the editor and host share one copy of each (plugin keys survive).

## 0.1.0

- First release. Markdown and MDX editor: ProseMirror WYSIWYG, source mode, math, diagrams and properties panel.
