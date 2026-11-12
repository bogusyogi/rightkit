# Changelog

## Unreleased

- `env::AppRoot` is re-exported from `rightkit_roots` (new dependency on `rightkit-roots`); it is the same type `rightkit_daemon::AppRoot` named.

## 0.2.0 (unpublished)

Taken on the recommended option of the shape review (`docs/audits/2026-11-11-shape-matrix/reports/cut-post-redraw.md`), pending the owner's confirmation.

- Breaking: `project` and `project_cmd` (feature `project-cmd`) moved, code unchanged, to `rightkit-video-project` 0.1.3 as `engine` and `cmd` (feature `engine-commands`): they are the video project's engine layer, and engines that are not video editors no longer carry `rightkit-video-project`, `rightkit-timeline`, `rightkit-transcript`, `rightkit-app-settings`, `rightkit-fs` or `rightkit-cache` behind a feature. The `project-cmd` and `test-mode` features are gone.
- Breaking: `doctor` deleted. Its report repeated `rightkit_doctor::report` and its `capabilities` repeated `rightkit_doctor::report::capability_list`; `rightkit-doctor` is the one doctor report. Its `resource_dir` was a second resolution rule beside `env::Env::resource_dir`; the one rule is the new `env::resource_dir(os, exe)`, which `Env::resource_dir` and `rightkit-doctor` call.
- New, additive: `call`, the caller side of the engine CLI this crate serves: run an installed engine by absolute path with `--input` on the command line or through stdin past `call::MAX_INPUT_ARG_BYTES`, a null stdin otherwise, bounded output capture and a deadline, parse the JSON result or the `{"error":{code,message,details?}}` envelope into `EngineError`, and the contract-version and capability handshake against the engine's `doctor` (`call::handshake`). Moved from `rightkit-pipeline`'s `ProcessRunner` and `GenRightClient::check_capabilities`, so every engine-to-engine caller uses one implementation.
- Compared with HeardRight's engine supervision and CodeRight's daemon before the shape was fixed (see the report): the framework is the engine side of the command-engine contract (CutRight, GenRight, ScrapeRight, PostRight, MailRight). HeardRight's engine speaks typed frames over `rightkit-job-queue`'s envelope with an explicit method allowlist and has no command registry; it does not need this crate. The crate is not on the mandatory set and stays off it.

## 0.1.2 (unpublished)

- `project::shorts_path` is public, like `transcript_path`, `envelope_path` and `timemap_path`.
- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.
- Removed the last `#[cfg(test)]` unit test in `data_folder.rs`.
- `data_folder`: removed `require_isolated`, `default_refusal` and `DataFolderRefusal` (Adrian, 2026-11-03: "dev mode, is mine ... real data. snapshot and restore."). The engine uses the app's real data folder; QA protects it by snapshot and restore. `qa_marked` stays for `doctor`.

## 0.1.1 (unreleased)

- New, additive: `env::Env`, the engine's process environment (operating system, executable, app data root, home) with `resource_dir()`; and `apps`, installed-app discovery for an `apps` command (`AppEntry`, `discover_installed`, `discover_with` with an injectable `Host`, `apps_report`), moved from CutRight's `cutright-engine`. Adds dependencies on `rightkit-daemon` and the `doctor` feature of `rightkit-job-queue`.
- New, additive (feature `project-cmd`, off by default): `project`, the support layer for engine command modules built on `rightkit-video-project`: `read_json`/`write_json`, transcript and analysis-folder helpers, `ClipRange`/`clip_ranges`, flat settings files (`open_settings`, `settings_object`, `settings_path`), `project_font`, `media_tools`, `title_font` and `renders_list`. `RIGHTKIT_FONT` is honoured with the new `test-mode` feature.
- New, additive (feature `project-cmd`): `project_cmd`, engine handlers for project housekeeping moved from CutRight's `cutright-engine`: `projects::m_projects_create` (`projects.create`), `slot::m_slot_fill` (`slot.fill`) and `storage::{m_storage_status, m_storage_clear}` (`storage.status`, `storage.clear`; the default project folder is `<Movies|Videos>/<product folder>`, the host names the folder). `project::parse_fps` joins the support layer. Adds `rightkit-cache`.

## 0.1.0 (unreleased)

- First RightKit release, built from the engine framework pieces of GenRight (`genright-engine`, `genright-protocol`).
- `doctor`: the engine doctor report (identity, contract version, capabilities, platform, install location, data-folder readiness, host readiness merged in), from `genright-engine` `main_doctor.rs`.
