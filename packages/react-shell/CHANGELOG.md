# Changelog

## 0.7.0 (2026-10-17)

Additive, shipped in 0.7.0. Defaults render the same markup and CSS values as 0.6.8, except where noted.

- `ErrorBoundary resetKey`; `ShortcutHelp slots` and `renderGroup`; `ContextMenu anchor` and `placement` (`x`/`y` now optional); new `Popover` (anchored panel, dismissal ignores its opener); `FuseCurves side="left"` (mirrored two-corner form).
- `AppearanceMenu presets`, `preset`, `onPresetChange`, `fonts`, `font`, `onFontChange` (swatches and font faces in the menu); `applyBodyFont` sets `--rk-font-body`, consumed by `.rk-shell`. Tokens `--rk-wash-hover`, `--rk-wash-press` (hover and press washes for item, seg, pill, toggle and checkbox) and `--rk-line-2` (hairline).
- `installHoverGuard()` sets `data-rk-pointer` so the washes apply only while the pointer is over the window (WebView2 stale `:hover`).
- `createThemeStore`, `applyStoredThemeSync` and `themeBootScript` take `darkRemovesAttribute` (dark removes `data-theme`).
- `ModalShell`/`Dialog`/`ActionDialog`: `escape={false}` leaves Escape to the content; `dismissible` (boolean or function) vetoes Escape and backdrop dismissal before the exit animation starts. `PromptDialog secret` masks the value.
- `ToastViewport`: `placement="top-end"` (`--rk-toast-top-end-offset`, `--rk-toast-end-offset`), `dedupe="newest"`, `maxVisible` (default 4). `ShellProvider toastOptions` passes both.
- `CommandPalette` controlled props (`query`, `onQueryChange`, `activeIndex`, `onActiveChange`, `onRun`), `groupOrder`, `renderRow`, `scopeLabel`; `Command.kind` (type chip) and `Command.sublabel`; PageUp, PageDown, Home and End move the highlight.
- `Card onClick`, `label`, `flush`, `selected`; `Skeleton`, `SkeletonRows`; `Checkbox`, `Select`, `Slider`, `Notice`, `ProgressBar`.
- `ShortcutRecorder` (records a chord with `chordFromEvent`); `usePersistedState`.
- `createShortcutRegistry`: pseudo-keys `digit` and `arrow` (for example `mod+digit`); Alt and AltGr chords use the physical key for punctuation too; `conflicts()` returns `kind: "exact" | "prefix"` (a one-press chord that starts a sequence); `list()` returns `unassigned`; `suspend()`, `resume()`, `isSuspended()`.
- `AppShell sidebarResizable` (drag and arrow-key handle, optional `storageKey`) and `onSidebarWidthChange`; `ShellLabels.resizeSidebar`.
- `NavList`: `itemProps` (drop-target handlers, classes), `itemCheck` (checkbox rows), `collapsedGroups`, `onToggleGroup`, `renderTwisty` (collapsible groups); `NavItem.ariaLabel`.

- Fix (0.6.6): `CommandPalette` runs the chosen command once. A second Enter (or click) during the exit animation used to schedule the command again; now the palette ignores Enter, arrow keys and clicks once a command has committed or while it is closing, until it is reopened. Apps that had a once-per-open guard in their adapter can drop it. `e2e/options.e2e.test.ts` pins it in Chrome.
- Additive (0.6.5): `applyStoredThemeSync(options)` applies the stored theme synchronously before first render, and `themeBootScript(options)` returns an inline `<head>` script doing the same before any bundle loads (needs a CSP hash or nonce). Both use the theme store's storage key, choices, system resolution and `data-theme` rules and never throw. `createThemeStore`'s apply now skips writing a `data-theme` the root already has (when the root offers `getAttribute`), so the provider does not flip a boot-applied theme. Fixes the light flash on restart with a saved dark theme.
- Fix (0.6.5): after a `data-theme` switch, WebKit (WKWebView) could leave shell text that only inherited `color` stale, for example the titlebar title staying the light ink while the root and plane were already dark. `shell.visual.css` / `shell.css` now give the text elements (`.rk-top`, `.rk-top__title`, `.rk-side`, `.rk-plane`, `.rk-nav`, `.rk-item__label`, `.rk-brand-foot`, the page band, scroll and heading, `.rk-field label`, modal title and label, palette label, toast message) an explicit `color: var(--rk-ink)` in one `:where()` rule, so a custom-property change restyles them directly. Zero specificity: a host rule or any shell rule on the same element still wins. No JavaScript change; apps whose own headers inherit ink should set `color: var(--rk-ink)` on them too. `e2e/cascade.e2e.test.ts` asserts the computed colours after a switch (Chrome; the harness has no WebKit project).
- Renamed from @rightkit/app-shell; @rightkit/app-shell@0.6.2 remains published under the old name.
- Shortcut labels take the platform from `ShellProvider`. `ContextMenu` drew its `Kbd` for the host browser even inside a provider given another `platform`; `JumpTo`, `PaletteTrigger`, `CommandPalette` (without a registry), `Titlebar`, `AppFrame` and the `AppShell` layout fell back to host detection the same way when not given their own `platform` prop. Order is now: the component's `platform` prop, then the provider's platform, then host detection (the provider's own default, and the fallback outside a provider).

## 0.6.8

Additive (#291). Without the new props the markup, classes and CSS values are those of 0.6.7.

- Toast placement: `ToastViewport placement` and `ShellProvider toastPlacement`, `"bottom-center"` (default) or `"top-center"`. `top-center` adds `data-placement="top-center"` to the live region. New type `ToastPlacement`.
- Token `--rk-toast-top-offset` (default `16px`) is the top-center distance from the top edge; `--rk-toast-offset` still moves only the bottom edge. Top-center rows enter from above (`rk-toast-in-top`). `shell.structure.css`, `shell.visual.css` and the generated `shell.css` carry the rules.
- CutRight can drop its `.rk-toasts` override: `toastPlacement="top-center"` plus `--rk-toast-top-offset: calc(var(--rk-titlebar-height) + 12px)`.

## 0.6.7

Additive (#174). Defaults render the same markup and CSS values as 0.6.6.

- `Titlebar` options: `captionButtonClasses` (extra class per caption button), `captionTitles` (tooltip per button, or `false`; `aria-label` keeps the label), `blurCaptionOnClick` (default true; false keeps focus on the button), and `as` (root element type, default `"header"`). `CaptionControls` takes `buttonClasses`, `titles` and `blurOnClick`. New types `CaptionButtonClasses`, `CaptionTitles`; `blurThen` is now exported.
- Tokens `--rk-caption-cursor` (default `default`), `--rk-caption-outline` (default `2px solid var(--rk-focus)`) and `--rk-caption-outline-offset` (default `-2px`) on `.rk-caption__btn`. `shell.visual.css` and the generated `shell.css` carry them.
- No `color-scheme` opt-out was added: no shell stylesheet or component sets `color-scheme` in this version, so there is nothing to opt out of.

## 0.6.4

Additive. Defaults equal 0.6.3; nothing changes for an app that sets none of the new options. Peer range raised (see below).

- New entry `@rightkit/app-shell/headless` (imports only `react`): `useToggle({ checked, onChange, label, disabled })` returns switch props (`role`, `aria-checked`, `aria-label`, `aria-disabled`, `tabIndex`, `onClick`, `onKeyDown` for Space and Enter). `useSegmentedControl({ value, options, onChange, label?, disabled?, mode? })` returns `groupProps` and `getOptionProps(value)`. `mode: "radio"` (default) is a radiogroup with roving tabindex, Arrow, Home and End keys and per-option `disabled`; `mode: "toggle"` is a set of `aria-pressed` buttons with Tab between them. No DOM, class or style.
- `Toggle` and `SegmentedControl`: optional `className` merged after the built-in class.
- `shell.css` / `shell.visual.css`: custom properties `--rk-toggle-width` (36px), `--rk-toggle-height` (22px), `--rk-toggle-knob` (18px), `--rk-toggle-travel` (14px) and `--rk-seg-gap` (2px), each defaulting to the previous fixed value. `shell.css` is regenerated-identical with the two source files.
- Peer `@rightkit/platform-ui` raised from `>=0.2.2 <1` to `>=0.2.8 <1`: the `ctrlormeta` chord (added in 0.6.3) is formatted by platform-ui 0.2.8. This supersedes the 0.6.3 note that the peer range was unchanged.

## 0.6.3

Additive. Defaults equal 0.6.2; a shortcut that sets none of the new options behaves as before.

- `createShortcutRegistry` / `ShortcutDef`: `allowComposing` and `allowDefaultPrevented` (both default `false`) let one shortcut fire during IME composition or after another handler prevented the event. Without an opt-in, nothing changes; the composing and prevented early return is kept for registries where no shortcut opts in.
- `ignoreExtraModifiers` (default `false`): the shortcut matches when modifiers its chord does not name are held. Named modifiers must still be held.
- Chord modifier `ctrlormeta` (also `CtrlOrMeta`): exactly one of Ctrl or Meta, on every platform (`ignoreExtraModifiers` also accepts both). It is distinct from `mod` in conflict detection. Display needs `@rightkit/platform-ui` 0.2.8 or later; the peer range is unchanged.

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
