# Changelog

## 0.2.5

Update-stable styling seams; apps no longer need to reset package defaults.

- `styles.css`: every rule is now wrapped in `:where(...)` (zero specificity), so any app
  rule wins regardless of order. Includes `.rk-media-fallback` (`flex-direction: column`)
  and the narrow-screen audio rules.
- The `speedControl="menu"` `<select>` now has its own class `rk-media-speed-menu` and no
  longer carries `rk-media-btn` / `rk-media-speed` (so `min-width: 30px`, `min-height: 30px`,
  `min-width: 34px` and `font-size: 11px` no longer apply to it). Defaults: `font: inherit`,
  `color: inherit`, no border/background, 6px radius, pointer cursor, `0 4px` padding.
  `rk-media-speed` remains the cycle button.
- Visible default changes (only for apps that override nothing): the speed select is
  content-height instead of a 30px minimum, and its text inherits the controls font size
  (12px) instead of 11px. Everything else renders as in 0.2.4.
- README styling contract lists the media-player classes.

## 0.2.4 — 2026-10-07

- `adapters.icons` overrides image-viewer glyphs, starting with the sibling pagers (`caretLeft` / `caretRight`); same semantics as MediaPlayer `icons` (`null` hides).

## 0.2.3

ViewRight parity for image text fonts and the text panel; all additive, defaults unchanged.

- Font fallback split. `fallbackFontStack` / `ImageFontOptions.fallbackStack` now applies only
  when there is no font at all (empty family resolving to no bundled font). New
  `systemFontFallbackStack` / `ImageFontOptions.systemFallbackStack` (default
  `system-ui, sans-serif`) follows a named system font: `"X", system-ui, sans-serif`. Used by
  the display overlay, text-panel preview, export and print. With
  `fallbackFontStack: "'Inter', system-ui, sans-serif"` output equals ViewRight's
  `cssFontFamilyForImageText`.
- New adapters `bundledFontsLabel` (select title, default "Bundled fonts") and
  `bundledFontsEmptyOption` (empty option, default "Bundled").
- Text color control: "color" text span is `aria-hidden`; the swatch gains the diagonal-slash
  overlay (`.rk-iv-swatch-slash`) as in ViewRight.
- Bundled-font select has class `rk-iv-font-bundled`, width token
  `--rk-iv-font-bundled-width` (default 144px).

## 0.2.2

Image viewer defaults now match ViewRight; behaviours 0.2.1 added beyond it are opt-in.

- `rovingFocus` (default `false`): arrow-key roving focus is off by default, so every
  capsule control is its own Tab stop as in ViewRight. Set `true` for the 0.2.1 single tab
  stop with Left/Right/Home/End. `role="toolbar"` and `toolbarLabel` apply either way.
- Buttons now activate on mousedown for any mouse button (as ViewRight's TBtn); click still
  runs only when `event.detail === 0`.
- Fonts: `cssFontFamilyForImageText` / `fontCssForImageText` / `exportImageEdit` take an
  optional options object `{ defaultFontId, fallbackStack }`; an empty font family resolves
  through `defaultFontId`. New adapter `fallbackFontStack` (default
  `"system-ui, sans-serif"`). Adapters `defaultFontId` and `fallbackFontStack` are applied to
  on-screen text, export and print alike. A host passing
  `fallbackFontStack: "'Inter', system-ui, sans-serif"` reproduces ViewRight exactly.
- Save and print stay registered against the export source while a sibling decode is
  in flight (ViewRight behaviour). `blockExportDuringSwap` (default `false`) enables the
  0.2.1 guard that unregisters save and ignores print until the new image has decoded.

## 0.2.1

Image viewer, all additive; 0.2.0 APIs unchanged.

- `toolbarContainer` now portals only the capsule row (`.rk-iv-bar`); tool panels stay
  inside the viewer above the stage (0.2.0 moved the panels too). New optional
  `panelsContainer` portals the panels separately. Portalled parts keep their default
  styling outside `.rk-iv`.
- Capsule is `role="toolbar"` with `toolbarLabel` (default "Image tools") and arrow-key
  roving focus (Left/Right/Home/End, single tab stop, disabled controls skipped).
- Buttons act on mousedown (primary button, `preventDefault`, focus unchanged) and on click
  only when `event.detail === 0`, matching ViewRight's TBtn. Buttons gain a
  `:focus-visible` ring.
- Public styling contract documented in the README; new tokens for capsule, button, input,
  badge and sibling-arrow styling (defaults identical to 0.2.0).
- Dev: jsdom + @testing-library/react devDependencies for toolbar tests.

## 0.2.0

Breaking: the image viewer moved out of the root entry.

- `ImageViewer`, `ImageEditor`, `ImageViewerProps`, `ImageTool` are now exported
  from `@rightkit/media-ui/image`. The root entry no longer imports `fabric`.
- `fabric` is now an optional peer dependency (was a dependency), so player-only
  apps never install it or approve the `canvas` native build. Image apps add
  `fabric` and set pnpm `allowBuilds: { canvas: false }`.
- `react-dom` is a peer dependency (toolbar portal).
- ImageViewer: `toolbarContainer` prop portals the toolbar into a host element.
- Adapters: `defaultFontId`; `acquireResource` may return `{ release, update }`
  (plain release functions still work); `ResourceRequest` gains `dirty` and
  `stateExternalized`, pushed through `update` instead of re-acquiring.
- MediaPlayer: `showArtwork`, `showOpenOriginal`, `icons`, `speeds`,
  `speedControl` ("cycle" | "menu"), `labels`, `renderError`; exports
  `IconName`, `MediaPlayerLabels`, `DEFAULT_MEDIA_PLAYER_LABELS`.
- Text overlay now syncs layer `text` on update; active toolbar tokens
  `--rk-iv-active-bg` / `--rk-iv-active-fg`; styles derive private `--_rk*`
  variables from the public `--rk-media-*` / `--rk-iv-*` inputs.
- Includes the 0.1.1 fix (never published): restore the media `src` after a
  StrictMode effect remount.

## 0.1.0

Initial release.
