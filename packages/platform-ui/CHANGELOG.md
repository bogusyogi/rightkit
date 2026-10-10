# Changelog

## 0.2.1

Added:
- `@rightkit/platform-ui/headless` entry: `formatChord`, `normalizePlatform`, `floatingPanelStyle`, `createToastBus`, `defaultToastBus`, `toast`, `useFocusTrap`, `useModalDismiss`, `useLatest`. It imports nothing from `@tauri-apps/plugin-os`.
- `loadPlatform()` resolves the native platform with a dynamic import of `@tauri-apps/plugin-os`. It falls back to the browser's report when the plugin is absent or outside Tauri.

Changed:
- `@tauri-apps/plugin-os` is no longer imported at module load. `getPlatform()` stays synchronous; it starts the native load in the background and returns the browser's report until the native value arrives. `usePlatform()` updates when the native value loads.
- `@tauri-apps/plugin-os` is an optional peer (`peerDependenciesMeta`).

Fixed:
- The native-load fallback also covers a `platform()` call that throws outside Tauri.
- `prepack` runs the build, so `pnpm pack` and publish always ship a fresh `dist`, as `@rightkit/app-shell` does.

Unchanged: every existing export, its signature and its results. `formatChord` from the root entry still defaults to the detected platform.

## 0.2.0

- Initial published surface: platform detection, chord formatting, window-control adapters, QA platform override.
