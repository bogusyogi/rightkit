# @rightkit/platform-ui

Brand-neutral platform primitives for Right Suite desktop apps: Tauri-backed platform detection, display-only shortcut formatting, keyboard-chord rendering, platform capability flags, window-control labels and headless window-control adapters. Title bars, colours and navigation stay in each app.

| Item | Value |
|---|---|
| Where it runs | App webview UI (React and plain TypeScript) |
| Rust pair | None |
| Version | see INDEX.md |
| License | MIT OR Apache-2.0 |

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/platform-ui` | `getPlatform`, `normalizePlatform`, `formatChord`, `platformCapabilities`, `createWindowControls`, `createFullscreenControls`, `createWindowControlLabels`, `DEFAULT_WINDOW_CONTROL_LABELS`, `WINDOW_CONTROL_LABELS`, `bootstrapQaPlatform`, `resolveQaPlatform`; types `Platform`, `WindowControlAdapter`, `FullscreenControlAdapter`, `WindowControlLabels`, `QaPlatformBootstrapInput` |
| `@rightkit/platform-ui/react` | `usePlatform`, `Kbd`, `KbdProps` |
| `@rightkit/platform-ui/headless` | No native detection and no `@tauri-apps/plugin-os` import, so it works in any app. `formatChord` (explicit platform, default `unknown`), `normalizePlatform`, `floatingPanelStyle`, `createToastBus`, `defaultToastBus`, `toast`, `useFocusTrap`, `useModalDismiss`, `useLatest` |

## Peer dependencies

| Peer | Range |
|---|---|
| `@tauri-apps/plugin-os` | `>=2 <3`, optional (`peerDependenciesMeta`). Loaded lazily; the browser's platform report is used when it is absent. |
| `react` | `>=18 <20` |

## Example

Illustrative. `nativeWindowAdapter` is the app's own Tauri window adapter.

```ts
import { createWindowControls, formatChord, getPlatform } from "@rightkit/platform-ui";
import { Kbd, usePlatform } from "@rightkit/platform-ui/react";

const platform = getPlatform();
const keys = formatChord("mod+shift+p", platform); // display tokens only
const controls = createWindowControls(nativeWindowAdapter); // your Tauri window adapter
```

`formatChord` only returns display tokens. The app owns the stored shortcut string. Fullscreen is a separate adapter, so an app can implement only minimize, maximize and close.

## Native platform loading

`getPlatform()` is synchronous. It returns the QA override if set, then the native platform once it has loaded, and before that the browser's report. Every supported desktop webview reports the right OS there, so the first render is correct. `loadPlatform()` loads `@tauri-apps/plugin-os` with a dynamic import and resolves the native value, which is cached. `usePlatform()` calls it for you.

Bundling note: the root and `./react` entries contain a dynamic `import("@tauri-apps/plugin-os")`. Apps that bundle those entries must install the plugin, which every Tauri app already does. An app without the plugin should import `./headless` only.

## Known issues

- `bootstrapQaPlatform` and `resolveQaPlatform` are QA helpers exported from the runtime entry. Call them only in QA builds. Browser overrides need `qa=1` and `qaPlatform`, or `RIGHTKIT_QA=1` and `RIGHTKIT_QA_PLATFORM`. Production detection uses `@tauri-apps/plugin-os`.
- Peer React is `<20`, while `doc-render`, `i18n`, `media-ui` and `pdf-ui` accept `<21` (npm audit 2026-10-08).
