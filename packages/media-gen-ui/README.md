# @rightkit/media-gen-ui

Media-generation UI components over a host-supplied engine client: the generate form, job cards and
job list, gallery and library search, voices, local and rented models, routes, spend, recipes,
templates and production panels.

| Item | Value |
|---|---|
| Where it runs | App webview UI (React) |
| Version | 0.2.0 |
| License | MIT OR Apache-2.0 |

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/media-gen-ui` | Every module whose exported names do not collide (generated barrel). |
| `@rightkit/media-gen-ui/<path>` | Any module under `src/` by path without extension, e.g. `@rightkit/media-gen-ui/views/Generate`. |
| `@rightkit/media-gen-ui/<path>.css` | Stylesheets shipped next to their components. |

## Peer dependencies

`react`, `react-dom`

## What the host supplies

The package holds panels, not an app. The window, navigation, onboarding, settings screens, legal
documents, licence and update flows, and the browser preview are the host's (use
`@rightkit/react-shell`, `@rightkit/onboarding`, `@rightkit/settings-ui`, `@rightkit/legal-ui`,
`@rightkit/license` and `@rightkit/app-update` for them). Call these once at startup, before
rendering:

| Call | What it sets |
|---|---|
| `setMediaGenClient(client)` | The engine transport: `request(method, params)`, `approveDraft(...)`, `assetUrl(path)`, `isDesktop()`. Default: `tauriClient` (the `engine_request` and `approve_draft` Tauri commands, `convertFileSrc`). A browser preview or test harness passes its own; the package ships no mock. |
| `setHostAppName(name)` | The product name in user-facing copy (default `"App"`). |
| `setHostBrands(brands)` | The brand codes, names and descriptors for the brand pickers and gallery labels (default none). Register the same brands with the engine (`rightkit-prompt-recipes` `brands::set_brands`, `rightkit-media-gen` `gallery::set_brand_catalog`). |

Panels that link elsewhere call the host's `onNavigate` with a `ViewId` or a settings section id
(`lib/nav`); without one, Production dispatches a `media-gen:navigate` window event.

## Engine

The panels speak the JSON commands of `rightkit-media-gen` (`generate`, `job.*`, `media.*`,
`library.search`, `asset.*`, `spend.*`), `rightkit-prompt-recipes` with feature `commands`
(`studio.recipes.*`, `studio.catalog.*`, `studio.characters.*`, `studio.video_templates.*`),
`rightkit-media-gen-models` and `rightkit-remote-compute` (`studio.models.*`) and
`rightkit-media-pipeline` (`studio.production.*`, pipelines).

## Host contract

Hosts bundle this package with Vite (the build keeps extensionless relative imports and asset
imports). Tailwind hosts add `node_modules/@rightkit/media-gen-ui/dist/**/*.js` to their content
globs. Masks are painted with `@rightkit/media-ui` `MaskPainter` (`components/MaskEditor`).

Modules left out of the root barrel because an export name collides with an earlier module
(import them by subpath): `lib/engine-error.ts`, `lib/gen-params.ts`, `lib/model-management.ts`, `lib/production-stack-builders.ts`, `lib/types/avatar.ts`, `lib/types/gallery.ts`, `lib/types/hyperframes.ts`, `lib/types/image-studio.ts`, `lib/types/image.ts`, `lib/types/local-image.ts`, `lib/types/video.ts`, `lib/types/workflow.ts`, `views/panels/character-lock.ts`, `views/panels/video-quality.ts`, `views/rentals-logic.ts`, `views/template-swap-logic.ts`
