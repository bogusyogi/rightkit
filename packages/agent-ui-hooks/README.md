# @rightkit/agent-ui-hooks

React hooks for coding-agent UIs: sessions, composer, workspace files, approvals and runtime.

| Item | Value |
|---|---|
| Where it runs | App webview UI (React) |
| Version | 0.1.0 |
| License | MIT OR Apache-2.0 |

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/agent-ui-hooks` | Every module whose exported names do not collide (generated barrel). |
| `@rightkit/agent-ui-hooks/<path>` | Any module under `src/` by path without extension, e.g. `@rightkit/agent-ui-hooks/appName`. |
| `@rightkit/agent-ui-hooks/<path>.css` | Stylesheets shipped next to their components. |

## Depends on

`@rightkit/agent-ui-core`

## Peer dependencies

`react`

## Product name

User-facing copy reads the product name from `hostAppName()` (default `"App"`). Call
`setHostAppName("Your App")` (exported from the root and from `appName`) once at startup, before
rendering.

## Host contract

Hosts bundle this package with Vite (the build keeps extensionless relative imports and asset imports
such as `?raw`). Tailwind hosts add `node_modules/@rightkit/agent-ui-hooks/dist/**/*.js` to their content globs.

## Testing

```sh
pnpm --dir packages/agent-ui-hooks test
```
