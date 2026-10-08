use rightkit_db::{
    checkpoint_if_wal_larger_than, checkpoint_truncate, optimize_boundary, write_txn, CacheProfile,
    DbOptions, DurabilityProfile,
};
use std::sync::mpsc;
use std::time::Duration;

fn pragma_i64(conn: &rusqlite::Connection, name: &str) -> i64 {
    conn.query_row(&format!("PRAGMA {name}"), [], |row| row.get(0))
        .unwrap()
}

#[test]
fn writer_and_reader_enforce_the_pragma_contract() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("contract.sqlite");
    let mut options = DbOptions::new(&path);
    options.profile = CacheProfile::LargeWorkingSet;
    options.durability = DurabilityProfile::Full;
    options.foreign_keys = true;
    options.integrity_check_on_open = true;

    let writer = options.open_writer().unwrap();
    assert_eq!(
        writer
            .query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "wal"
    );
    assert_eq!(pragma_i64(&writer, "busy_timeout"), 5_000);
    assert_eq!(pragma_i64(&writer, "synchronous"), 2);
    assert_eq!(pragma_i64(&writer, "journal_size_limit"), 67_108_864);
    assert_eq!(pragma_i64(&writer, "temp_store"), 2);
    assert_eq!(pragma_i64(&writer, "foreign_keys"), 1);
    assert_eq!(pragma_i64(&writer, "cache_size"), -65_536);
    assert_eq!(pragma_i64(&writer, "mmap_size"), 268_435_456);

    let reader = options.open_reader().unwrap();
    assert_eq!(pragma_i64(&reader, "query_only"), 1);
    assert_eq!(pragma_i64(&reader, "synchronous"), 2);
    assert_eq!(pragma_i64(&reader, "temp_store"), 2);
    assert!(reader
        .execute("CREATE TABLE forbidden(id INTEGER)", [])
        .is_err());
}

#[test]
fn write_txn_acquires_the_writer_lock_immediately() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("immediate.sqlite");
    let options = DbOptions::new(&path);
    let mut first = options.open_writer().unwrap();
    let mut second = options.open_writer().unwrap();
    second
        .busy_timeout(Duration::from_millis(20))
        .expect("short test timeout");

    let tx = write_txn(&mut first).expect("begin immediate transaction");
    let blocked = second.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate);
    assert!(
        blocked.is_err(),
        "IMMEDIATE must reserve the single writer slot"
    );
    tx.rollback().unwrap();
}

#[test]
fn default_profile_does_not_apply_large_cache_or_mmap() {
    let dir = tempfile::tempdir().unwrap();
    let options = DbOptions::new(dir.path().join("defaults.sqlite"));
    let conn = options.open_writer().unwrap();

    assert_ne!(pragma_i64(&conn, "cache_size"), -65_536);
    assert_eq!(pragma_i64(&conn, "mmap_size"), 0);
    assert_eq!(pragma_i64(&conn, "synchronous"), 1);
}

#[test]
fn read_pool_caps_total_connections() {
    let dir = tempfile::tempdir().unwrap();
    let options = DbOptions::new(dir.path().join("pool.sqlite"));
    options.open_writer().unwrap();
    let pool = options.open_read_pool(1).unwrap();
    let first = pool.get().unwrap();
    let (tx, rx) = mpsc::channel();
    let waiting_pool = pool.clone();
    let waiter = std::thread::spawn(move || {
        let second = waiting_pool.get().unwrap();
        tx.send(()).unwrap();
        drop(second);
    });

    assert!(rx.recv_timeout(Duration::from_millis(150)).is_err());
    assert_eq!(pool.total_len(), 1);
    drop(first);
    rx.recv_timeout(Duration::from_secs(2)).unwrap();
    waiter.join().unwrap();
    assert_eq!(pool.idle_len(), 1);
}

#[test]
fn checkpoint_and_optimize_helpers_report_results() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("maintenance.sqlite");
    let options = DbOptions::new(&path);
    let conn = options.open_writer().unwrap();
    conn.execute_batch("CREATE TABLE item(id INTEGER); INSERT INTO item VALUES (1);")
        .unwrap();

    let passive = checkpoint_if_wal_larger_than(&conn, &path, 0)
        .unwrap()
        .expect("the write should create a WAL sidecar");
    assert_eq!(passive.busy, 0);
    let truncate = checkpoint_truncate(&conn).unwrap();
    assert_eq!(truncate.busy, 0);
    optimize_boundary(&conn).unwrap();
}

#[cfg(not(feature = "cipher"))]
#[test]
fn plain_build_rejects_a_key_instead_of_silently_ignoring_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("plain.sqlite");
    let mut options = DbOptions::new(&path);
    options.key = Some(zeroize::Zeroizing::new("YWJjZA==".to_string()));
    assert!(matches!(
        options.open_writer(),
        Err(rightkit_db::DbError::SqlcipherFeatureRequired)
    ));
    assert!(!path.exists());
}

#[cfg(feature = "cipher")]
#[test]
fn sqlcipher_key_is_applied_before_any_database_access() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cipher.sqlite");
    let mut options = DbOptions::new(&path);
    options.key = Some(zeroize::Zeroizing::new("YWJjZA==".to_string()));
    options.integrity_check_on_open = true;
    let conn = options.open_writer().unwrap();
    conn.execute("CREATE TABLE secret(value TEXT)", []).unwrap();
    conn.execute("INSERT INTO secret VALUES ('kept')", [])
        .unwrap();
    drop(conn);

    let reopened = options.open_reader().unwrap();
    let value: String = reopened
        .query_row("SELECT value FROM secret", [], |row| row.get(0))
        .unwrap();
    assert_eq!(value, "kept");

    options.key = Some(zeroize::Zeroizing::new("d3Jvbmc=".to_string()));
    assert!(options.open_reader().is_err());
}
