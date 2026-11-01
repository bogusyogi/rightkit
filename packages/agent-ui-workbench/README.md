# @rightkit/agent-ui-workbench

Workbench and runtime panels for coding-agent UIs.

| Item | Value |
|---|---|
| Where it runs | App webview UI (React) |
| Version | 0.1.0 |
| License | MIT OR Apache-2.0 |

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/agent-ui-workbench` | Every module whose exported names do not collide (generated barrel). |
| `@rightkit/agent-ui-workbench/<path>` | Any module under `src/` by path without extension, e.g. `@rightkit/agent-ui-workbench/appName`. |
| `@rightkit/agent-ui-workbench/<path>.css` | Stylesheets shipped next to their components. |

## Depends on

`@rightkit/agent-ui-core`, `@rightkit/agent-ui-hooks`

## Peer dependencies

`react`

## Product name

User-facing copy reads the product name from `hostAppName()` (default `"App"`). Call
`setHostAppName("Your App")` (exported from the root and from `appName`) once at startup, before
rendering.

## Host contract

Hosts bundle this package with Vite (the build keeps extensionless relative imports and asset imports
such as `?raw`). Tailwind hosts add `node_modules/@rightkit/agent-ui-workbench/dist/**/*.js` to their content globs.

## Testing

```sh
pnpm --dir packages/agent-ui-workbench test
```

Modules left out of the root barrel because an export name collides with an earlier module
(import them by subpath): `components/runtime/panels/BenchmarksPanel.tsx`, `components/runtime/panels/LearningAuditPanels.tsx`, `components/runtime/panels/RolloutsFinetuningPanels.tsx`, `components/runtime/panels/TasteMetricsPanels.tsx`, `components/runtime/panels/shared.tsx`, `components/runtime/runtimePaneTypes.ts`, `components/runtime/runtimeTabsRenderRows.tsx`, `components/runtime/runtimeUtils.ts`
