# rightkit-db

Correct-by-default SQLite connection bootstrap for Right Suite desktop apps.
It owns connection pragmas, SQLCipher key ordering, bounded read pooling,
checkpointing, optimization, and optional integrity checks. It does not own app
schemas, migrations, or queries.

Default writer & legacy reader opens select WAL with rollback fallback, a bounded
busy wait & journal, memory-backed temporary storage, & selected durability.
Use `write_txn()` for
any transaction that may write; it starts `BEGIN IMMEDIATE` so read-then-write
work cannot fail with an unretryable stale snapshot.

Plain SQLite is the default feature. SQLCipher consumers must disable defaults:

```toml
rightkit-db = { version = "=0.3.0", default-features = false, features = ["sqlcipher"] }
```

Choose `DurabilityProfile::Full` for user mutations that cannot be reconstructed
and `DurabilityProfile::Normal` for rebuildable indexes or telemetry. The
`LargeWorkingSet` cache profile is opt-in and currently intended for MailRight.

## Also (0.3.0)

- `open_read_only(path)` uses `SQLITE_OPEN_READ_ONLY`;
  `open_existing_writer(path)` uses `SQLITE_OPEN_READ_WRITE` without CREATE.
  Both return `rusqlite::Connection`, accept paths or `file:` URIs with query
  parameters, & refuse missing files. Matching `DbOptions` methods use its path,
  key, timeout, & optional integrity check. They bypass journal selection,
  per-host routing, permission changes, `init`, optimization, & migrations.
  Read-only main DBs still allow SQLite TEMP tables unless callers set `query_only`.
  Existing `open_reader()` behavior stays unchanged: read-write/create bootstrap,
  then `query_only`.
- `DbOptions::busy_timeout: std::time::Duration` defaults to five seconds & is
  installed before any bootstrap SQL, including key verification & `init`.
- `open_in_memory()` returns a private ephemeral connection. Matching
  `DbOptions::open_in_memory()` retains key, timeout, initializer, cache,
  foreign-key, durability, optimization, & integrity options. Path, filesystem
  permissions, journal selection/fallback, & mmap do not apply; journal mode
  remains `memory`, including with `WalStrict`. Run `migrate` with `db_path: None`
  to skip filesystem backup & marker handling.
- `MigrateOptions::version_storage` defaults to `VersionStorage::UserVersion`.
  `VersionStorage::Table { table, key }` uses `key`/`value` columns compatible
  with Blueprint's `meta` table. Values are decimal text; identifiers accept ASCII
  letters, digits, & underscores; keys are bound parameters. Missing table/key
  reads as zero. Table creation & version writes share each migration transaction.
  `migration_version` reads chosen storage; `rollback_to_with_storage` reverses it.
  `user_version` & `rollback_to` retain their default storage.
- `DbOptions::restore_from(backup_path)` validates every `PRAGMA integrity_check` row (CHECK constraints included)
  before opening existing live DB, then restores through SQLite's backup API.
  A read transaction holds validated source snapshot until copy completes.
  Missing/corrupt backups leave live DB untouched; missing live DB is refused.
  Backup must use options' SQLCipher key. Free `restore_from(options, destination,
  backup_path)` restores into an existing mutable connection, including in-memory
  stores. Destination must be writable & outside any transaction; busy/locked
  retries are bounded by options' timeout.
- Optional `hooks = ["rusqlite/hooks"]` exposes `progress_handler()` for
  cancellation through returned connections & existing `rightkit_db::rusqlite`
  re-export. Default `bundled` & optional `sqlcipher` features stay unchanged.
  `rusqlite` remains a caret requirement (`0.40.1`), compatible with consumers
  resolving `0.40.2`.

```rust,no_run
use std::time::Duration;
use rightkit_db::{DbOptions, MigrateOptions, VersionStorage};

let mut db = DbOptions::new("file:context.db?mode=ro");
db.busy_timeout = Duration::from_millis(250);
let reader = db.open_read_only()?;

let migrations = MigrateOptions {
    version_storage: VersionStorage::Table {
        table: "meta".into(),
        key: "schema_version".into(),
    },
    ..Default::default()
};
# Ok::<(), rightkit_db::DbError>(())
```

Publish only through:

```text
right-release publish cargo --crate rightkit-db
```
