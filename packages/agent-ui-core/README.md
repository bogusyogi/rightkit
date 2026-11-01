# @rightkit/agent-ui-core

Coding-agent UI core: daemon client, session and transcript types, stores and helpers.

| Item | Value |
|---|---|
| Where it runs | App webview UI (React) |
| Version | 0.1.0 |
| License | MIT OR Apache-2.0 |

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/agent-ui-core` | Every module whose exported names do not collide (generated barrel). |
| `@rightkit/agent-ui-core/<path>` | Any module under `src/` by path without extension, e.g. `@rightkit/agent-ui-core/appName`. |
| `@rightkit/agent-ui-core/<path>.css` | Stylesheets shipped next to their components. |

## Peer dependencies

`react`

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
such as `?raw`). Tailwind hosts add `node_modules/@rightkit/agent-ui-core/dist/**/*.js` to their content globs.

## Testing

```sh
pnpm --dir packages/agent-ui-core test
```

Modules left out of the root barrel because an export name collides with an earlier module
(import them by subpath): `lib/appSurfaceProps/backgroundTasksTypes.ts`, `lib/daemon.ts`, `lib/daemon/config-host.ts`, `lib/promptTemplates/roles.ts`, `lib/promptTemplates/styles.ts`, `lib/promptTemplates/templates.ts`, `lib/shortcuts/schema.ts`, `lib/types.ts`, `lib/types/cliProviders.ts`, `lib/types/protocol.ts`, `lib/types/remote-schedules.ts`, `lib/types/settings-plugins.ts`, `lib/worktrees/worktreeProjectionManager.ts`, `state/composerStore.ts`
