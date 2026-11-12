# @rightkit/document-workspace

Multi-format document workspace shell a host configures: tabs, sidebar, document router, loaders, savers, actions, settings and the PDF editor pane. It names no product and carries no product's legal, licence or update wiring.

| Item | Value |
|---|---|
| Where it runs | App webview UI (React) |
| Version | 0.2.1 |
| License | MIT OR Apache-2.0 |

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/document-workspace` | Every module whose exported names do not collide (generated barrel). |
| `@rightkit/document-workspace/<path>` | Any module under `src/` by path without extension, e.g. `@rightkit/document-workspace/app/App`. |
| `@rightkit/document-workspace/<path>.css` | Stylesheets shipped next to their components. |

## Depends on

`@rightkit/document-core`, `@rightkit/docx-editor`, `@rightkit/html-editor`, `@rightkit/markdown-editor`, `@rightkit/pdf-ui`, `@rightkit/media-ui`, `@rightkit/onboarding`, `@rightkit/settings-ui`

## Peer dependencies

`react`, `react-dom`

## Host data (legal)

The package does not import the host's legal files. The host passes them in once at startup with
`configureLegal({ manifest, contract })` (exported from the root and from `state/legal`):

- `manifest`: the host's `legal/legal-manifest.json`, parsed. It must have a `documents` array.
- `contract`: the host's generated `legal-contract.json`, with `acceptanceVersion` and `materialSha256` strings.

Until `configureLegal` runs, the legal exports (`REQUIRED_LEGAL_ACCEPTANCE_VERSION`,
`LEGAL_MATERIAL_SHA256`, `LEGAL_GATE_DOCUMENTS`, `LEGAL_ABOUT_LINKS`) are empty or unset, so call it
before the first render. The gate fails closed: without `configureLegal` (`legalConfigured()` false) or
without `firstRun`, `AppRoot` renders the blocking "Legal configuration missing" screen and never the
workspace (`missingLegalConfiguration()` names what is missing). The exports are live bindings, so every importer sees the configured values.


## Host configuration

Call `configureDocumentWorkspace(config)` (root export, `config` module) once at startup, before the
first render. Nothing product-specific is built in; everything comes from this object:

| Field | Meaning |
|---|---|
| `appName` | Product name in user-facing copy. Sets the one name store of every document package. |
| `id` | The host's native identifier, prefix of event names, schemes (`<id>-stream`, `<id>-site`, `<id>-preview`), the `x-<id>-path-hex` header, storage keys and QA globals. Default `"rk-document"`. See `@rightkit/document-core` (`configureDocumentHost`). |
| `appVersion` | The running build's version (diagnostics). Required. |
| `supportEmail` | Address for "Email to support" in Diagnostics. Absent: the action is not shown. |
| `firstRun` | The first-run legal gate: `legal` (the gate's theme, trust line, agreement sentence, wordmark font and the rest of `LegalStepConfig` from `@rightkit/onboarding`) and `gateWindow` (`{ width, height }`, the gate's own fixed window). Required: absent, `AppRoot` fails closed and renders a blocking "Legal configuration missing" screen (with a Quit button) instead of the workspace. |
| `settings.about` | The host's About page (brand card, updates, licence, legal documents), rendered as the first settings section. Absent: no About section. |
| `settings.sections` | Further settings sections, merged after the workspace's own (same id replaces). |
| `backgroundUpdateCheck` | Advisory update check run once per launch while automatic checks are on; resolve `{ version, tier }` to show a toast, or null. Absent: no check. |

The licence, update and About composition (`@rightkit/license`, `@rightkit/app-update`,
`@rightkit/settings-ui` `aboutSection`/`legalSection`/`UpdatesControls`) is the host's, passed in
through `settings.about` and `backgroundUpdateCheck`.

## PDF

The PDF pane (`documents/pdf/*`: viewer with text editing, OCR, save plan and the macOS native read
pane) moved here from `@rightkit/document-core` in 0.2.0. It uses `@rightkit/pdf-ui` for the range
transport, the password dialog, find and the pdf.js helpers; it still renders pages itself rather
than through `PdfViewer` (see the CHANGELOG). Hosts load `@rightkit/document-workspace/styles.css`,
which includes pdf.js's `pdf_viewer.css` and `@rightkit/pdf-ui/styles.css`.

## Host toolbar (DOCX)

The DOCX pane uses `@rightkit/docx-editor`, which needs the host-toolbar patch on
`@eigenpal/docx-editor-react@1.9.0` (see that package's README). Until the workspace root registers
the patch, a host must apply it or the DOCX pane shows the editor's built-in toolbar.

## Host configuration (environment and state)

- The ProseMirror packages used here (`prosemirror-view` and `prosemirror-transform`) are pinned exactly in the
  dependent packages. A host must not install a second copy of either, or plugin keys break.

## Host contract

Hosts bundle this package with Vite (the build keeps extensionless relative imports and asset imports
such as `?raw`). Tailwind hosts add `node_modules/@rightkit/document-workspace/dist/**/*.js` to their content globs.

## Testing

```sh
pnpm --dir packages/document-workspace test
```

Modules left out of the root barrel because an export name collides with an earlier module
(import them by subpath): `app/useDocuments.ts`, `documents/html/HtmlDocumentProps.ts`, `documents/pdf/pdfBridge.ts`, `documents/txt/sourceEditorTypes.ts`, `documents/txt/sourceFormatting.ts`
