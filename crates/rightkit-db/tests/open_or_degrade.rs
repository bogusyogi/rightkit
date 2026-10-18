//! Open-or-degrade: a damaged file is quarantined and recreated when the policy allows it; an
//! in-memory store with a degraded flag is used only when explicitly acknowledged (default
//! refuse). A newer database stays an error.

use rightkit_db::{
    user_version, DbError, DbOptions, MigrateOptions, Migration, OpenOrDegrade, RecoveryPolicy,
};
use std::fs;
use std::path::{Path, PathBuf};

fn migrations() -> Vec<Migration> {
    vec![
        Migration::sql(1, "create", "CREATE TABLE t(v INTEGER);"),
        Migration::sql(2, "add_w", "ALTER TABLE t ADD COLUMN w INTEGER;"),
    ]
}

fn fresh(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().canonicalize().unwrap().join("app.sqlite")
}

fn allow_quarantine() -> RecoveryPolicy {
    RecoveryPolicy {
        quarantine_corrupt: true,
        ..RecoveryPolicy::default()
    }
}

fn open(path: &Path, policy: RecoveryPolicy) -> rightkit_db::Result<OpenOrDegrade> {
    DbOptions::new(path).open_writer_migrated_or_degrade(
        &migrations(),
        &MigrateOptions::default(),
        policy,
    )
}

#[test]
fn a_healthy_store_opens_without_degrading() {
    let dir = tempfile::tempdir().unwrap();
    let path = fresh(&dir);
    let outcome = open(&path, allow_quarantine()).unwrap();
    assert!(outcome.degraded.is_none());
    assert_eq!(outcome.report.to, 2);
    assert_eq!(user_version(&outcome.conn).unwrap(), 2);
}

#[test]
fn a_damaged_file_is_quarantined_and_recreated_when_the_policy_allows() {
    let dir = tempfile::tempdir().unwrap();
    let path = fresh(&dir);
    let damaged = vec![0x42u8; 8192];
    fs::write(&path, &damaged).unwrap();
    let outcome = open(&path, allow_quarantine()).unwrap();
    let degraded = outcome.degraded.expect("the damage is reported");
    assert!(!degraded.in_memory);
    assert!(!degraded.reason.is_empty());
    let moved = degraded
        .quarantined
        .expect("the damaged file was moved aside");
    assert_eq!(fs::read(&moved).unwrap(), damaged);
    assert!(moved
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with("app.sqlite.corrupt-"));
    assert_eq!(user_version(&outcome.conn).unwrap(), 2);
}

#[test]
fn a_damaged_file_is_left_alone_and_refused_by_default_without_memory_acknowledgement() {
    let dir = tempfile::tempdir().unwrap();
    let path = fresh(&dir);
    let damaged = vec![0x42u8; 8192];
    fs::write(&path, &damaged).unwrap();
    assert!(
        !RecoveryPolicy::default().allow_in_memory_fallback,
        "the default is Refuse"
    );
    assert!(
        open(&path, RecoveryPolicy::default()).is_err(),
        "no automatic in-memory degradation"
    );
    assert_eq!(
        fs::read(&path).unwrap(),
        damaged,
        "the file was not touched"
    );
}

#[test]
fn acknowledged_in_memory_fallback_degrades_visibly_and_leaves_the_file_alone() {
    let dir = tempfile::tempdir().unwrap();
    let path = fresh(&dir);
    let damaged = vec![0x42u8; 8192];
    fs::write(&path, &damaged).unwrap();
    let policy = RecoveryPolicy {
        allow_in_memory_fallback: true,
        ..RecoveryPolicy::default()
    };
    let outcome = open(&path, policy).unwrap();
    let degraded = outcome.degraded.expect("the degraded state is reported");
    assert!(degraded.in_memory);
    assert!(degraded.quarantined.is_none());
    assert_eq!(
        fs::read(&path).unwrap(),
        damaged,
        "the file was not touched"
    );
    assert_eq!(user_version(&outcome.conn).unwrap(), 2);
}

#[test]
fn a_newer_database_is_an_error_and_is_not_degraded() {
    let dir = tempfile::tempdir().unwrap();
    let path = fresh(&dir);
    let conn = DbOptions::new(&path).open_writer().unwrap();
    conn.execute_batch("PRAGMA user_version = 9;").unwrap();
    drop(conn);
    let error = match open(&path, allow_quarantine()) {
        Ok(_) => panic!("a newer database must not open"),
        Err(error) => error,
    };
    assert!(
        matches!(error, DbError::NewerDatabase { current: 9, max: 2 }),
        "{error:?}"
    );
}

#[test]
fn an_unfinished_quarantine_journal_keeps_the_store_degraded_instead_of_recreating_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = fresh(&dir);
    fs::write(
        path.with_file_name("app.sqlite.quarantine-journal"),
        b"{\"schema\":\"rightkit-db-quarantine.v1\"}\n",
    )
    .unwrap();
    assert!(
        open(&path, allow_quarantine()).is_err(),
        "unacknowledged in-memory fallback is refused"
    );
    let policy = RecoveryPolicy {
        allow_in_memory_fallback: true,
        ..allow_quarantine()
    };
    let outcome = open(&path, policy).unwrap();
    let degraded = outcome.degraded.expect("the split state is reported");
    assert!(degraded.in_memory);
    assert!(degraded.quarantine_incomplete);
    assert!(!path.exists(), "no replacement database was created");
}
