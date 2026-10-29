//! Membrane compatibility: no-create opens, URI paths, migration metadata, and restore.

use rightkit_db::rusqlite::Connection;
use rightkit_db::{
    journal_mode, migrate, migration_version, online_backup_to, open_existing_writer,
    open_in_memory, open_read_only, restore_from, rollback_to_with_storage, user_version,
    CacheProfile, DbError, DbOptions, DurabilityProfile, JournalSelect, MigrateOptions, Migration,
    VersionStorage,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

fn temp_path(dir: &tempfile::TempDir, name: &str) -> PathBuf {
    // macOS may spell this directory /var or /private/var; normalize before use.
    dir.path().canonicalize().unwrap().join(name)
}

fn pragma_i64(conn: &Connection, name: &str) -> i64 {
    conn.query_row(&format!("PRAGMA {name}"), [], |row| row.get(0))
        .unwrap()
}

fn value(conn: &Connection) -> String {
    conn.query_row("SELECT value FROM item", [], |row| row.get(0))
        .unwrap()
}

fn seed(path: &Path, contents: &str) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch("CREATE TABLE item(value TEXT);")
        .unwrap();
    conn.execute("INSERT INTO item VALUES (?1)", [contents])
        .unwrap();
}

fn file_uri(path: &Path, query: &str) -> String {
    let text = path.to_str().unwrap();
    // Windows canonical paths carry a verbatim prefix that SQLite URIs do not accept.
    let text = text
        .strip_prefix("\\\\?\\")
        .unwrap_or(text)
        .replace('\\', "/");
    let text = if text.as_bytes().get(1) == Some(&b':') {
        format!("/{text}")
    } else {
        text
    };
    let mut encoded = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b':' | b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    format!("file:{encoded}?{query}")
}

#[test]
fn no_create_opens_refuse_missing_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir, "missing.sqlite");
    assert!(open_read_only(&path).is_err());
    assert!(open_existing_writer(&path).is_err());
    assert!(DbOptions::new(&path).open_read_only().is_err());
    assert!(DbOptions::new(&path).open_existing_writer().is_err());
    assert!(!path.exists());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn read_only_open_refuses_main_writes_even_when_query_only_is_disabled() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir, "readonly.sqlite");
    seed(&path, "kept");
    let conn = open_read_only(&path).unwrap();
    assert_eq!(value(&conn), "kept");
    conn.pragma_update(None, "query_only", false).unwrap();
    assert!(conn
        .execute("INSERT INTO item VALUES ('forbidden')", [])
        .is_err());
    assert!(conn
        .execute_batch("CREATE TABLE forbidden(id INTEGER);")
        .is_err());
    assert!(conn.execute_batch("PRAGMA user_version=99;").is_err());
    assert_eq!(user_version(&conn).unwrap(), 0);
    // Ledger read lanes need connection-local TEMP scope/erasure tables.
    conn.execute_batch("CREATE TEMP TABLE scope(id INTEGER); INSERT INTO scope VALUES(1);")
        .unwrap();
}

#[test]
fn existing_opens_skip_mutating_bootstrap_and_preserve_exact_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir, "existing.sqlite");
    seed(&path, "kept");
    let before = std::fs::read(&path).unwrap();
    let called = Arc::new(AtomicUsize::new(0));
    let observed = called.clone();
    let mut options = DbOptions::new(&path).with_per_host_fallback(true);
    options.journal = JournalSelect::Truncate;
    options.profile = CacheProfile::LargeWorkingSet;
    options.integrity_check_on_open = true;
    options.init = Some(Arc::new(move |conn| {
        observed.fetch_add(1, Ordering::SeqCst);
        conn.execute_batch("CREATE TABLE bootstrap_wrote(id INTEGER);")
    }));
    for conn in [
        options.open_read_only().unwrap(),
        options.open_existing_writer().unwrap(),
    ] {
        assert_eq!(journal_mode(&conn).unwrap(), "delete");
        assert_eq!(value(&conn), "kept");
        assert_ne!(pragma_i64(&conn, "cache_size"), -65_536);
        let opened: String = conn
            .query_row("PRAGMA database_list", [], |row| row.get(2))
            .unwrap();
        assert_eq!(
            Path::new(&opened).canonicalize().unwrap(),
            path.canonicalize().unwrap()
        );
    }
    assert_eq!(called.load(Ordering::SeqCst), 0);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    let writer = open_existing_writer(&path).unwrap();
    writer
        .execute("UPDATE item SET value='written'", [])
        .unwrap();
    assert_eq!(value(&writer), "written");
}

#[test]
fn uri_opens_preserve_escaped_paths_and_query_parameters() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir, "uri space # percent%.sqlite");
    seed(&path, "URI");
    let uri = file_uri(&path, "mode=ro&immutable=1");
    for conn in [
        open_read_only(&uri).unwrap(),
        open_existing_writer(&uri).unwrap(),
    ] {
        assert_eq!(value(&conn), "URI");
        assert!(conn.execute("DELETE FROM item", []).is_err());
        let opened: String = conn
            .query_row("PRAGMA database_list", [], |row| row.get(2))
            .unwrap();
        assert_eq!(
            Path::new(&opened).canonicalize().unwrap(),
            path.canonicalize().unwrap()
        );
    }
    let writer = open_existing_writer(file_uri(&path, "mode=rw")).unwrap();
    writer
        .execute("UPDATE item SET value='URI writer'", [])
        .unwrap();
    let missing = temp_path(&dir, "missing URI.sqlite");
    assert!(open_read_only(file_uri(&missing, "mode=ro")).is_err());
    assert!(open_existing_writer(file_uri(&missing, "mode=rw")).is_err());
    // URI parameters cannot widen no-create flags into CREATE.
    assert!(open_existing_writer(file_uri(&missing, "mode=rwc")).is_err());
    assert!(!missing.exists());
}

#[test]
fn timeout_is_installed_before_init_and_on_every_open_kind() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir, "timeout.sqlite");
    let mut options = DbOptions::new(&path);
    assert_eq!(options.busy_timeout, Duration::from_secs(5));
    options.busy_timeout = Duration::from_millis(137);
    options.init = Some(Arc::new(|conn| {
        assert_eq!(pragma_i64(conn, "busy_timeout"), 137);
        Ok(())
    }));
    let writer = options.open_writer().unwrap();
    assert_eq!(pragma_i64(&writer, "busy_timeout"), 137);
    for conn in [
        options.open_reader().unwrap(),
        options.open_read_only().unwrap(),
        options.open_existing_writer().unwrap(),
        options.open_in_memory().unwrap(),
    ] {
        assert_eq!(pragma_i64(&conn, "busy_timeout"), 137);
    }
    options.busy_timeout = Duration::ZERO;
    assert_eq!(
        pragma_i64(&options.open_existing_writer().unwrap(), "busy_timeout"),
        0
    );
}

#[test]
fn in_memory_uses_meaningful_bootstrap_without_creating_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir, "ignored.sqlite");
    let mut options = DbOptions::new(&path).with_per_host_fallback(true);
    options.journal = JournalSelect::WalStrict;
    options.profile = CacheProfile::LargeWorkingSet;
    options.durability = DurabilityProfile::Full;
    options.foreign_keys = true;
    options.integrity_check_on_open = true;
    options.init = Some(Arc::new(|conn| {
        conn.execute_batch("CREATE TABLE initialized(id INTEGER);")
    }));
    let conn = options.open_in_memory().unwrap();
    assert_eq!(journal_mode(&conn).unwrap(), "memory");
    assert_eq!(pragma_i64(&conn, "busy_timeout"), 5_000);
    assert_eq!(pragma_i64(&conn, "foreign_keys"), 1);
    assert_eq!(pragma_i64(&conn, "temp_store"), 2);
    assert_eq!(pragma_i64(&conn, "cache_size"), -65_536);
    conn.execute_batch("INSERT INTO initialized VALUES(1);")
        .unwrap();
    let all = [Migration::sql(1, "item", "CREATE TABLE item(value TEXT);")];
    assert_eq!(
        migrate(&conn, None, &all, &MigrateOptions::default())
            .unwrap()
            .to,
        1
    );
    conn.execute("INSERT INTO item VALUES ('ephemeral')", [])
        .unwrap();
    assert_eq!(value(&conn), "ephemeral");
    assert!(!path.exists());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    let other = open_in_memory().unwrap();
    assert!(other.prepare("SELECT * FROM item").is_err());
}

fn meta_migrations() -> Vec<Migration> {
    vec![
        Migration::sql(1, "item", "CREATE TABLE item(value TEXT);").with_down("DROP TABLE item;"),
        Migration::sql(2, "index", "CREATE INDEX item_value ON item(value);")
            .with_down("DROP INDEX item_value;"),
    ]
}

#[test]
fn meta_table_version_round_trips_across_migrate_reopen_and_rollback() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir, "meta.sqlite");
    let options = DbOptions::new(&path);
    let storage = VersionStorage::Table {
        table: "meta".into(),
        key: "schema_version".into(),
    };
    let migrate_options = MigrateOptions {
        version_storage: storage.clone(),
        ..Default::default()
    };
    let conn = options.open_writer().unwrap();
    assert_eq!(migration_version(&conn, &storage).unwrap(), 0);
    conn.execute_batch(
        "PRAGMA user_version=42;
         CREATE TABLE meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
         INSERT INTO meta VALUES('other','kept');",
    )
    .unwrap();
    drop(conn);
    let (conn, first) = options
        .open_writer_migrated(&meta_migrations()[..1], &migrate_options)
        .unwrap();
    assert_eq!(first.applied, vec![1]);
    assert_eq!(migration_version(&conn, &storage).unwrap(), 1);
    drop(conn);
    let (conn, second) = options
        .open_writer_migrated(&meta_migrations(), &migrate_options)
        .unwrap();
    assert_eq!((second.from, second.to), (1, 2));
    assert!(second.backup.unwrap().exists());
    assert!(
        migrate(&conn, Some(&path), &meta_migrations(), &migrate_options)
            .unwrap()
            .applied
            .is_empty()
    );
    assert_eq!(user_version(&conn).unwrap(), 42);
    let version: String = conn
        .query_row(
            "SELECT value FROM meta WHERE key='schema_version'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(version, "2");
    assert_eq!(
        rollback_to_with_storage(&conn, &meta_migrations(), 0, &storage).unwrap(),
        vec![2, 1]
    );
    assert_eq!(migration_version(&conn, &storage).unwrap(), 0);
    assert_eq!(user_version(&conn).unwrap(), 42);
    assert!(conn.prepare("SELECT * FROM item").is_err());
    let other: String = conn
        .query_row("SELECT value FROM meta WHERE key='other'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(other, "kept");
    drop(conn);
    assert_eq!(
        migration_version(&open_read_only(&path).unwrap(), &storage).unwrap(),
        0
    );
}

#[test]
fn meta_version_validation_and_failed_steps_leave_state_intact() {
    let conn = open_in_memory().unwrap();
    // A SQL keyword is a valid table name; a quote-bearing key remains a parameter.
    let storage = VersionStorage::Table {
        table: "select".into(),
        key: "schema'quote".into(),
    };
    let options = MigrateOptions {
        version_storage: storage.clone(),
        ..Default::default()
    };
    let failing = [Migration::sql(
        1,
        "broken",
        "CREATE TABLE transient(id); INSERT INTO missing VALUES(1);",
    )];
    assert!(matches!(
        migrate(&conn, None, &failing, &options),
        Err(DbError::MigrationFailed { .. })
    ));
    assert_eq!(migration_version(&conn, &storage).unwrap(), 0);
    assert!(conn.prepare("SELECT * FROM transient").is_err());
    migrate(&conn, None, &meta_migrations()[..1], &options).unwrap();
    assert_eq!(migration_version(&conn, &storage).unwrap(), 1);
    for bad in ["garbage", "-1", "9223372036854775808"] {
        conn.execute("UPDATE \"select\" SET value=?1", [bad])
            .unwrap();
        assert!(matches!(
            migration_version(&conn, &storage),
            Err(DbError::InvalidMigrations(_))
        ));
    }
    let unsafe_storage = VersionStorage::Table {
        table: "meta\"; DROP TABLE item;--".into(),
        key: "version".into(),
    };
    assert!(matches!(
        migration_version(&conn, &unsafe_storage),
        Err(DbError::InvalidMigrations(_))
    ));
    assert!(conn.prepare("SELECT * FROM item").is_ok());
}

#[test]
fn restore_rejects_corrupt_or_missing_backup_without_touching_live_file() {
    let dir = tempfile::tempdir().unwrap();
    let live = temp_path(&dir, "live.sqlite");
    seed(&live, "live");
    let before = std::fs::read(&live).unwrap();
    let options = DbOptions::new(&live);
    let corrupt = temp_path(&dir, "corrupt.sqlite");
    std::fs::write(&corrupt, b"not a SQLite database").unwrap();
    assert!(options.restore_from(&corrupt).is_err());
    let missing = temp_path(&dir, "missing.sqlite");
    assert!(options.restore_from(&missing).is_err());
    assert!(!missing.exists());
    assert_eq!(std::fs::read(&live).unwrap(), before);
    assert_eq!(value(&open_read_only(&live).unwrap()), "live");
    let mut conn = open_existing_writer(&live).unwrap();
    assert!(restore_from(&options, &mut conn, &corrupt).is_err());
    assert_eq!(value(&conn), "live");
    drop(conn);
    assert_eq!(std::fs::read(&live).unwrap(), before);
    let absent_live = temp_path(&dir, "absent-live.sqlite");
    assert!(DbOptions::new(&absent_live).restore_from(&corrupt).is_err());
    assert!(!absent_live.exists());
}

#[test]
fn restore_good_backup_replaces_data_schema_and_version() {
    let dir = tempfile::tempdir().unwrap();
    let live = temp_path(&dir, "live.sqlite");
    let options = DbOptions::new(&live);
    let conn = options.open_writer().unwrap();
    conn.execute_batch(
        "CREATE TABLE item(value TEXT);
         INSERT INTO item VALUES ('saved');
         PRAGMA user_version=7;",
    )
    .unwrap();
    let backup = temp_path(&dir, "good.sqlite");
    online_backup_to(&options, &conn, &backup, None).unwrap();
    conn.execute_batch(
        "UPDATE item SET value='new'; CREATE TABLE extra(id); PRAGMA user_version=9;",
    )
    .unwrap();
    options.restore_from(&backup).unwrap();
    assert_eq!(value(&conn), "saved");
    assert_eq!(user_version(&conn).unwrap(), 7);
    assert!(conn.prepare("SELECT * FROM extra").is_err());
    let mut ephemeral = open_in_memory().unwrap();
    restore_from(&options, &mut ephemeral, &backup).unwrap();
    assert_eq!(value(&ephemeral), "saved");
    assert_eq!(user_version(&ephemeral).unwrap(), 7);
    let absent = temp_path(&dir, "absent.sqlite");
    assert!(DbOptions::new(&absent).restore_from(&backup).is_err());
    assert!(!absent.exists());
}

#[test]
fn restore_reads_committed_pages_from_backup_wal() {
    let dir = tempfile::tempdir().unwrap();
    let live = temp_path(&dir, "live.sqlite");
    seed(&live, "live");
    let backup = temp_path(&dir, "wal-source.sqlite");
    let source_options = DbOptions::new(&backup);
    let source = source_options.open_writer().unwrap();
    source.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    source
        .execute_batch("CREATE TABLE item(value TEXT); INSERT INTO item VALUES('WAL');")
        .unwrap();
    assert!(backup.with_file_name("wal-source.sqlite-wal").exists());
    DbOptions::new(&live).restore_from(&backup).unwrap();
    assert_eq!(value(&open_read_only(&live).unwrap()), "WAL");
    assert_eq!(value(&source), "WAL");
}

#[cfg(feature = "hooks")]
#[test]
fn hooks_allow_cancellation_through_returned_connection() {
    let mut conn = open_in_memory().unwrap();
    conn.progress_handler(1, Some(|| true)).unwrap();
    let error = conn
        .query_row(
            "WITH RECURSIVE n(x) AS (
                SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<100000
             ) SELECT sum(x) FROM n",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_err();
    assert!(matches!(
        error,
        rightkit_db::rusqlite::Error::SqliteFailure(code, _)
            if code.code == rightkit_db::rusqlite::ErrorCode::OperationInterrupted
    ));
    conn.progress_handler(0, None::<fn() -> bool>).unwrap();
    assert_eq!(
        conn.query_row("SELECT 1", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[cfg(feature = "cipher")]
#[test]
fn keyed_restore_and_in_memory_work_with_minimal_opens() {
    let dir = tempfile::tempdir().unwrap();
    let live = temp_path(&dir, "keyed.sqlite");
    let options = DbOptions::new(&live).with_passphrase("YWJjZA==");
    let conn = options.open_writer().unwrap();
    conn.execute_batch("CREATE TABLE item(value TEXT); INSERT INTO item VALUES('secret');")
        .unwrap();
    let backup = temp_path(&dir, "keyed.bak");
    online_backup_to(&options, &conn, &backup, None).unwrap();
    conn.execute("UPDATE item SET value='changed'", []).unwrap();
    drop(conn);
    options.restore_from(&backup).unwrap();
    assert_eq!(value(&options.open_read_only().unwrap()), "secret");
    let mut memory = options.open_in_memory().unwrap();
    restore_from(&options, &mut memory, &backup).unwrap();
    assert_eq!(value(&memory), "secret");
}
