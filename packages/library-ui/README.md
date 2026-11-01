# @rightkit/library-ui

Library and queue UI for transcripts and downloads: queue board, job cards, library list, detail reader and history.

| Item | Value |
|---|---|
| Where it runs | App webview UI (React) |
| Version | 0.1.0 |
| License | MIT OR Apache-2.0 |

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/library-ui` | Every module whose exported names do not collide (generated barrel). |
| `@rightkit/library-ui/<path>` | Any module under `src/` by path without extension, e.g. `@rightkit/library-ui/appName`. |
| `@rightkit/library-ui/<path>.css` | Stylesheets shipped next to their components. |

## Depends on

`@rightkit/dictation-ui`

## Peer dependencies

`react`, `react-dom`

## Host data

Legal and version data stay in the host app. Alias `@rightkit-host` to the host project root
(Vite `resolve.alias`); the package imports `@rightkit-host/legal/legal-manifest.json`,
`@rightkit-host/src/generated/legal-contract.json`, `@rightkit-host/package.json` and
`@rightkit-host/legal/*.md?raw` where it needs them.

## Product name

User-facing copy reads the product name from `hostAppName()` (default `"App"`). Call
`setHostAppName("Your App")` (exported from the root and from `appName`) once at startup, before
rendering.

## Host contract

Hosts bundle this package with Vite (the build keeps extensionless relative imports and asset imports
such as `?raw`). Tailwind hosts add `node_modules/@rightkit/library-ui/dist/**/*.js` to their content globs.

## Testing

```sh
pnpm --dir packages/library-ui test
```

Modules left out of the root barrel because an export name collides with an earlier module
(import them by subpath): `queue/contract.ts`, `queue/lib/historyPage.ts`, `queue/lib/ipc/qaMock.ts`, `queue/lib/mediaSelection.ts`, `queue/views/DetailReader.tsx`
