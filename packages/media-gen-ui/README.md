# @rightkit/media-gen-ui

Generative media studio UI: generate, jobs, gallery, recipes, voices and production panels.

| Item | Value |
|---|---|
| Where it runs | App webview UI (React) |
| Version | 0.1.0 |
| License | MIT OR Apache-2.0 |

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/media-gen-ui` | Every module whose exported names do not collide (generated barrel). |
| `@rightkit/media-gen-ui/<path>` | Any module under `src/` by path without extension, e.g. `@rightkit/media-gen-ui/App`. |
| `@rightkit/media-gen-ui/<path>.css` | Stylesheets shipped next to their components. |

## Peer dependencies

`react`, `react-dom`

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
such as `?raw`). Tailwind hosts add `node_modules/@rightkit/media-gen-ui/dist/**/*.js` to their content globs.

## Testing

```sh
pnpm --dir packages/media-gen-ui test
```

Modules left out of the root barrel because an export name collides with an earlier module
(import them by subpath): `lib/engine-error.ts`, `lib/gen-params.ts`, `lib/model-management.ts`, `lib/production-stack-builders.ts`, `lib/types/avatar.ts`, `lib/types/gallery.ts`, `lib/types/hyperframes.ts`, `lib/types/image-studio.ts`, `lib/types/image.ts`, `lib/types/local-image.ts`, `lib/types/video.ts`, `lib/types/workflow.ts`, `views/panels/character-lock.ts`, `views/panels/video-quality.ts`, `views/rentals-logic.ts`, `views/template-swap-logic.ts`
