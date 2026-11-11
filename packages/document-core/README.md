# @rightkit/document-core

Shared base for the document editors and the document workspace: document types and the format registry, the host bridge, drafts, diagnostics, settings store, find, print and toolbar primitives, and the one host-identity store.

| Item | Value |
|---|---|
| Where it runs | App webview UI (React) |
| Version | 0.2.0 |
| License | MIT OR Apache-2.0 |

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/document-core` | Every module whose exported names do not collide (generated barrel). |
| `@rightkit/document-core/<path>` | Any module under `src/` by path without extension, e.g. `@rightkit/document-core/app/BlockDraftContext`. |
| `@rightkit/document-core/<path>.css` | Stylesheets shipped next to their components. |

## Peer dependencies

`react`, `react-dom`

## Host identity

One store for every document package (`@rightkit/document-core/host`, also on the root). Call it once
at startup, before rendering; values are read where they are used, never at module load:

```ts
configureDocumentHost({ appName: "Your App", id: "yourapp" });
```

- `appName` (default `"App"`): product name in user-facing copy. `hostAppName()` reads it;
  `setHostAppName(name)` is the same as passing only `appName`. The editors' and the workspace's
  `appName` modules re-export this store.
- `id` (default `"rk-document"`): the prefix of every name shared with the host's native side or
  between the document packages: events `<id>:<name>` (`documentEvent`), schemes `<id>-stream`,
  `<id>-site`, `<id>-preview` (`hostScheme`), the raw-body path header `x-<id>-path-hex`
  (`hostHeader`), storage keys `<id>:<key>` (`hostStorageKey`) and QA globals `__<camelId><Name>`
  (`hostQaGlobal`). A host whose native side already uses its own prefix passes it here.

Not host-derived (fixed, with a later neutral rename recorded in the CHANGELOG): the theme ids
`viewright-paper` and `viewright-night` (stored in user settings) and the settings document key
`vr.settings.v1`.

## What moved out in 0.2.0

The PDF editor (`documents/pdf/*`, its native commands from `state/tauriBridge` and the PDF print
path), the settings sections and their stylesheet, and the hooks only the workspace used moved to
`@rightkit/document-workspace`; `app/shortcutChords` moved to `@rightkit/html-editor`;
`documents/scopedSelectAll` and `documents/mdx/mdxRuntimeScript` moved to `@rightkit/markdown-editor`.
See the CHANGELOG for the full list.

## Host contract

Hosts bundle this package with Vite (the build keeps extensionless relative imports and asset imports
such as `?raw`). Tailwind hosts add `node_modules/@rightkit/document-core/dist/**/*.js` to their content globs.

## Testing

```sh
pnpm --dir packages/document-core test
```

Modules left out of the root barrel because an export name collides with an earlier module
(import them by subpath): `documents/previewViewport.tsx`, `state/tauriBridge.ts`
