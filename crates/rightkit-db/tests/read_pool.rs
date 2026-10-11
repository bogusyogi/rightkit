//! Read-pool write refusal, close and drain semantics, and reader cache options, against real
//! SQLite files.

use rightkit_db::rusqlite::Connection;
use rightkit_db::{journal_mode, CacheProfile, DbError, DbOptions, ReadPoolOptions};
use std::path::PathBuf;
use std::time::Duration;

fn pragma_i64(conn: &Connection, name: &str) -> i64 {
    conn.query_row(&format!("PRAGMA {name}"), [], |row| row.get(0))
        .unwrap()
}

fn temp_path(dir: &tempfile::TempDir, name: &str) -> PathBuf {
    // macOS may spell this directory /var or /private/var; normalize before use.
    dir.path().canonicalize().unwrap().join(name)
}

fn values(conn: &Connection) -> Vec<String> {
    let mut statement = conn
        .prepare("SELECT value FROM item ORDER BY value")
        .unwrap();
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap();
    let out: Vec<String> = rows.map(|row| row.unwrap()).collect();
    out
}

/// A WAL database holding one table and one row. The returned writer keeps it open.
fn fixture(dir: &tempfile::TempDir) -> (DbOptions, Connection) {
    let options = DbOptions::new(temp_path(dir, "pool.sqlite"));
    let writer = options.open_writer().unwrap();
    writer
        .execute_batch("CREATE TABLE item(value TEXT); INSERT INTO item VALUES ('kept');")
        .unwrap();
    (options, writer)
}

#[test]
fn default_reader_refuses_writes_and_leaves_the_table_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let (options, writer) = fixture(&dir);
    let pool = options.open_read_pool(1).unwrap();
    let reader = pool.get().unwrap();

    assert!(reader
        .execute("INSERT INTO item VALUES ('forbidden')", [])
        .is_err());
    assert!(reader
        .execute_batch("UPDATE item SET value = 'changed'")
        .is_err());
    assert!(reader
        .execute_batch("CREATE TABLE forbidden(id INTEGER);")
        .is_err());
    drop(reader);

    assert_eq!(values(&writer), vec!["kept".to_string()]);
}

#[test]
fn read_only_reader_refuses_writes_even_after_query_only_is_turned_off() {
    let dir = tempfile::tempdir().unwrap();
    let (options, writer) = fixture(&dir);
    let read_only = ReadPoolOptions::new().with_read_only(true);
    let pool = options.open_read_pool_with(1, read_only).unwrap();
    let reader = pool.get().unwrap();
    assert!(reader.is_readonly("main").unwrap());
    assert_eq!(pragma_i64(&reader, "query_only"), 1);
    assert_eq!(values(&reader), vec!["kept".to_string()]);

    reader.pragma_update(None, "query_only", false).unwrap();
    assert!(reader
        .execute("INSERT INTO item VALUES ('forbidden')", [])
        .is_err());
    assert!(reader
        .execute_batch("CREATE TABLE forbidden(id INTEGER);")
        .is_err());
    assert!(reader.execute_batch("PRAGMA user_version = 99;").is_err());
    drop(reader);

    assert_eq!(values(&writer), vec!["kept".to_string()]);
}

#[test]
fn every_pooled_reader_reads_back_query_only_on() {
    let dir = tempfile::tempdir().unwrap();
    let (options, _writer) = fixture(&dir);
    for pool_options in [
        ReadPoolOptions::new(),
        ReadPoolOptions::new().with_read_only(true),
    ] {
        let pool = options.open_read_pool_with(3, pool_options).unwrap();
        let readers: Vec<_> = (0..3).map(|_| pool.get().unwrap()).collect();
        assert_eq!(pool.total_len(), 3);
        for reader in &readers {
            assert_eq!(pragma_i64(reader, "query_only"), 1);
        }
    }
    let plain = options.open_reader().unwrap();
    assert_eq!(pragma_i64(&plain, "query_only"), 1);
}

#[test]
fn checkout_after_close_returns_the_typed_error() {
    let dir = tempfile::tempdir().unwrap();
    let (options, _writer) = fixture(&dir);
    let pool = options.open_read_pool(2).unwrap();
    pool.close();
    assert!(matches!(pool.get(), Err(DbError::ReadPoolClosed)));
    assert_eq!(pool.idle_len(), 0);
    assert_eq!(pool.total_len(), 0);
}

#[test]
fn a_get_waiting_for_capacity_fails_when_the_pool_closes() {
    let dir = tempfile::tempdir().unwrap();
    let (options, _writer) = fixture(&dir);
    let pool = options.open_read_pool(1).unwrap();
    let held = pool.get().unwrap();
    let waiting = pool.clone();
    let waiter = std::thread::spawn(move || waiting.get().map(|_| ()));
    std::thread::sleep(Duration::from_millis(100));

    pool.close();
    let outcome = waiter.join().unwrap();
    assert!(
        matches!(outcome, Err(DbError::ReadPoolClosed)),
        "waiter returned {outcome:?}"
    );
    drop(held);
    assert_eq!(pool.total_len(), 0);
}

#[test]
fn close_and_wait_returns_ok_once_the_last_reader_is_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let (options, _writer) = fixture(&dir);
    let pool = options.open_read_pool(1).unwrap();
    let reader = pool.get().unwrap();
    let closer = pool.clone();
    let waiter = std::thread::spawn(move || closer.close_and_wait(Duration::from_secs(10)));
    std::thread::sleep(Duration::from_millis(100));

    drop(reader);
    let outcome = waiter.join().unwrap();
    assert!(outcome.is_ok(), "close_and_wait returned {outcome:?}");
    assert_eq!(pool.total_len(), 0);
}

#[test]
fn close_and_wait_times_out_and_names_the_readers_still_out() {
    let dir = tempfile::tempdir().unwrap();
    let (options, _writer) = fixture(&dir);
    let pool = options.open_read_pool(2).unwrap();
    let held = pool.get().unwrap();
    let returned = pool.get().unwrap();
    drop(returned);
    assert_eq!(pool.idle_len(), 1);

    match pool.close_and_wait(Duration::from_millis(50)) {
        Err(DbError::ReadPoolCloseTimeout {
            outstanding,
            timeout,
        }) => {
            // The idle reader was closed by `close`; only the held one is still out.
            assert_eq!(outstanding, 1);
            assert_eq!(timeout, Duration::from_millis(50));
        }
        other => panic!("expected a close timeout, got {other:?}"),
    }

    drop(held);
    assert!(pool.close_and_wait(Duration::from_secs(1)).is_ok());
    assert_eq!(pool.total_len(), 0);
}

#[test]
fn closing_twice_is_a_no_op() {
    let dir = tempfile::tempdir().unwrap();
    let (options, _writer) = fixture(&dir);
    let pool = options
        .open_read_pool_with(2, ReadPoolOptions::new())
        .unwrap();
    let held = pool.get().unwrap();
    let returned = pool.get().unwrap();
    drop(returned);
    assert_eq!(pool.total_len(), 2);
    assert_eq!(pool.idle_len(), 1);

    pool.close();
    assert_eq!(pool.total_len(), 1);
    pool.close();
    assert_eq!(
        pool.total_len(),
        1,
        "a second close must not release the held reader's count again"
    );
    assert_eq!(pool.idle_len(), 0);
    assert!(matches!(pool.get(), Err(DbError::ReadPoolClosed)));

    drop(held);
    assert_eq!(pool.total_len(), 0);
    assert!(pool.close_and_wait(Duration::from_secs(1)).is_ok());
}

#[test]
fn reader_cache_option_reads_back_on_each_pooled_reader() {
    let dir = tempfile::tempdir().unwrap();
    let (options, writer) = fixture(&dir);
    let sized = ReadPoolOptions::new().with_reader_cache_kib(2048);
    let pool = options.open_read_pool_with(2, sized).unwrap();
    let first = pool.get().unwrap();
    let second = pool.get().unwrap();
    assert_eq!(pragma_i64(&first, "cache_size"), -2048);
    assert_eq!(pragma_i64(&second, "cache_size"), -2048);
    drop(first);
    drop(second);

    let read_only = ReadPoolOptions::new()
        .with_read_only(true)
        .with_reader_cache_kib(1024);
    let read_only_pool = options.open_read_pool_with(1, read_only).unwrap();
    let reader = read_only_pool.get().unwrap();
    assert_eq!(pragma_i64(&reader, "cache_size"), -1024);
    // The writer keeps its own default size.
    assert_ne!(pragma_i64(&writer, "cache_size"), -2048);
}

#[test]
fn reader_cache_option_overrides_the_large_profile_on_readers_only() {
    let dir = tempfile::tempdir().unwrap();
    let mut options = DbOptions::new(temp_path(&dir, "large.sqlite"));
    options.profile = CacheProfile::LargeWorkingSet;
    let writer = options.open_writer().unwrap();
    assert_eq!(pragma_i64(&writer, "cache_size"), -65_536);

    let sized = ReadPoolOptions::new().with_reader_cache_kib(2048);
    let pool = options.open_read_pool_with(1, sized).unwrap();
    let reader = pool.get().unwrap();
    assert_eq!(pragma_i64(&reader, "cache_size"), -2048);
    assert_eq!(pragma_i64(&reader, "mmap_size"), 268_435_456);
    drop(reader);

    // Without the option, readers keep the profile size.
    let profile_only = options.open_read_pool(1).unwrap();
    let reader = profile_only.get().unwrap();
    assert_eq!(pragma_i64(&reader, "cache_size"), -65_536);
}

#[test]
fn out_of_range_reader_cache_is_refused_when_the_pool_opens() {
    let dir = tempfile::tempdir().unwrap();
    let (options, _writer) = fixture(&dir);
    let zero = ReadPoolOptions::new().with_reader_cache_kib(0);
    assert!(matches!(
        options.open_read_pool_with(1, zero),
        Err(DbError::InvalidReaderCacheSize(0))
    ));
    let too_large = ReadPoolOptions::new().with_reader_cache_kib(u32::MAX);
    assert!(matches!(
        options.open_read_pool_with(1, too_large),
        Err(DbError::InvalidReaderCacheSize(kib)) if kib == u32::MAX
    ));
}

#[test]
fn read_only_pool_refuses_a_missing_database_and_creates_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir, "missing.sqlite");
    let options = DbOptions::new(&path);
    let read_only = ReadPoolOptions::new().with_read_only(true);
    assert!(options.open_read_pool_with(1, read_only).is_err());
    assert!(!path.exists());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn defaults_are_unchanged_when_the_new_options_are_not_used() {
    assert_eq!(ReadPoolOptions::default(), ReadPoolOptions::new());
    let dir = tempfile::tempdir().unwrap();
    let (options, _writer) = fixture(&dir);
    let plain = options.open_reader().unwrap();
    let pool = options.open_read_pool(1).unwrap();
    let pooled = pool.get().unwrap();

    assert_eq!(
        pragma_i64(&pooled, "cache_size"),
        pragma_i64(&plain, "cache_size")
    );
    assert_ne!(pragma_i64(&pooled, "cache_size"), -65_536);
    assert_eq!(pragma_i64(&pooled, "mmap_size"), 0);
    assert_eq!(pragma_i64(&pooled, "query_only"), 1);
    assert_eq!(journal_mode(&pooled).unwrap(), "wal");
    // Read-write file handles stay the default; read-only is opt-in.
    assert!(!pooled.is_readonly("main").unwrap());
}
