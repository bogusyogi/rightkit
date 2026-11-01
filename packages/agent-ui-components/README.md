# @rightkit/agent-ui-components

Coding-agent UI components: app frame, chat, composer, review, panels, delivery, remote and usage.

| Item | Value |
|---|---|
| Where it runs | App webview UI (React) |
| Version | 0.1.0 |
| License | MIT OR Apache-2.0 |

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/agent-ui-components` | Every module whose exported names do not collide (generated barrel). |
| `@rightkit/agent-ui-components/<path>` | Any module under `src/` by path without extension, e.g. `@rightkit/agent-ui-components/appName`. |
| `@rightkit/agent-ui-components/<path>.css` | Stylesheets shipped next to their components. |

## Depends on

`@rightkit/agent-ui-core`, `@rightkit/agent-ui-hooks`, `@rightkit/agent-ui-settings`, `@rightkit/agent-ui-workbench`, `@rightkit/dictation-ui`, `@rightkit/terminal-ui`

## Peer dependencies

`react`, `react-dom`

## Product name

User-facing copy reads the product name from `hostAppName()` (default `"App"`). Call
`setHostAppName("Your App")` (exported from the root and from `appName`) once at startup, before
rendering.

## Host contract

Hosts bundle this package with Vite (the build keeps extensionless relative imports and asset imports
such as `?raw`). Tailwind hosts add `node_modules/@rightkit/agent-ui-components/dist/**/*.js` to their content globs.

## Testing

```sh
pnpm --dir packages/agent-ui-components test
```

Modules left out of the root barrel because an export name collides with an earlier module
(import them by subpath): `components/VirtualSessionList.tsx`, `components/app/composerBarHelpers.ts`, `components/review/index.ts`, `components/settings/types.ts`, `hooks/app/useAppSurfaces.ts`, `lib/appSurfaceProps/index.ts`
