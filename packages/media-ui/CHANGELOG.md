# Changelog

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
