# @rightkit/terminal-ui

Persistent terminal pane and its React hook.

| Item | Value |
|---|---|
| Where it runs | App webview UI (React) |
| Version | 0.1.0 |
| License | MIT OR Apache-2.0 |

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/terminal-ui` | Every module whose exported names do not collide (generated barrel). |
| `@rightkit/terminal-ui/<path>` | Any module under `src/` by path without extension, e.g. `@rightkit/terminal-ui/components/PersistentTerminal`. |
| `@rightkit/terminal-ui/<path>.css` | Stylesheets shipped next to their components. |

## Depends on

`@rightkit/agent-ui-core`

## Peer dependencies

`react`

## Host contract

Hosts bundle this package with Vite (the build keeps extensionless relative imports and asset imports
such as `?raw`). Tailwind hosts add `node_modules/@rightkit/terminal-ui/dist/**/*.js` to their content globs.

## Testing

```sh
pnpm --dir packages/terminal-ui test
```
