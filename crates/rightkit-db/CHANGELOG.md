# Changelog

## 0.3.2 — 2026-10-09

- **Narrower `rightkit-fs` dependency.** Depends on `rightkit-fs` 0.3.0 with `default-features = false, features = ["fs"]`. The `lock`, `json` and `digest` features are no longer in the db dependency tree. Public API unchanged (`ca41ba38`).

## 0.3.1 — 2026-10-08

- **`sqlcipher-system` feature.** SQLCipher with the system crypto provider (CommonCrypto on Apple, libcrypto elsewhere). Use it only where existing encrypted databases were created with system crypto; switching providers is not silent (`4118bd54`).
- **Shared `cipher` gate.** `sqlcipher` and `sqlcipher-system` both enable an internal `cipher` feature, so keyed open and `rekey` compile under either. Enabling both is a compile error (`4118bd54`).
- `DbError::SqlcipherFeatureRequired` message now names both features. The variant is unchanged (`4118bd54`).

## 0.3.0 — 2026-10-08

Breaking:
- `DbOptions` gained `pub busy_timeout: Duration` (default five seconds, applied before key verification or any bootstrap statement). `MigrateOptions` gained `pub version_storage: VersionStorage` (default `VersionStorage::default()`). Both structs have public fields and are not `#[non_exhaustive]`, so a struct literal without `..` no longer compiles. Build them with `DbOptions::new(path)` or `..Default::default()` (`ee6ec860`).

Added:
- `open_read_only`, `open_existing_writer` and `open_in_memory` (default-options opens; the read-only and existing-writer opens never create a missing database) (`ee6ec860`).
- `restore_from` (backup restore), re-exported beside `online_backup_to` (`ee6ec860`).
- `VersionStorage` enum; `migration_version` and `rollback_to_with_storage` take it (`ee6ec860`).
- `hooks` feature, which enables rusqlite hooks for cancellation (`ee6ec860`).

Changed:
- `rightkit-fs` dependency is now a caret requirement (`0.1.0`) instead of an exact pin (`=0.1.0`) (`ee6ec860`).

## 0.2.1 — 2026-10-07

- **`rusqlite` re-export.** `pub use rusqlite;`, so callers share the SQLite version used by db. 0.2.0 was published without it (`7ffb1c87`, `2cd866ce`).

## 0.2.0 — 2026-10-07

Breaking:
- `DbOptions` gained `pub journal_env: Option<String>` and `pub per_host_fallback: bool`. A struct literal without `..` no longer compiles; use `DbOptions::new(path)` (`6a476cdd`).

Added:
- `DbOptions::with_journal_env` sets the app's own kill-switch variable, checked before `JOURNAL_ENV` (`RIGHTKIT_SQLITE_JOURNAL_MODE`) (`6a476cdd`).
- `DbOptions::with_per_host_fallback(on)`, default off. When a rollback journal is in force (network mount, kill switch, or WAL refused), it opens the per-host sibling instead of the requested file, so hosts sharing a network directory never share one rollback-journaled database. The requested file is not created (`6a476cdd`).
- `per_host_path` and `DbOptions::effective_path()` (`6a476cdd`).
- Unix builds depend on `libc` for the host-name call in the `host` module (`6a476cdd`).
