# Changelog

## 0.2.1

The owner's decision on 0.2.0's redraw: CutRight's UI belongs in RightKit, not in the app. 0.2.0 deleted it
while CutRight's app is only `<DesktopWorkspace/>` from this package, so CutRight could not upgrade.

- Restored CutRight's application UI as an app-level entry, `@rightkit/video-ui/app` (`src/app/`): the 25
  files of 0.1.x `views/*` (every page and its stylesheet; `FinishedEdit.css` stays merged into
  `editor/Editor.css`), `components/DesktopWorkspace` and `Workspace.css`, and what they need that 0.2.0
  also removed: `appName`, `legal/legalConfig`, `demo/*`, `hooks/useProgress`, `lib/assemble`, `lib/dialog`,
  `lib/genright`, `lib/mic`, `lib/models`, `lib/modelsBackend`, `lib/services`, `lib/settingsBackend`,
  `lib/shell`, `mediaAdapters` and the page UI helpers (`Thumbnail`, `ProgressRow`, `Drawer`, `ErrorPanel`,
  `fileSize`) with their styles. The entry is a superset of the root and carries every runtime name the
  0.1.x root exported except `shell`, which moved to `./tauri`. The editing components are 0.2.0's, unchanged.
- Rewired onto the host seam instead of direct Tauri: the app entry imports no Tauri module. `VideoHost`
  gains an optional `app` part (`VideoAppHost`: `invoke`/`listen` command bridge, `onEngineRaw`, window,
  `onDragDrop`, pickers, `appVersion`, `checkUpdate`, `relaunch`, `quit`, `exit`), read through
  `videoAppHost()`, which throws with the fix when the host has none. The licence and logs clients, demo
  mode, media playback derivatives, pickers, drag and drop, the updater, the legal gate's quit and the
  Settings version read go through it.
- New `@rightkit/video-ui/tauri`: `tauriVideoHost()`, the Tauri `VideoHost` with the `app` part, making the
  native calls 0.1.x made (shell engine call/events/status with the same lifecycle filter and status mapping,
  `playback_*` with `toolsDir: null`, `media_scope_allow`, `authorised_media_source_id`, `reveal_path`,
  `prepare_registered_media_playback`, `cancel_media_playback`, plugin process/updater/dialog, window and
  webview drag and drop). CutRight upgrades with `installVideoHost(tauriVideoHost())` and imports from
  `./app` (and `./app.css`) instead of the root.
- Host-supplied app name and legal facts as before (`setHostAppName`, `configureLegal`); `configureLegal`
  also takes `step` to override the gate's copy and look, which default to 0.1.x's.
- New `./app.css`: the app's stylesheets in 0.1.x order plus `styles.css`.
- Icons: the pages use the shared `Icon` with the editing glyphs plus six app glyphs (`APP_GLYPHS`), not a
  second icon set.
- Dependencies: the app and Tauri entries bring back `@rightkit/app-update`, `legal`, `legal-ui`, `license`,
  `logs`, `onboarding`, `platform-ui`, `settings-ui`, `tauri`, `tauri-shell` and `@tauri-apps/api`,
  `plugin-dialog`, `plugin-process`, `plugin-updater` (0.1.x's set without `@rightkit/theme`, which no
  source imports). The root entry still imports only `media-ui` and `react-shell`.

## 0.2.0

Taken on the recommended option of the shape review (`docs/audits/2026-11-11-shape-matrix/reports/cut-post-redraw.md`), pending the owner's confirmation.

- Breaking: the package is the video editing components over a host the app supplies, not CutRight's frontend. `installVideoHost(host)` installs a `VideoHost` (the video engine call, events and status; the native preview session; asset URLs, media scope, registered media ids, reveal). Nothing imports `@tauri-apps/*`, `@rightkit/tauri` or `@rightkit/tauri-shell`; the `playback_*`, `media_scope_allow`, `reveal_path`, `authorised_media_source_id` and media-adapter invokes are the host's.
- Breaking: removed the app shell, which stays in the app: `views/*` (Home, Settings, Sources, Auto-edit, Shorts, Phrases, Retention, Assets, Exports, Batch delivery, Bundle export, Recipe library, Prompt panel, Progress, Onboarding gate, Timeline view), `components/DesktopWorkspace`, `demo/*`, `legal/legalConfig`, `appName` (`setHostAppName`, `hostAppName`), `lib/genright` (the GenRight UI client; the generation client is `rightkit-media-gen-client`), `lib/services`, `lib/settingsBackend`, `lib/modelsBackend`, `lib/models`, `lib/shell` (theme store, shell bridge, quit), `lib/dialog`, `lib/mic`, `lib/assemble`, `hooks/useProgress`, `editor/index`, and the `ui` helpers only those pages used (`Thumbnail`, `ProgressRow`, `Drawer`, `ErrorPanel`, `fileSize`) with their styles.
- Breaking: the generated export-everything index is replaced by a deliberate list; the `@rightkit-host` aliases and build-switch globals are no longer read.
- Dependencies: only `@rightkit/media-ui` and `@rightkit/react-shell` (was also app-update, legal, legal-ui, license, logs, onboarding, platform-ui, settings-ui, tauri, tauri-shell, theme and four Tauri packages).
- One icon implementation: `Icon` is `@rightkit/react-shell`'s with the editing glyphs it lacks (`EDITING_GLYPHS`), instead of a second icon set; strokes follow the shared icon.
- Progress events read `media_gen.*` (were `genright.*`), as `rightkit-media-gen-client` emits them; window events are `rightkit-video-ui:playback-refreshed` and `rightkit-video-ui:settings-changed` (were `cutright:*`).
- Editor styles fall back to the suite's spacing and radius when the host defines no `--s*`/`--r-*` tokens; `src/editor/NOTICES.md` carries the hyperframes provenance the vendored files point to.

## 0.1.1

- Adds the `--caption-clip` surface token (light and dark) for the caption track clip fill. Fixes CutRight #38 (the token was referenced but defined nowhere).
- Removes the unit test (`src/lib/progress.test.ts`), its vitest config and the `test/host-fixture` fixture, and the `test` script. Adrian's rule: e2e only, no fixtures or dummy data.

## 0.1.0

- First release. Video editor UI: timeline, inspector, preview and project views.
