# Changelog

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
