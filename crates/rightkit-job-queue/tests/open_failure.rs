//! `JobError::open_failure`: a typed classification of database failures, so a consumer stops
//! matching error text. The string `code` stays the wire identifier.

use std::path::PathBuf;

use rightkit_job_queue::db::{
    DbError, DbOptions, RecoveryPolicy, StorageProblem, UnreadableCause, UnreadablePolicy,
};
use rightkit_job_queue::{ExtColumn, JobError, JobStore, OpenFailure, StoreConfig, DB_UNREADABLE};

/// A file that is not a database.
fn garbage_at(dir: &std::path::Path) -> (PathBuf, Vec<u8>) {
    let path = dir.join("j.db");
    let garbage: Vec<u8> = (0..8192u32).map(|i| (i % 251) as u8 ^ 0x5a).collect();
    std::fs::write(&path, &garbage).unwrap();
    (path, garbage)
}

#[test]
fn every_open_classifies_a_corrupt_file_without_reading_text() {
    let dir = tempfile::tempdir().unwrap();
    let (path, garbage) = garbage_at(dir.path());
    let options = DbOptions::new(path.clone());

    let e = JobStore::open(&path).err().expect("not a database");
    assert_eq!(e.open_failure(), Some(OpenFailure::Corrupt));
    assert_eq!(e.code, "db", "the wire code is unchanged");

    let e = JobStore::open_with(&options).err().expect("not a database");
    assert_eq!(e.open_failure(), Some(OpenFailure::Corrupt));

    let e = JobStore::open_with_config(&options, StoreConfig::new())
        .err()
        .expect("not a database");
    assert_eq!(e.open_failure(), Some(OpenFailure::Corrupt));

    // A recovery policy that does not allow quarantine leaves the file and says why.
    let e = JobStore::open_recovering(&options, StoreConfig::new(), RecoveryPolicy::default())
        .err()
        .expect("not a database");
    assert_eq!(e.open_failure(), Some(OpenFailure::Corrupt));

    let e = JobStore::open_recovering_unreadable(
        &options,
        StoreConfig::new(),
        RecoveryPolicy::default(),
        UnreadablePolicy::default(),
    )
    .err()
    .expect("not a database");
    assert_eq!(e.open_failure(), Some(OpenFailure::Corrupt));
    assert_eq!(e.code, "db");
    assert_eq!(std::fs::read(&path).unwrap(), garbage, "nothing was moved");
}

#[test]
fn a_file_that_cannot_be_read_is_not_corrupt() {
    fn unreadable(cause: UnreadableCause) -> JobError {
        JobError::from(DbError::Unreadable {
            path: PathBuf::from("/data/jobs.sqlite"),
            cause,
            consecutive_failures: 1,
            quarantine_after: Some(3),
            source: Box::new(DbError::InvalidDatabasePath),
        })
    }
    let e = unreadable(UnreadableCause::PermissionDenied);
    assert_eq!(e.code, DB_UNREADABLE);
    assert_eq!(
        e.open_failure(),
        Some(OpenFailure::Unreadable(UnreadableCause::PermissionDenied))
    );
    assert_eq!(
        unreadable(UnreadableCause::IoReadFailure).open_failure(),
        Some(OpenFailure::Unreadable(UnreadableCause::IoReadFailure))
    );
}

#[test]
fn unavailable_storage_is_told_apart_from_a_bad_file() {
    let e = JobError::from(DbError::Unreadable {
        path: PathBuf::from("/data/jobs.sqlite"),
        cause: UnreadableCause::StorageUnavailable(StorageProblem::DiskFull),
        consecutive_failures: 0,
        quarantine_after: None,
        source: Box::new(DbError::InvalidDatabasePath),
    });
    assert_eq!(e.code, DB_UNREADABLE);
    assert_eq!(
        e.open_failure(),
        Some(OpenFailure::StorageUnavailable(StorageProblem::DiskFull))
    );
    assert_eq!(
        JobError::from(DbError::ReadPoolClosed).open_failure(),
        Some(OpenFailure::Other)
    );
}

#[test]
fn errors_that_never_touched_the_database_carry_no_classification() {
    let dir = tempfile::tempdir().unwrap();
    // A refused configuration is not a database failure.
    let cfg = StoreConfig::new().column(ExtColumn::text("Not A Valid Name"));
    let e = JobStore::open_configured(dir.path().join("j.db"), cfg)
        .err()
        .expect("bad column name");
    assert_eq!(e.code, "config");
    assert_eq!(e.open_failure(), None);
    assert_eq!(JobError::bad_input("x").open_failure(), None);
}

#[test]
fn the_classification_does_not_change_equality() {
    // Two errors with the same code and message are equal, whether or not one was classified.
    let classified = JobError::from(DbError::ReadPoolClosed);
    let plain = JobError::new(classified.code.clone(), classified.message.clone());
    assert_eq!(classified, plain);
}
