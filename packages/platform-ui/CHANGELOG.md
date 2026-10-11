# Changelog

## 0.2.2

Headless behaviour hooks for apps whose markup is fixed. Everything is added to `@rightkit/platform-ui/headless`; nothing is imported from `@tauri-apps/plugin-os`. See the README section "Headless behaviour hooks" for the API table and a worked menu, modal and palette example.

Added:
- `useFocusTrap` options (ledger #89): `trap`, `restore`, `restoreTiming` (`"microtask"` | `"sync"` | ms), `captureOpener` (`"render"` | `"effect"`), `fromOutside` (`"both"` | `"backward"` | `"none"`), `skipHidden`, `emptyList` (`"prevent"` | `"pass"`), `focusFallback` (`"container"` | `"none"`). Exported type `FocusTrapOptions`; `attachFocusTrap` is the framework-free core.
- `useModalDismiss` options `preventDefault` and `stopPropagation` (both default true).
- `useDismiss(onDismiss, options)`: Escape (window capture or an element), outside press (`"mousedown"` | `"click"` | `"click-target"`, against a `container` or a `backdrop`), outside context menu, window blur, resize and scroll flags, IME guard. Core: `attachDismiss`.
- `useExitTransition(options)`: `open` / `closing` / `closed` with `exitMs`, safe re-open during the exit, `close(action)` with `closeAfter: "immediately" | "settled"`, timers cleared on unmount. Core: `createExitTransition`.
- `useListNavigation(options)`: active index with `wrap` or clamp, Home/End, `keepIndexOnItemsChange`, `resetKey`, `hoverSelects` (`"always"` | `"after-pointer-move"`), optional typeahead, disabled items, `idPrefix` / `listId` / `optionId`, returns input, list and option props. Pure helpers `clampIndex`, `moveIndex`, `selectByPointer`, `typeaheadMatch`.
- `fitToViewport({ x, y, size | estimatedSize, viewport, margin })`, a pure placement for a measured or estimated size, beside `floatingPanelStyle`.
- `rankCommands`, `rankItems`, `matchScore` (moved from `@rightkit/app-shell` 0.6.0 without a change in behaviour; app-shell 0.6.1 re-exports them).

Unchanged: every 0.2.1 export, signature and default. `useFocusTrap`, `useModalDismiss` and `floatingPanelStyle` behave as in 0.2.1 when called with the arguments they accepted then.

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
