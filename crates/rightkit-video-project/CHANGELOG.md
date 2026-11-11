# Changelog

## 0.1.3 (unpublished)

Taken on the recommended option of the shape review (`docs/audits/2026-11-11-shape-matrix/reports/cut-post-redraw.md`), pending the owner's confirmation.

- New, additive (feature `engine-commands`, off by default): `engine`, the project-method layer engine command modules share (JSON and transcript files, analysis layout, clip occurrence ranges, flat settings files, project font, bundled media tools, renders listing, `parse_fps`), and `cmd` (`projects.create`, `slot.fill`, `storage.status`, `storage.clear`). Moved, code unchanged, from `rightkit-engine-host` 0.1.x `project` and `project_cmd`. `engine::SETTINGS_SCHEMA_ID` is `video-project-settings` (was `cutright-settings`); flat settings files carry no schema id, so nothing on disk changes. Adds optional dependencies on `rightkit-engine-host` 0.2.0, `rightkit-transcript`, `rightkit-app-settings` and `rightkit-cache`.
- New, additive (feature `channel-recipe`, off by default): `channel_recipe::validate(body, styles)`, the read-only check of a channel recipe body against the presets a render accepts, moved unchanged from `rightkit-pipeline` 0.1.x (it is a render-preset check, not a generation client). Adds an optional dependency on `rightkit-color`.
- The crate documentation no longer calls it the CutRight project layer.

## 0.1.2 (unreleased)

- Calls `rightkit_ffmpeg::verify_ffmpeg_handle` / `verify_ffprobe_handle` (renamed from `verify_*_or_development`); requires `rightkit-ffmpeg` 0.2.16.
- Removed test-only code: the `RIGHTKIT_FFMPEG`/`RIGHTKIT_FFPROBE` override stub and `MediaTools::resolve_with`; `resolve` always verifies both bundled tools (Adrian, 2026-11-03: "make sure no debug assertions in their respective apps/ repos"; "My decision was no unit tests. Only e2e."; "dev mode, is mine. im the fucking owner, my data. no dummy data ... real data. snapshot and restore."). Release behaviour unchanged.

## 0.1.1 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `cutright-project` (CutRight).
