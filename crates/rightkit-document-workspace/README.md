# rightkit-document-workspace

Document workspace store: recent files, pinned files and folders, per-document view state and tab session in one SQLite file, with corruption quarantine and path purge.

Moved into RightKit from ViewRight (src-tauri persistence.rs, migrations/0001_init.sql) on 2026-11-01.

## Command bodies (0.1.1)

Besides the store, the crate holds the viewer command bodies as synchronous functions that return `WorkspaceError`: `library` (recent files, pins, view state, tab session), `document_io` (read, write, copy, sibling images, file watching), `explorer` (locations, listings, folder watches, reveal), `markdown_resources` (reference inspection, pasted-asset save, commit, rollback) and `settings` (`SettingsStore`). The host implements three traits: `PathGrants` (its path-grant broker), `DocumentBackend` (format classification and document IO) and `WorkspaceWatcher` (OS watchers), and keeps its IPC wrappers.
