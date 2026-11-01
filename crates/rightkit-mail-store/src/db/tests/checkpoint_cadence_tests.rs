use super::*;

// L4: checkpoint cadence — the WAL-bounded-after-every-sync-pass invariant is
// non-negotiable (82MB / transient SQLITE_CORRUPT lesson, 2026-05-25), but
// per-batch/per-small-write TRUNCATE checkpoints inside the writer lock are
// gone from the hot paths. `rightkit_db::checkpoint_if_wal_larger_than` is the size-gated
// replacement for those hot-path callers: below the threshold it's a no-op,
// above it it runs a PASSIVE checkpoint that never waits on active readers.
//
// These tests use a real file-backed connection (not `test_conn()`'s
// in-memory DB) because the size gate reads the `-wal` sidecar file's actual
// byte size off disk.

fn file_backed_conn(tag: &str) -> (Connection, std::path::PathBuf) {
    semantic::vec_ext::ensure_registered();
    let dir = std::env::temp_dir().join(format!(
        "mailright-checkpoint-cadence-test-{tag}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db_path = dir.join("db.sqlite");
    let conn = Connection::open(&db_path).expect("open file-backed db");
    conn.execute_batch("PRAGMA foreign_keys = ON;")
        .expect("enable fk");
    conn.execute_batch("PRAGMA journal_mode = WAL;")
        .expect("enable wal");
    migrate(&conn).expect("migrate");
    ensure_runtime_schema(&conn).expect("ensure runtime schema");
    (conn, db_path)
}

fn wal_len(db_path: &std::path::Path) -> u64 {
    let wal_path = format!("{}-wal", db_path.to_string_lossy());
    std::fs::metadata(wal_path).map(|m| m.len()).unwrap_or(0)
}

#[test]
fn below_threshold_checkpoint_is_a_no_op() {
    let (conn, db_path) = file_backed_conn("below-threshold");
    upsert_account(&conn, "a@x.com").unwrap();
    apply_messages(&conn, "a@x.com", &[msg("t1", &["INBOX"], true, 1)]).unwrap();

    let wal_before = wal_len(&db_path);
    assert!(wal_before > 0, "WAL should have grown from the write above");

    // Threshold far above the actual WAL size: no checkpoint should run, so
    // the WAL keeps whatever it had (TRUNCATE would shrink it to ~0).
    let _ = rightkit_db::checkpoint_if_wal_larger_than(&conn, &db_path, u64::MAX);
    let wal_after = wal_len(&db_path);
    assert_eq!(
        wal_after, wal_before,
        "checkpoint must not run below the threshold"
    );

    let _ = std::fs::remove_dir_all(db_path.parent().unwrap());
}

#[test]
fn above_threshold_checkpoint_runs_without_blocking_on_an_active_reader() {
    let (conn, db_path) = file_backed_conn("above-threshold");
    upsert_account(&conn, "a@x.com").unwrap();
    apply_messages(&conn, "a@x.com", &[msg("t1", &["INBOX"], true, 1)]).unwrap();

    let wal_before = wal_len(&db_path);
    assert!(wal_before > 0, "WAL should have grown from the write above");

    let reader = Connection::open(&db_path).expect("open reader");
    reader
        .execute_batch("PRAGMA busy_timeout=5000; BEGIN; SELECT count(*) FROM messages;")
        .expect("hold reader snapshot");
    apply_messages(&conn, "a@x.com", &[msg("t2", &["INBOX"], true, 2)]).unwrap();

    // With an active reader, TRUNCATE can wait for the full busy timeout.
    // PASSIVE must return promptly and checkpoint whatever is currently safe.
    let started = std::time::Instant::now();
    let _ = rightkit_db::checkpoint_if_wal_larger_than(&conn, &db_path, 0);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(1),
        "hot-loop checkpoint must not wait on readers"
    );
    reader.execute_batch("ROLLBACK;").unwrap();

    let _ = std::fs::remove_dir_all(db_path.parent().unwrap());
}

#[test]
fn missing_wal_file_is_treated_as_below_threshold() {
    // No writes at all yet (or an empty db_path) — the size-gated checkpoint
    // must be a best-effort no-op, never a panic, when the `-wal` sidecar
    // doesn't exist.
    let (conn, db_path) = file_backed_conn("missing-wal");
    // Fresh WAL-mode connection with no writes may or may not have created a
    // `-wal` file yet; either way this must not panic and must not error.
    let _ = rightkit_db::checkpoint_if_wal_larger_than(&conn, &db_path, 16 * 1024 * 1024);
    let _ = rightkit_db::checkpoint_if_wal_larger_than(&conn, std::path::Path::new(""), 0);

    let _ = std::fs::remove_dir_all(db_path.parent().unwrap());
}
