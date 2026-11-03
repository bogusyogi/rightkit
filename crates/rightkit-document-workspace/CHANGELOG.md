# Changelog

## 0.1.3

- Added `SettingsStore::in_app_folder(app_folder, namespace)`: the store opens `settings.json` and `explorer.json` directly in the app's own data folder (rightkit-fs `AppDirs::data`), so a host needs no parent-folder resolver (viewright#57). `new(data_dir, namespace)` is unchanged and opens the same files under `<data_dir>/<namespace>/`. Additive.

## 0.1.2 — 2026-11-03

- Declared `markdown_index` (it shipped in 0.1.1 as a file but was never a `pub mod`, so no host could import it).

## 0.1.1

- Added the viewer command bodies (library, document IO, explorer, markdown resources, settings) behind `PathGrants`, `DocumentBackend` and `WorkspaceWatcher`, returning `WorkspaceError`. Additive.

## 0.1.0 (unreleased)

- First RightKit release, moved from ViewRight (src-tauri persistence.rs, migrations/0001_init.sql).
- `markdown_index::index_markdown_folder` (the body of the `index_markdown_folder` command; consent and path grants stay with the caller) and `WorkspaceError`. Moved from ViewRight (commands/markdown_index.rs).
