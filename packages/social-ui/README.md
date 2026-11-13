# @rightkit/social-ui

Social media management UI: accounts, drafts, approvals and analytics screens with their data layer.

| Item | Value |
|---|---|
| Where it runs | App webview UI (React) |
| Version | 0.1.0 |
| License | MIT OR Apache-2.0 |

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/social-ui` | Every module whose exported names do not collide (generated barrel). |
| `@rightkit/social-ui/<path>` | Any module under `src/` by path without extension, e.g. `@rightkit/social-ui/appName`. |
| `@rightkit/social-ui/<path>.css` | Stylesheets shipped next to their components. |

## Peer dependencies

`react`

## Product name

User-facing copy reads the product name from `hostAppName()` (default `"App"`). Call
`setHostAppName("Your App")` (exported from the root and from `appName`) once at startup, before
rendering.

## Host contract

Hosts bundle this package with Vite (the build keeps extensionless relative imports and asset imports
such as `?raw`). Tailwind hosts add `node_modules/@rightkit/social-ui/dist/**/*.js` to their content globs.

## Testing

```sh
pnpm --dir packages/social-ui test
```

Modules left out of the root barrel because an export name collides with an earlier module
(import them by subpath, `@rightkit/social-ui/data` and `@rightkit/social-ui/data/source`): `data/index.ts`, `data/source.ts`
