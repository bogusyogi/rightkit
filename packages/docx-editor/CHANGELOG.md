# Changelog

## 0.1.1

- Ask 4: pins `prosemirror-view` 1.42.8 and `prosemirror-transform` 1.12.4 (were 1.42.6 and ^1.12.2), matching the host so one copy of each ProseMirror package is in use.
- Ask 5: the host-toolbar patch (`patches/docx-editor-react-host-toolbar.patch`) is documented as the source of `renderToolbar`. Registering it in the workspace root `patchedDependencies` is still pending.
- `DocxPane` no longer imports the editor stylesheet itself; `styles.css` already imports it, so hosts import `@rightkit/docx-editor/styles.css` once.
- Ask 1 review: `HostDocxToolbar` matches the host toolbar apart from import paths and the component name, so no behaviour was ported. Host names in budget, blank-document and find copy read from `hostAppName()`.

## 0.1.0

- First release. DOCX editing pane over @eigenpal/docx-editor with a host-owned toolbar, find, print and Markdown export.
