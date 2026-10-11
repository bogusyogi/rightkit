# Changelog

## 0.4.0 — 2026-10-11

Breaking:
- `DbOptions` gained `pub integrity: IntegrityCheck`. Struct literals without `..` no longer compile; use `DbOptions::new(path)` or `..Default::default()`.
- `MigrateOptions` gained `pub legacy_versions: Option<LegacyVersionTable>`. Struct literals without `..` no longer compile.
- `RecoveryPolicy` gained `pub quarantine_stamp: QuarantineStamp` and `pub sidecars: SidecarPolicy`. Struct literals without `..` no longer compile.
- `VersionStorage` gained the `VersionTable { table, version_column, applied_at_column }` variant. Exhaustive matches need a new arm.
- `DbError` gained `InvalidQuarantineSuffix(String)`. Exhaustive matches need a new arm.
- Recovery treats `DbError::Integrity` as unreadable. A caller that sets `integrity_check_on_open` with a recovery policy now quarantines and recreates instead of receiving an error.

Added:
- `VersionStorage::VersionTable { table, version_column, applied_at_column }` stores one row per applied version, in ViewRight's `_migrations(version, applied_at)` shape. The applied version is the highest stored `version_column`, and a rollback deletes the top row. `VersionStorage::version_table(table)` uses the `version` and `applied_at` column names. The existing `VersionStorage::Table { table, key }` is unchanged. The new variant is not named `Table` because a changed `Table` would drop its `key` field.
- `LegacyVersionTable` and `MigrateOptions::import_legacy_versions`. Once, from version 0, the configured storage adopts the highest version in an app's existing version table, so migrations up to it are not re-run. The legacy table is only read. A legacy version above the newest migration is refused with `NewerDatabase`, and nothing is written.
- `IntegrityCheck { Off, Quick, Full }` and `DbOptions::integrity`, default `Off`. `Quick` runs `PRAGMA quick_check`. `Full` runs `integrity_check`, or `cipher_integrity_check` when keyed. `integrity_check_on_open` alone still means `Full`, and `integrity` takes precedence when it is not `Off`.
- `QuarantineStamp` (`Millis` default, `Seconds`, `Custom(fn(SystemTime) -> String)`) and `SidecarPolicy` (`RenameSidecars` default, `DeleteSidecars`). `RecoveryPolicy` gained `quarantine_stamp` and `sidecars`. `quarantine_database_with` takes both.
- `DbError::InvalidQuarantineSuffix`, for an empty custom suffix or one containing a path separator or NUL.

Changed:
- `DbError::Integrity` is recoverable. `open_writer_recovering` quarantines and recreates for a failed `Quick` or `Full` check, under the same keyed/plain policy as a corrupt file. Callers that set `integrity_check_on_open` with a recovery policy now get quarantine instead of an error.
- Quarantine refuses to overwrite an existing destination. Sidecar move or delete failures are returned instead of ignored, so a stale `-wal` cannot be replayed into the replacement database.

- `statement-cache` feature (default on): enables rusqlite's `cache`, so `prepare_cached` works through the re-export. Without it `prepare_cached` did not compile.
- `QuarantineOptions { name, sidecars, move_mode, marker }` and `quarantine_database_opts`. `marker` writes an owner-only JSON recovery marker (`{"schema","original","quarantined","unixMs"}`) before any file moves, and the marker stays after success. `move_mode: MoveMode::{Rename, HardLinkThenRemove}` selects the move; `HardLinkThenRemove` links the destination before removing the original. `quarantine_database` and `quarantine_database_with` are unchanged and use the defaults.
- `DbOptions::secure_delete: Option<bool>` (default `None`) applies `PRAGMA secure_delete` at open. `owner_only = false` is documented as the intended switch when sibling processes read the file.
- Breaking for struct literals of `DbOptions` without `..`: the new `secure_delete` field. Use `DbOptions::new(path)` or `..`.

The new options default to the previous behaviour.

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
