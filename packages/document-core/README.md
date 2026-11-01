# @rightkit/document-core

Shared base for the document editors: platform bridge, settings, state, PDF helpers and the format registry.

| Item | Value |
|---|---|
| Where it runs | App webview UI (React) |
| Version | 0.1.0 |
| License | MIT OR Apache-2.0 |

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/document-core` | Every module whose exported names do not collide (generated barrel). |
| `@rightkit/document-core/<path>` | Any module under `src/` by path without extension, e.g. `@rightkit/document-core/app/BlockDraftContext`. |
| `@rightkit/document-core/<path>.css` | Stylesheets shipped next to their components. |

## Peer dependencies

`react`, `react-dom`

## Product name

User-facing copy reads the product name from `hostAppName()` (default `"App"`). Call
`setHostAppName("Your App")` (exported from the root and from `appName`) once at startup, before
rendering.

## Host contract

Hosts bundle this package with Vite (the build keeps extensionless relative imports and asset imports
such as `?raw`). Tailwind hosts add `node_modules/@rightkit/document-core/dist/**/*.js` to their content globs.

## Testing

```sh
pnpm --dir packages/document-core test
```

Modules left out of the root barrel because an export name collides with an earlier module
(import them by subpath): `documents/pdf/macPdfNativeBridge.ts`, `documents/pdf/pdfPrinting.ts`, `documents/previewViewport.tsx`, `state/tauriBridge.ts`
