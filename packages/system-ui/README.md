# @rightkit/system-ui

System monitor views (activity, storage, cleanup, duplicates, drive health, apps, chat) and chart helpers for desktop utility apps.

| Item | Value |
|---|---|
| Where it runs | App webview UI (React) |
| Version | 0.1.0 |
| License | MIT OR Apache-2.0 |

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/system-ui` | Every module whose exported names do not collide (generated barrel). |
| `@rightkit/system-ui/<path>` | Any module under `src/` by path without extension, e.g. `@rightkit/system-ui/activityLease`. |
| `@rightkit/system-ui/<path>.css` | Stylesheets shipped next to their components. |

## Peer dependencies

`react`

## Host modules

`lib/legal` imports `virtual:rightkit-legal` (legal documents, version, hash, receipt documents),
which the host's Vite legal plugin provides.

## Host contract

Hosts bundle this package with Vite (the build keeps extensionless relative imports and asset imports
such as `?raw`). Tailwind hosts add `node_modules/@rightkit/system-ui/dist/**/*.js` to their content globs.

## Testing

```sh
pnpm --dir packages/system-ui test
```
