# Changelog

## 0.1.2 (unpublished)

Shape review 2026-11-11 (`docs/audits/2026-11-11-shape-matrix/reports/family-documents.md`; design in `reports/documents-redraw-design.md`). Taken on the recommended option, pending the owner's confirmation.

- `appName` re-exports the one store in `@rightkit/document-core/host` (was a separate copy), so a host names the product once for every document package. `setHostAppName` and `hostAppName` are still exported. Requires `@rightkit/document-core` 0.2.0.
- The eigenpal dependency and the host-toolbar patch are unchanged (decision 7 is open; the two options are recorded in the design note).

## 0.1.1

- Ask 4: pins `prosemirror-view` 1.42.8 and `prosemirror-transform` 1.12.4 (were 1.42.6 and ^1.12.2), matching the host so one copy of each ProseMirror package is in use.
- Ask 5: the host-toolbar patch (`patches/docx-editor-react-host-toolbar.patch`) is documented as the source of `renderToolbar`. Registering it in the workspace root `patchedDependencies` is still pending.
- `DocxPane` no longer imports the editor stylesheet itself; `styles.css` already imports it, so hosts import `@rightkit/docx-editor/styles.css` once.
- Ask 1 review: `HostDocxToolbar` matches the host toolbar apart from import paths and the component name, so no behaviour was ported. Host names in budget, blank-document and find copy read from `hostAppName()`.

## 0.1.0

- First release. DOCX editing pane over @eigenpal/docx-editor with a host-owned toolbar, find, print and Markdown export.
