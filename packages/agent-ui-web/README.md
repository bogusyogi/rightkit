# @rightkit/agent-ui-web

Browser client for a coding-agent daemon: transcript, input bar, diff, model and usage panels.

| Item | Value |
|---|---|
| Where it runs | App webview UI (React) |
| Version | 0.1.0 |
| License | MIT OR Apache-2.0 |

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/agent-ui-web` | Every module whose exported names do not collide (generated barrel). |
| `@rightkit/agent-ui-web/<path>` | Any module under `src/` by path without extension, e.g. `@rightkit/agent-ui-web/App`. |
| `@rightkit/agent-ui-web/<path>.css` | Stylesheets shipped next to their components. |

## Peer dependencies

`react`, `react-dom`

## Dev host

`app/` holds the Vite dev host (index.html, vite.config.ts, service worker) for running the client
on its own; it is not published. `RIGHTKIT_DAEMON_URL` selects the daemon the dev server proxies to.

## Product name

User-facing copy reads the product name from `hostAppName()` (default `"App"`). Call
`setHostAppName("Your App")` (exported from the root and from `appName`) once at startup, before
rendering.

## Host contract

Hosts bundle this package with Vite (the build keeps extensionless relative imports and asset imports
such as `?raw`). Tailwind hosts add `node_modules/@rightkit/agent-ui-web/dist/**/*.js` to their content globs.

## Testing

```sh
pnpm --dir packages/agent-ui-web test
```
