# @rightkit/document-workspace

Multi-format document workspace shell: tabs, sidebar, document router, loaders, savers and actions.

| Item | Value |
|---|---|
| Where it runs | App webview UI (React) |
| Version | 0.1.0 |
| License | MIT OR Apache-2.0 |

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/document-workspace` | Every module whose exported names do not collide (generated barrel). |
| `@rightkit/document-workspace/<path>` | Any module under `src/` by path without extension, e.g. `@rightkit/document-workspace/app/App`. |
| `@rightkit/document-workspace/<path>.css` | Stylesheets shipped next to their components. |

## Depends on

`@rightkit/document-core`, `@rightkit/docx-editor`, `@rightkit/html-editor`, `@rightkit/markdown-editor`

## Peer dependencies

`react`, `react-dom`

## Host data (legal)

The package does not import the host's legal files. The host passes them in once at startup with
`configureLegal({ manifest, contract })` (exported from the root and from `state/legal`):

- `manifest`: the host's `legal/legal-manifest.json`, parsed. It must have a `documents` array.
- `contract`: the host's generated `legal-contract.json`, with `acceptanceVersion` and `materialSha256` strings.

Until `configureLegal` runs, the legal exports (`REQUIRED_LEGAL_ACCEPTANCE_VERSION`,
`LEGAL_MATERIAL_SHA256`, `LEGAL_GATE_DOCUMENTS`, `LEGAL_ABOUT_LINKS`) are empty or unset, so call it
before the first render. The exports are live bindings, so every importer sees the configured values.

The `@rightkit-host` alias is only used by this package's own test fixture (`test/host-fixture`,
set in `vitest.config.ts`). The `@rightkit-host/*` module declarations in `src/env.d.ts` describe the
same files for type-checking; a host does not need to alias them to run the package.

## Product name

User-facing copy reads the product name from `hostAppName()` (default `"App"`). Call
`setHostAppName("Your App")` (exported from the root and from `appName`) once at startup, before
rendering. Accent preset names and copy are read at display time, so the order matters only for
values the host reads at import.

## Host toolbar (DOCX)

The DOCX pane uses `@rightkit/docx-editor`, which needs the host-toolbar patch on
`@eigenpal/docx-editor-react@1.9.0` (see that package's README). Until the workspace root registers
the patch, a host must apply it or the DOCX pane shows the editor's built-in toolbar.

## Host configuration (environment and state)

- `VITE_RIGHTKIT_BROWSER_QA=1` at build time enables browser-QA fixtures. A host's QA build sets
  it; release builds leave it unset.
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
(import them by subpath): `app/useDocuments.ts`, `documents/html/HtmlDocumentProps.ts`, `documents/txt/sourceEditorTypes.ts`, `documents/txt/sourceFormatting.ts`
