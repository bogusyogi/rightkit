# @rightkit/workspace-ui

Calendar, contacts, drive, tasks and meeting views for workspace-style apps.

| Item | Value |
|---|---|
| Where it runs | App webview UI (React) |
| Version | 0.1.0 |
| License | MIT OR Apache-2.0 |

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/workspace-ui` | Every module whose exported names do not collide (generated barrel). |
| `@rightkit/workspace-ui/<path>` | Any module under `src/` by path without extension, e.g. `@rightkit/workspace-ui/lib/vcard`. |
| `@rightkit/workspace-ui/<path>.css` | Stylesheets shipped next to their components. |

## Depends on

`@rightkit/mail-ui`

## Peer dependencies

`react`

## Host contract

Hosts bundle this package with Vite (the build keeps extensionless relative imports and asset imports
such as `?raw`). Tailwind hosts add `node_modules/@rightkit/workspace-ui/dist/**/*.js` to their content globs.

## Testing

```sh
pnpm --dir packages/workspace-ui test
```
