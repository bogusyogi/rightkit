//! A failed open must not checkpoint or delete the `-wal`/`-shm` before quarantine moves them
//! aside (rightkit#932, ViewRight). Real SQLite files.

use rightkit_db::rusqlite::Connection;
use rightkit_db::{
    DbOptions, IntegrityCheck, MigrateOptions, Migration, RecoveryPolicy, UnreadablePolicy,
};
use std::path::{Path, PathBuf};

fn sidecar(p: &Path, suffix: &str) -> PathBuf {
    let mut n = p.as_os_str().to_os_string();
    n.push(suffix);
    PathBuf::from(n)
}

fn allow_quarantine() -> RecoveryPolicy {
    RecoveryPolicy {
        quarantine_corrupt: true,
        ..RecoveryPolicy::default()
    }
}

const GARBAGE_WAL: &[u8] = b"stale wal bytes that must be kept";

fn garbage_store(dir: &Path) -> PathBuf {
    let db = dir.canonicalize().unwrap().join("app.sqlite");
    std::fs::write(&db, vec![0x5a; 4096]).unwrap();
    std::fs::write(sidecar(&db, "-wal"), GARBAGE_WAL).unwrap();
    db
}

#[test]
fn garbage_main_and_wal_quarantine_keeps_the_wal_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let db = garbage_store(dir.path());
    let migrations = [Migration::sql(1, "create", "CREATE TABLE t(v INTEGER);")];
    let out = DbOptions::new(&db)
        .open_writer_migrated_or_degrade(
            &migrations,
            &MigrateOptions::default(),
            allow_quarantine(),
        )
        .unwrap();
    let moved = out
        .degraded
        .expect("degraded")
        .quarantined
        .expect("quarantined");
    assert_eq!(std::fs::read(&moved).unwrap(), vec![0x5a; 4096]);
    assert_eq!(
        std::fs::read(sidecar(&moved, "-wal")).unwrap(),
        GARBAGE_WAL,
        "quarantined -wal keeps its exact bytes"
    );
}

#[test]
fn garbage_main_and_wal_via_unreadable_keeps_the_wal_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let db = garbage_store(dir.path());
    let out = DbOptions::new(&db)
        .open_writer_recovering_unreadable(allow_quarantine(), UnreadablePolicy::Fail)
        .unwrap();
    let moved = out.quarantined.expect("quarantined");
    assert_eq!(std::fs::read(&moved).unwrap(), vec![0x5a; 4096]);
    assert_eq!(std::fs::read(sidecar(&moved, "-wal")).unwrap(), GARBAGE_WAL);
}

#[test]
fn valid_wal_store_with_corrupt_page_is_quarantined_byte_identical() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let src = root.join("src.sqlite");
    let w = Connection::open(&src).unwrap();
    w.query_row("PRAGMA journal_mode=WAL", [], |r| r.get::<_, String>(0))
        .unwrap();
    w.execute_batch(
        "PRAGMA wal_autocheckpoint=0; CREATE TABLE a(x);
         WITH RECURSIVE c(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM c WHERE i<5000)
         INSERT INTO a SELECT randomblob(200) FROM c;",
    )
    .unwrap();
    w.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
        .unwrap();
    // The last write lives only in the -wal.
    w.execute_batch("CREATE TABLE b(v); INSERT INTO b VALUES('LAST-USER-WRITE');")
        .unwrap();
    let db = root.join("app.sqlite");
    std::fs::copy(&src, &db).unwrap();
    std::fs::copy(sidecar(&src, "-wal"), sidecar(&db, "-wal")).unwrap();
    drop(w);

    let mut bytes = std::fs::read(&db).unwrap();
    let page = u16::from_be_bytes([bytes[16], bytes[17]]) as usize;
    for b in &mut bytes[9 * page..10 * page] {
        *b = 0x5a;
    }
    std::fs::write(&db, &bytes).unwrap();
    let main_before = std::fs::read(&db).unwrap();
    let wal_before = std::fs::read(sidecar(&db, "-wal")).unwrap();
    assert!(!wal_before.is_empty());

    let opts = DbOptions {
        integrity: IntegrityCheck::Quick,
        ..DbOptions::new(&db)
    };
    let out = opts.open_writer_recovering(allow_quarantine()).unwrap();
    let moved = out.quarantined.expect("quarantined");
    assert_eq!(
        std::fs::read(&moved).unwrap(),
        main_before,
        "main rewritten"
    );
    assert_eq!(
        std::fs::read(sidecar(&moved, "-wal")).unwrap(),
        wal_before,
        "-wal changed or lost"
    );
}

// Apple's system SQLite persists the -wal on close, so this only holds for a bundled SQLite.
#[cfg(any(
    feature = "bundled",
    feature = "sqlcipher",
    feature = "sqlcipher-system"
))]
#[test]
fn healthy_wal_store_still_checkpoints_and_removes_the_wal_on_close() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().canonicalize().unwrap().join("app.sqlite");
    let opts = DbOptions::new(&db);
    let conn = opts.open_writer().unwrap();
    conn.execute_batch("CREATE TABLE t(v TEXT); INSERT INTO t VALUES('kept');")
        .unwrap();
    drop(conn);
    assert!(
        !sidecar(&db, "-wal").exists(),
        "a normal close checkpoints and removes the -wal"
    );
    let conn = opts.open_writer().unwrap();
    let v: String = conn.query_row("SELECT v FROM t", [], |r| r.get(0)).unwrap();
    assert_eq!(v, "kept");
}

#[test]
fn corruption_first_seen_during_migration_keeps_the_wal_and_the_main_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let src = root.join("src.sqlite");
    let writer = Connection::open(&src).unwrap();
    writer
        .query_row("PRAGMA journal_mode=WAL", [], |r| r.get::<_, String>(0))
        .unwrap();
    writer
        .execute_batch(
            "PRAGMA wal_autocheckpoint=0; CREATE TABLE a(x);
             WITH RECURSIVE c(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM c WHERE i<5000)
             INSERT INTO a SELECT randomblob(200) FROM c;",
        )
        .unwrap();
    writer
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
        .unwrap();
    writer
        .execute_batch("CREATE TABLE b(v); INSERT INTO b VALUES('LAST-USER-WRITE');")
        .unwrap();
    let db = root.join("app.sqlite");
    std::fs::copy(&src, &db).unwrap();
    std::fs::copy(sidecar(&src, "-wal"), sidecar(&db, "-wal")).unwrap();
    drop(writer);
    let mut bytes = std::fs::read(&db).unwrap();
    let page = u16::from_be_bytes([bytes[16], bytes[17]]) as usize;
    for b in &mut bytes[9 * page..10 * page] {
        *b = 0x5a;
    }
    std::fs::write(&db, &bytes).unwrap();
    let main_before = std::fs::read(&db).unwrap();
    let wal_before = std::fs::read(sidecar(&db, "-wal")).unwrap();

    // Integrity check off and no optimize: `open_at` succeeds and the damage first shows in the migration.
    let migrations = [Migration::sql(
        1,
        "idx",
        "CREATE TABLE IF NOT EXISTS a(x); CREATE INDEX ia ON a(x);",
    )];
    let out = DbOptions::new(&db)
        .without_optimize_on_open()
        .open_writer_migrated_or_degrade(
            &migrations,
            &MigrateOptions::default(),
            allow_quarantine(),
        )
        .unwrap();
    let degraded = out.degraded.expect("degraded");
    let moved = degraded.quarantined.expect("quarantined");
    assert_eq!(
        std::fs::read(&moved).unwrap(),
        main_before,
        "quarantined main was rewritten"
    );
    assert_eq!(
        std::fs::read(sidecar(&moved, "-wal")).ok().as_deref(),
        Some(&wal_before[..]),
        "quarantined -wal lost or changed"
    );
}
