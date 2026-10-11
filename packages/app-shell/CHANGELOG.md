# Changelog

## 0.6.2

Added:
- `data-rk-motion="full"` (on the root, an ancestor or the element) opts the shell out of its `prefers-reduced-motion` handling. The default is unchanged: the reduce rule still applies everywhere the attribute is absent or has any other value, and the rule's specificity is unchanged (`:where()`). Documented in the README; the choice to opt out is the host's. `src/css.test.ts` pins the rule and `e2e/motion.e2e.test.ts` checks it in Chrome with the media feature emulated.

## 0.6.1

Stylesheet defects only. No component, prop or export is added or removed; `rankCommands`, `matchScore`, `rankItems` and `Rankable` are now re-exported from `@rightkit/platform-ui/headless` (0.2.2), where the implementation lives, with the same behaviour. The `@rightkit/platform-ui` peer range is now `>=0.2.2 <1` for that reason.

Fixed:
- `.rk-toasts` centred itself with `transform: translateX(-50%)`, which stacks with Tailwind 4's `-translate-x-1/2` (the `translate` property) and moved the stack half a width too far. It now uses `translate: var(--rk-toast-translate, -50% 0)`: same rendered position, no `transform`, and a host can set `--rk-toast-translate` (`0 0`, `none`) to take over. Pinned by `e2e/default-output.e2e.test.ts` (position equals the 0.5.1 transform centring; a host `translate` utility does not shift it).
- Rules that restyle a surface or control a host usually owns, and are not needed for the frame to function, moved from `shell.structure.css` to `shell.visual.css`: `.rk-modal { overflow: auto }`, `.rk-palette { overflow: hidden }`, `.rk-btn { display: inline-flex; align-items: center; justify-content: center }`, `.rk-btn:disabled { pointer-events: none }`. A structure-only host no longer gets them, and `shell.css` (structure then visual) computes the same styles; `src/css.test.ts` checks that no moved property is also set by another visual rule for the same class.

Unchanged: every default for `shell.css` users. The computed style and box of every element in the shell, palette, confirm dialog, menu and toast states (light and dark) are identical to 0.6.0, with one exception: the toast stack's own `transform` and `translate` (see above). `e2e/default-output.e2e.test.ts` leaves those two properties out of the stack's hash and pins its position separately.

## 0.6.0

Everything new is optional. With no new prop or import, DOM, attributes, classes, computed styles and timing match 0.5.1; this is pinned by `src/react/default-overlays.test.tsx`, `src/react/default-markup.test.tsx` and `e2e/default-output.e2e.test.ts` (computed style and box of every element in the shell, palette, confirm dialog, menu and toast states, light and dark).

Added:
- Stylesheets: `shell.layered.css` (the whole stylesheet inside `@layer rk`; the host declares the layer order, for Tailwind 4 `@layer theme, base, rk, components, utilities;`), `shell.structure.css` (position, z-index, layout, overflow, pointer-events, layout variables) and `shell.visual.css` (colour, type, radius, spacing, shadows, motion). `shell.css` stays unlayered and self-contained; it is now generated from the two (`pnpm run build:css`, `pnpm run check:css`).
- `ContextMenu`: `itemAs` (`"button"` renders `button[role=menuitem][disabled]`), `renderItem`, `showLead`, `scrimClassName`, `slots.scrim` and `slots.lead`, `autoFocus` (false keeps focus and selection in the page and drives the menu from the window), `initialHighlight` (`"none"`), `exitMs`, `exitClassName`, `onExitStart`. `useContextMenu` takes these as a third argument.
- `ModalShell` and `Dialog`: `overlayClassName`, `overlayStyle`, `slots.overlay`, `dismissOn` (`"click"`), `ariaLabel` (replaces `aria-labelledby`).
- `ActionDialog` and `DialogAction`: a titled dialog with any number of actions (label, `onClick`, `variant`, `className`, `autoFocus`, `disabled`) and an explicit `role`. `ConfirmDialog` is built on it and takes `role`.
- `CommandPalette`: `labels.inputAriaLabel`, `enterDelayMs`, `rank` and the exported `rankCommands`, `searchIcon`, `renderIcon`, `slots.icon`.
- `ToastViewport`: `renderGlyph`, `renderRow` (`ToastRowContext`), `rowClassName`, `exitClassName`, `exitMs`, `roles` (`ToastRoles`), `slots.glyph`; CSS variable `--rk-toast-width`.
- `ContextMenu` `keyboard="escape-only"`; IME guard on the window key handler; re-opening during an exit animation is a new menu (`useContextMenu` remounts per open); `ToastViewport` timers survive `exitMs` changes; `dismissOn="click"` guards both drag directions.
- CSS: `.rk-menu.is-closing` and `.rk-menu-scrim.is-closing` (used only when `exitMs > 0`).

Changed:
- The toast stack width rule reads `min(var(--rk-toast-width, 26rem), calc(100vw - 2rem))`; the default width is unchanged.
- The overlay button reset `:where(.rk-overlay, .rk-menu, .rk-toasts) button { font: inherit; color: inherit }` now lives in `shell.visual.css`, so it is inside the `rk` layer when the layered entry is used. In `shell.css` it behaves as before.
- Browser tests: `e2e/default-output.e2e.test.ts`, `e2e/options.e2e.test.ts` and `e2e/cascade.e2e.test.ts`; `src/css.test.ts` proves the split is lossless.

## 0.5.1

Added:
- Titlebar tokens: `--rk-top-bg` (bar background, default `--rk-chrome`), `--rk-top-border` (bottom border, default `none`, counted inside `--rk-titlebar-height`), `--rk-caption-hover-bg` (default `--rk-fill-2`), `--rk-caption-margin-start` (default `6px`), `--rk-caption-margin-end` (default `calc(-1 * var(--rk-titlebar-pad-end))`) and `--rk-top-end-gap` (default `--rk-titlebar-gap`). Derived defaults stay in the rules, not on `:root`, so dark mode still follows the surface.
- `captionGlyphs` on `Titlebar` (and `CaptionControls`): `{ minimize, maximize, restore, close }` ReactNodes replace the built-in SVGs. A glyph left out keeps its SVG.
- `maximized` on `Titlebar`: shows `captionGlyphs.restore` on the maximize button (falls back to `captionGlyphs.maximize`, then the square). The window adapter does not report window state, so the app passes it.
- `CaptionGlyphs` type export.

Changed:
- The end content of `Titlebar` is wrapped in `div.rk-top__end` (with `data-tauri-drag-region`) when `end` is given. It carries the `slots.end` class, which was previously accepted but never applied. Titlebars without `end` render the same markup as 0.5.0. With `end`, the wrapper is the only markup change; the default layout is pixel-identical.
- `slots.end` is now applied (see above).

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
