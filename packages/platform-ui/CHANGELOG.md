# Changelog

## Unreleased

Additive. Every new option defaults to the 0.2.8 behaviour.

- `isTauri()` (root): true inside a Tauri WebView. Pure detection; never loads `plugin-os`.
- `blockNativeContextMenu(doc?)` (root, `./headless`): suppresses the native `contextmenu` in the capture phase; returns the uninstall. Idempotent per document.
- `relativeTimeFrom(ms, now?)` (root, `./headless`): "just now" / "N minutes|hours|days ago", ported from ViewRight. `now` defaults to `Date.now()`.
- `useFocusTrap` / `attachFocusTrap`: `scope: "element" | "document"` (document: capture-phase Tab on the document, so the trap holds while focus is on a backdrop); `skipUnrendered: boolean` (drops controls whose `getClientRects()` is empty).
- `useDismiss` / `attachDismiss`: `isInside(target: Node) => boolean` for a panel spread over several elements; `onFocusOut: boolean` dismisses with the new reason `"focus"` when focus leaves the panel (focusout to an outside target, or a Tab after which focus is outside). The Tab is never prevented.
- `createToastBus({ dedupe, max })`: `dedupe` suppresses an identical plain toast (no action, no `id`) while it is live; `max` dismisses the oldest live toast before a push that would exceed it. A toast is live until `bus.dismiss(id)`; viewports must call it on every removal.
- `toast(message, { id })` and `bus.push(..., { id })`: replace-by-key. A push with a live `id` dismisses that toast first. Keyed and action toasts are never deduplicated. Existing callers compile unchanged.
- Docs: the README states that platform-ui has no stylesheet and no motion handling, and points to `@rightkit/app-shell`'s `data-rk-motion="full"` opt-out.

## 0.2.8

Additive. Defaults equal 0.2.7; nothing changes for an app that calls none of the new options.

- Host-injected platform: `getPlatform()` reads `window.__RIGHTKIT_PLATFORM__` (normalised) before the native load. `loadPlatform()` resolves the native value; when it disagrees with the injected one the native value wins and a warning is logged.
- `platformInitializationScript(platform)` (root, `./headless`): returns the exact initialization-script snippet, built from the normalised platform only.
- `configurePlatform(options)` with `PlatformOptions`: `qaQueryParam` (default `"qaPlatform"`), `qaRequireQaFlag` (default `true`), `nativeWinsOverQa` (default `false`), `fallbackPlatform` (default none, used only when the result would be `unknown`).
- `normalizePlatform` accepts the aliases `mac` and `darwin` (macOS) and `win` (Windows).
- `formatChord` renders the chord token `ctrlormeta` (Ctrl or Meta): `⌘/⌃` on macOS, `Ctrl/Win` on Windows and Linux, `Touch` on iOS, `Ctrl/Meta` when unknown.

## 0.2.7

- `formatChord`: side-specific modifier tokens `lctrl`, `rctrl`, `lalt`, `ralt`, `lshift`, `rshift` are spelled out on every platform (macOS "Left Option", "Right Control"; elsewhere "Left Alt", "Right Ctrl"). Before, a token missing from a platform's table was only capitalised (`lalt` → "Lalt"; on macOS `ralt` → "Ralt"). Display change for any app that already passed those tokens.
- `formatChord(chord, platform, { labels })`: `labels` (by lower-case token) win over the platform table, for an app whose wording differs (e.g. `{ esc: "esc" }`, or Windows labels for an unknown platform).
- Docs: for a platform value that never changes after first read, `await loadPlatform()` before the first `getPlatform()`; until it resolves `getPlatform()` returns the browser's report.

## 0.2.6

- `useExitTransition` / `createExitTransition`: new `closeOn: "original-settled" | "first-settled"` (default `"original-settled"`, unchanged). With `closeAfter: "settled"` and `onActionWhileClosing: "run"`, `"first-settled"` starts the exit when whichever running action settles first, as for a wrapper that attaches its own close to every action. The other actions keep running, start no second exit, and reject only their own promise.

## 0.2.5

Added (default equals 0.2.4):
- `useExitTransition` `dismissWhilePending: "ignore" | "exit"` for `closeAfter: "settled"`. With `"exit"` a plain `close()` while an action is pending starts the exit at once; the action keeps running and its settling starts no second exit and no second `onClosed`. A later rejection goes only to the promise returned by the original `close(action)`.


## 0.2.4

Two optional edge cases for ViewRight parity. Defaults equal 0.2.3.

Added:
- `useListNavigation` with `emptyIndex: "preserve"`: on an empty list the stored index is what the clamp rule computes instead of a forced -1. ArrowDown and End store -1, ArrowUp and Home store 0 (so the first row is selected when results arrive), a wrapping list stores -1. `moveIndex` takes a sixth argument `preserveEmpty` for this. `"clamp"` (default) still stores -1.
- `useExitTransition` `onSyncThrow: "close" | "propagate"` (default `"close"`, as before): `"propagate"` rethrows a synchronous throw from the action to the caller of `close()`, starts nothing and leaves state unchanged, for both `closeAfter` values (the action then runs before `onExitStart`). Asynchronous rejections behave as before in both modes.

Changed internally: `createExitTransition().close` is no longer an `async` function (it still returns a promise); with the default option a synchronous throw is still returned as a rejected promise.

## 0.2.3

Optional parity options for ViewRight's three families. Every default equals 0.2.2 except the one fix below.

Fixed:
- `useFocusTrap` with `captureOpener: "effect"`: under React StrictMode the second effect run recorded the trap's own first control as the opener, so closing in development sent focus to a node about to be removed. The opener is now recorded once per open (never from inside the container, never over an existing value) and cleared when the trap really closes. `"render"` capture is unchanged.

Added:
- `useFocusTrap` `restoreWhen: "inactive" | "always"` (default `"inactive"`): `"always"` restores focus on every effect cleanup.
- `useListNavigation` `imeGuard` (default true), `emptyIndex: "clamp" | "preserve"` (default `"clamp"`; `"preserve"` keeps the -1 that ArrowDown leaves on an empty list, so nothing is selected when results arrive), `scrollOn: "active" | "active-or-length"`, and `rawIndex` in the result. Exported `readIndex`. ArrowUp with nothing selected now goes to the first item (the last when wrapping); this can only be reached with `emptyIndex: "preserve"`.
- `useExitTransition` `onActionWhileClosing: "drop" | "run"` (default `"drop"`): `"run"` runs an action passed to `close` while closing or pending. A double activation then executes it twice; for parity only.

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
