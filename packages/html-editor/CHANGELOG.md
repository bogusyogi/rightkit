# Changelog

## 0.2.0 (unpublished)

Shape review 2026-11-11 (`docs/audits/2026-11-11-shape-matrix/reports/family-documents.md`; design in `reports/documents-redraw-design.md`). Taken on the recommended option, pending the owner's confirmation.

- Changed (breaking): window events shared with the workspace are `<id>:undo`, `<id>:redo`, `<id>:print`, `<id>:html-reload`, `<id>:html-select-all` (`documentEvent` from `@rightkit/document-core/host`; were `viewright:*`). The site scheme in the base URL and both CSPs is `<id>-site` (`hostScheme("site")`, was `viewright-site`); the preview scheme comes from document-core's `previewScheme()`. A host passes its `id` to `configureDocumentHost`; ViewRight keeps its names with `id: "viewright"`.
- Changed (breaking): the postMessage protocol between the parent and the interactive preview's bridge script uses fixed neutral names `rk-document:interactive-html-*` (were `viewright:interactive-html-*`). Both ends are in this package.
- `appName` re-exports the one store in `@rightkit/document-core/host` (was a separate copy).
- `app/shortcutChords` moved here from `@rightkit/document-core` (its only user).
- Still `viewright`-named, for this package's own step: `data-viewright-href`, the `viewrightTheme` frame dataset, the `viewright-html-inframe-style` element id and the `open.viewright.invalid` link broker origin.

## 0.1.1

- Ask 4: pins `prosemirror-view` 1.42.8 (was 1.42.6), matching the host so one copy is in use and plugin keys survive. This package does not import `prosemirror-transform`.
- Doc comment wording fix only (`HtmlViewer` `onOpenPath`); no behaviour change.

## 0.1.0

- First release. HTML document editor and viewer: ProseMirror editing, sanitising and source view.
