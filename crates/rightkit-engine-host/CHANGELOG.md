# Changelog

## 0.1.2 (unpublished)

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
