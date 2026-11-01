# @rightkit/dictation-ui

Dictation UI: recording pill, microphone and hotkey hooks, voice conversation and dictation pages.

| Item | Value |
|---|---|
| Where it runs | App webview UI (React) |
| Version | 0.1.0 |
| License | MIT OR Apache-2.0 |

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/dictation-ui` | Every module whose exported names do not collide (generated barrel). |
| `@rightkit/dictation-ui/<path>` | Any module under `src/` by path without extension, e.g. `@rightkit/dictation-ui/appName`. |
| `@rightkit/dictation-ui/<path>.css` | Stylesheets shipped next to their components. |

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
such as `?raw`). Tailwind hosts add `node_modules/@rightkit/dictation-ui/dist/**/*.js` to their content globs.

## Testing

```sh
pnpm --dir packages/dictation-ui test
```

Modules left out of the root barrel because an export name collides with an earlier module
(import them by subpath): `lib/license/accountSession.ts`, `lib/license/core.ts`, `lib/license/proBridge.ts`, `lib/license/windowsLink.ts`, `lib/licenseLifecycle.ts`, `lib/tauriBridge.ts`, `pill/pillBars.ts`, `voice/hooks/useVoiceConversation.ts`
