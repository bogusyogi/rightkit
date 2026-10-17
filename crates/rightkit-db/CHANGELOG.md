# Changelog

## 0.4.2

- Additive: public `remove_database_files(&Path) -> io::Result<()>`, the former private `remove_with_sidecars` (Orthic-Labs/rightkit#199). Deletes a database and its `-wal`, `-shm` and `-journal` sidecars; a missing file is not an error, every file is attempted and the first real failure is returned. Backup's internal cleanup still ignores errors. For releasing connections first, `ReadPool::close` / `close_and_wait` already exist (the crate has no `JobStore`; a store type in an app or in rightkit-jobs closes its own writer and pool before calling this). Not compiled when written.

## 0.4.1

- Additive: `ReadPool::get_timeout(Duration) -> Result<Option<PooledReadConnection>>`, a checkout with a bounded wait (Orthic-Labs/rightkit#173). `Ok(Some(reader))` as soon as a reader is idle, can be opened under the cap, or is returned inside the duration; `Ok(None)` when none frees up in time; `Err(DbError::ReadPoolClosed)` when the pool is closed or closes while waiting (it wins over a timeout). A zero duration never waits. `get()` is unchanged and still waits without a bound. Tests in `tests/read_pool.rs`. Not compiled when written.

## 0.4.0 — 2026-10-11

- **Breaking: default features are now empty (`default = []`).** 0.3.x built `bundled` and `statement-cache` by default. Name `features = ["bundled"]` for the bundled SQLite (without it `libsqlite3-sys` links the system library) and `"statement-cache"` for `prepare_cached`. SQLCipher consumers already set `default-features = false` and are unchanged. `rightkit-jobs` (store) now names `bundled` and `statement-cache`.

- Additive: opt-in recovery for a database that cannot be *read*. `DbOptions::open_writer_recovering_unreadable(policy, UnreadablePolicy)` and, with the open step supplied by the caller (the test seam), `open_writer_recovering_via`. `UnreadablePolicy::Fail` (default) returns the new `DbError::Unreadable { path, cause, consecutive_failures, quarantine_after, source }` and never touches the file. `UnreadablePolicy::QuarantineAfter { attempts }` renames the file (with its sidecars, never deleted, never overwritten) and creates a fresh database only after the read failed `attempts` consecutive opens; the count is kept in `<db>.unreadable-opens` and any successful open removes it. Only `UnreadableCause::PermissionDenied` and `UnreadableCause::IoReadFailure` count. `UnreadableCause::StorageUnavailable(StorageProblem)` (disk full, read-only filesystem, device or share not ready, sharing/lock violation, busy or locked, file missing, other) never counts and never quarantines, so a full disk, a dropped mount or a lock cannot replace a healthy database with an empty one. `SQLITE_CANTOPEN` is split by probing the file with a read open (SQLite reports `EACCES` as `CANTOPEN`). New `classify_open_failure`, `unreadable_counter_path`, `RecoveryAction`, `UnreadableRecovery`. The corruption path (`RecoveryPolicy`, `open_writer_recovering`) is unchanged. `DbError::Unreadable` is a new variant of a `#[non_exhaustive]` enum. Tests in `tests/unreadable_recovery.rs`. Not compiled when written.

- Additive: `QuarantineStamp::Seconds` no longer fails when the plain `.corrupt-<secs>` name is taken. The quarantine takes `-1`, `-2`, ... (up to 99 retries) and claims each name with create-new semantics (`rename_exclusive`, or `link` for `MoveMode::HardLinkThenRemove`), so an earlier quarantine is never overwritten. `Millis` and `Custom` still refuse an existing name. Under `SidecarPolicy::RenameSidecars` a name counts as taken when a sidecar name is taken too, and a sidecar that is taken now errors instead of being overwritten. Tests in `tests/quarantine_options.rs`. Not compiled when written.
- `DbOptions::optimize_on_open: bool` (default `true`, so behaviour is unchanged) and `DbOptions::without_optimize_on_open()`. When `false`, writer connections skip `PRAGMA optimize` at open; readers never ran it. The new public field is a breaking change for struct literals written without `..`; use `DbOptions::new(path)` or `..Default::default()`. Not compiled when written.

Breaking:
- `DbError` is `#[non_exhaustive]`. Exhaustive matches need a wildcard arm. Variants added later are not breaking.
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
- `ReadPoolOptions` with `new()`, `with_read_only(on)` and `with_reader_cache_kib(kib)`, and `DbOptions::open_read_pool_with(cap, options)`. `open_read_pool(cap)` is unchanged and uses `ReadPoolOptions::new()`.
- `with_read_only(true)`: pooled readers open with `SQLITE_OPEN_READ_ONLY`, so writes fail even if `query_only` is turned off. The database must already exist. The pool never creates it, changes its journal mode, narrows its permissions, or runs `optimize` or `init`. Default off.
- `with_reader_cache_kib(kib)`: each pooled reader gets `PRAGMA cache_size = -kib` (KiB, 1 to 2147483647, checked when the pool opens). It overrides `CacheProfile::LargeWorkingSet` on readers only; writers and `open_reader()` keep the profile size.
- `ReadPool::close()` stops checkout and drops idle readers. A checked-out reader closes when it is returned. A second call does nothing.
- `ReadPool::close_and_wait(timeout)` closes the pool, then waits until every reader is returned, so the database files are released. It returns `DbError::ReadPoolCloseTimeout { outstanding, timeout }` if any are still out.
- `DbError` gained `InvalidReaderCacheSize(u32)`, `ReadPoolClosed`, `ReadPoolCloseTimeout { outstanding, timeout }` and `ReadOnlyNotEffective(String)`.

Changed:
- `DbError::Integrity` is recoverable. `open_writer_recovering` quarantines and recreates for a failed `Quick` or `Full` check, under the same keyed/plain policy as a corrupt file. Callers that set `integrity_check_on_open` with a recovery policy now get quarantine instead of an error.
- Quarantine refuses to overwrite an existing destination. Sidecar move or delete failures are returned instead of ignored, so a stale `-wal` cannot be replayed into the replacement database.
- Every reader verifies `PRAGMA query_only` after setting it and fails to open if it did not stay on. This includes readers from `open_reader()`. Default readers still open read-write at the file level.
- Readers no longer run `PRAGMA optimize`. It can write statistics, and the writer runs it when it opens (`optimize_boundary` remains for apps). `query_only` is set after every other reader setup statement.
- `get()` on a closed pool returns `DbError::ReadPoolClosed`, including a `get` that was waiting for capacity when the pool closed.

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
