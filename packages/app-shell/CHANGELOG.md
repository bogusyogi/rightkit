# Changelog

## 0.5.0

Added:
- `keybindings` on `ShellProvider` (and `AppShell`'s `shell` prop) to rebind or disable the palette (`mod+k`), help (`?`) and sidebar (`mod+\`) chords. `false` disables a binding. Overrides the legacy `paletteChord` and `helpChord` props. `ShellContextValue.keybindings` exposes the resolved values; `resolveShellKeybindings` is exported for tests.
- `paletteWidth` on `ShellProvider` and `CommandPalette` (default 640). The palette sets its width inline.
- `className` and `slots` on `ModalShell`, `Dialog`, `ConfirmDialog`, `PromptDialog`, `ContextMenu`, `CommandPalette`, `ToastViewport`, `Titlebar` and `AppShell`. Slot regions are listed in the README.
- Titlebar and caption tokens: `--rk-titlebar-start-pad`, `--rk-titlebar-side-pad-end`, `--rk-titlebar-pad-end`, `--rk-titlebar-gap`, `--rk-caption-width`, `--rk-caption-close-bg`, `--rk-caption-close-ink`, `--rk-plane-radius`, `--rk-toast-offset` and `--rk-palette-width`. Defaults match the previous fixed values.
- Theme store options: `defaultChoice` (default `"system"`), `alwaysPaintDataTheme` (default `true`) and `systemMedia`. The store listens to the OS colour scheme itself and exposes `resolved()` and `dispose()`.
- Peer `@rightkit/platform-ui` `>=0.2.1` (headless exports). Optional peer `@tauri-apps/plugin-os` (`peerDependenciesMeta`).

Changed:
- `system` theme mode keeps `data-theme` set to the resolved `light` or `dark` value instead of removing it. Set `alwaysPaintDataTheme: false` for the previous behaviour. Stored `"system"` values are now honoured, and selecting the default removes the stored key.
- `floatingPanelStyle`, `createToastBus`, `defaultToastBus`, `toast`, `useFocusTrap`, `useModalDismiss` and `useLatest` now live in `@rightkit/platform-ui/headless`. app-shell re-exports them, so existing imports are unchanged.

## 0.4.1

- Republish with `dist/` present. 0.4.0 was published from an unbuilt tree and cannot be imported (exports point at ./dist/index.js); do not pin 0.4.0. `prepack` now builds.

## 0.4.0 — 2026-10-10

- Peer contract: declare `react-dom` (`>=18 <21`, matching the `react` range) as a peer dependency; `@rightkit/app-shell/react` imports it for the context-menu portal. Apps already install it alongside `react`.
- Test-only: the e2e harness imports `@rightkit/qa-chrome` directly; the devDependency moves from the `@rightkit/qa` facade.

## 0.2.1 — 2026-10-07

- Test-only: the real-browser e2e harness launches Chrome for Testing through `@rightkit/qa/chrome` (`RIGHTKIT_CHROME_FOR_TESTING` overrides; skips with install instructions when missing, never regular Chrome) and closes gracefully (CDP `Browser.close`, SIGTERM, SIGKILL last) with its temp profile removed. No runtime change.

## 0.2.0 — 2026-10-07

Breaking:
- Every suite token is now `--rk-*` (`--rk-ink`, `--rk-accent`, `--rk-plane`, ...). shell.css no longer defines unprefixed `:root` variables, so it cannot recolour an app that uses names like `--ink`. Apps that relied on the flat names can opt in with `@rightkit/app-shell/suite-aliases.css`.
- Wordmark font is `--rk-wordmark-font` (was `--rk-wordmark`).

Fixes:
- Primary/danger buttons inside the shell keep their own text colour: the shell's button reset is `:where(.rk-shell) button` (0,0,1), below every `.rk-btn--*` rule.

Added:
- `FuseCurves` works outside `.rk-item` and without shell.css (inline positioning, `fill` and `size` props; `--rk-fuse-fill`).
