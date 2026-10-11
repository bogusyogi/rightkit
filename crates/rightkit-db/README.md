# rightkit-db

Correct-by-default SQLite and SQLCipher connection bootstrap for Right Suite desktop apps. It owns connection pragmas, SQLCipher key ordering, a bounded read pool, checkpointing, optimization, optional integrity checks, online backup and restore, and versioned migrations. It does not own app schemas or queries.

Catalog capability: `sqlite-storage`. Published to crates.io (see [INDEX.md](../../INDEX.md)).

## When to use it

- Use it to open a SQLite or SQLCipher database with the suite's journal, busy-timeout and durability rules.
- Use `write_txn()` for any transaction that may write. It starts `BEGIN IMMEDIATE`, so read-then-write work cannot fail with an unretryable stale snapshot.

Do not use it to define app tables or queries. Keep those in the app.

## Default behaviour

The default writer and legacy reader open select WAL with rollback fallback, a bounded busy wait and journal, memory-backed temporary storage, and the selected durability profile.

- `DurabilityProfile::Full`: for user mutations that cannot be reconstructed.
- `DurabilityProfile::Normal`: for rebuildable indexes or telemetry.
- `CacheProfile::LargeWorkingSet`: opt-in. It is currently intended for MailRight.

## Cargo features

| Feature | Default | Pulls in | Enables |
|---|---|---|---|
| `bundled` | yes | `rusqlite/bundled` | Bundled plain SQLite. |
| `sqlcipher` | no | `cipher`, `rusqlite/bundled-sqlcipher-vendored-openssl` | SQLCipher with vendored OpenSSL, the same crypto provider on every OS. |
| `sqlcipher-system` | no | `cipher`, `rusqlite/bundled-sqlcipher` | SQLCipher with the system crypto provider: CommonCrypto on Apple, libcrypto elsewhere. Use it where existing encrypted databases were created with system crypto. Switching providers is not a silent refactor. |
| `cipher` | no | none | Internal gate for keyed open and `rekey`. Enabled by either SQLCipher feature. Do not name it directly. |
| `hooks` | no | `rusqlite/hooks` | `progress_handler()` for cancellation through returned connections. The `rightkit_db::rusqlite` re-export is not gated by it (`src/lib.rs:22`). |

SQLCipher consumers must turn off default features and choose exactly one provider feature. Both providers expose the same keyed-open and `rekey` APIs. Enabling both is a compile error. Keep the provider compatible with existing encrypted databases:

```toml
rightkit-db = { version = "<exact published version from INDEX.md>", default-features = false, features = ["sqlcipher"] }
```

`rusqlite` is a caret requirement (`0.40.1`), so consumers that resolve `0.40.2` stay compatible.

## Usage

Taken from `tests/contract.rs` and the crate's read-only and migration API. Illustrative as a snippet:

```rust
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
```

## Other APIs

- `open_read_only(path)` opens with `SQLITE_OPEN_READ_ONLY`. `open_existing_writer(path)` opens read-write without CREATE. Both accept paths or `file:` URIs with query parameters, and refuse missing files. The matching `DbOptions` methods use the path, key, timeout and optional integrity check. They skip journal selection, per-host routing, permission changes, `init`, optimization and migrations. Read-only main databases still allow SQLite TEMP tables unless callers set `query_only`.
- `open_reader()` is read-write or create bootstrap, then `query_only` as the last setup statement, read back and required to be on. It does not run `PRAGMA optimize`; the writer runs it when it opens.
- `open_read_pool(cap)` and `open_read_pool_with(cap, ReadPoolOptions)` return a `ReadPool`. Every reader has `query_only` set and read back as on, or its open fails. The default pool (`ReadPoolOptions::new()`) keeps read-write file handles. `ReadPoolOptions::with_read_only(true)` opens readers with `SQLITE_OPEN_READ_ONLY`, so writes fail even after `query_only` is turned off. A read-only pool needs an existing database: it never creates the file, changes the journal mode, narrows permissions, or runs `optimize` or `init`. A rollback journal left by a crash must be recovered by a writer first. `ReadPoolOptions::with_reader_cache_kib(kib)` sets each reader's `cache_size` to `-kib` (KiB, 1 to 2147483647, checked when the pool opens). It overrides `CacheProfile::LargeWorkingSet` on readers only. `ReadPool::close()` stops checkout and drops idle readers; a checked-out reader closes when it is returned. `ReadPool::close_and_wait(timeout)` closes, then waits until every reader is returned, so the database files are released (needed on Windows). If readers are still out at the timeout, it returns `DbError::ReadPoolCloseTimeout { outstanding, timeout }`. After close, `get()` returns `DbError::ReadPoolClosed`.
- `DbOptions::busy_timeout: Duration` defaults to five seconds. It is installed before any bootstrap SQL, including key verification and `init`.
- `open_in_memory()` returns a private ephemeral connection. The matching `DbOptions::open_in_memory()` keeps key, timeout, initializer, cache, foreign-key, durability, optimization and integrity options. Path, filesystem permissions, journal selection and mmap do not apply. The journal mode stays `memory`, including with `WalStrict`. Run `migrate` with `db_path: None` to skip filesystem backup and marker handling.
- `MigrateOptions::version_storage` defaults to `VersionStorage::UserVersion`. `VersionStorage::Table { table, key }` uses `key` and `value` columns compatible with Blueprint's `meta` table. Values are decimal text. Identifiers accept ASCII letters, digits and underscores. Keys are bound parameters. A missing table or key reads as zero. Table creation and version writes share each migration transaction. `migration_version` reads the chosen storage, and `rollback_to_with_storage` reverses it. `user_version` and `rollback_to` keep the default storage.
- `DbOptions::restore_from(backup_path)` checks every `PRAGMA integrity_check` row, including CHECK constraints, before it restores an existing live database through SQLite's backup API. A read transaction holds the validated source snapshot until the copy finishes. A missing or corrupt backup leaves the live database untouched. A missing live database is refused. The backup must use the options' SQLCipher key. The free function `restore_from(options, destination, backup_path)` restores into an existing mutable connection, including in-memory stores. The destination must be writable and outside any transaction. Busy and locked retries are bounded by the options' timeout.

## Platform support

- Unix: descriptor-relative file handling through `rightkit-fs`.
- Windows: the same database API. SQLCipher provider choice is the main per-OS difference (see Cargo features).
- Apple: `sqlcipher-system` uses CommonCrypto.

## Verification

Tests: `tests/contract.rs`, `tests/e2e.rs`, `tests/journal_fallback.rs`, `tests/membrane.rs`, `tests/read_pool.rs`.

## Version and changes

Published version: see [INDEX.md](../../INDEX.md). Release notes: [CHANGELOG.md](CHANGELOG.md).

Publish only through the RightKit release command:

```text
right-release publish cargo --crate rightkit-db
```
