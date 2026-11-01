# Changelog

## 0.1.1

- Added the viewer command bodies (library, document IO, explorer, markdown resources, settings) behind `PathGrants`, `DocumentBackend` and `WorkspaceWatcher`, returning `WorkspaceError`. Additive.

## 0.1.0 (unreleased)

- First RightKit release, moved from ViewRight (src-tauri persistence.rs, migrations/0001_init.sql).
- `markdown_index::index_markdown_folder` (the body of the `index_markdown_folder` command; consent and path grants stay with the caller) and `WorkspaceError`. Moved from ViewRight (commands/markdown_index.rs).
