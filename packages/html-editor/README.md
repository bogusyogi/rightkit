# @rightkit/html-editor

HTML document editor and viewer: ProseMirror editing, sanitising and source view.

| Item | Value |
|---|---|
| Where it runs | App webview UI (React) |
| Version | 0.1.0 |
| License | MIT OR Apache-2.0 |

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/html-editor` | Every module whose exported names do not collide (generated barrel). |
| `@rightkit/html-editor/<path>` | Any module under `src/` by path without extension, e.g. `@rightkit/html-editor/appName`. |
| `@rightkit/html-editor/<path>.css` | Stylesheets shipped next to their components. |

## Depends on

`@rightkit/document-core`

## Peer dependencies

`react`

## Product name

User-facing copy reads the product name from `hostAppName()` (default `"App"`). Call
`setHostAppName("Your App")` (exported from the root and from `appName`) once at startup, before
rendering.

## Host contract

Hosts bundle this package with Vite (the build keeps extensionless relative imports and asset imports
such as `?raw`). Tailwind hosts add `node_modules/@rightkit/html-editor/dist/**/*.js` to their content globs.

## Testing

```sh
pnpm --dir packages/html-editor test
```

Modules left out of the root barrel because an export name collides with an earlier module
(import them by subpath): `documents/html/htmlOperationPlanner.ts`, `documents/html/htmlPmBlockHelpers.ts`
