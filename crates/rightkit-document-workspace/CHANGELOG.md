# Changelog

## 0.2.0

- Breaking for dependents: dependency requirements `rightkit-app-settings` 0.3 -> 0.4. 0.1.4 was published before these requirements were raised; a semver-breaking dependency is a breaking change, so they ship as 0.2.0 instead of changing 0.1.4 in place (#1572). No source change in this crate since 0.1.4.

## 0.1.4 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.3

- Added `SettingsStore::in_app_folder(app_folder, namespace)`: the store opens `settings.json` and `explorer.json` directly in the app's own data folder (rightkit-fs `AppDirs::data`), so a host needs no parent-folder resolver (viewright#57). `new(data_dir, namespace)` is unchanged and opens the same files under `<data_dir>/<namespace>/`. Additive.

## 0.1.2 — 2026-11-03

- Declared `markdown_index` (it shipped in 0.1.1 as a file but was never a `pub mod`, so no host could import it).

## 0.1.1

- Added the viewer command bodies (library, document IO, explorer, markdown resources, settings) behind `PathGrants`, `DocumentBackend` and `WorkspaceWatcher`, returning `WorkspaceError`. Additive.

## 0.1.0 (unreleased)

- First RightKit release, moved from ViewRight (src-tauri persistence.rs, migrations/0001_init.sql).
- `markdown_index::index_markdown_folder` (the body of the `index_markdown_folder` command; consent and path grants stay with the caller) and `WorkspaceError`. Moved from ViewRight (commands/markdown_index.rs).
