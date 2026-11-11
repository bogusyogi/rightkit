# Changelog

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
