//! The `rightkit-db` types in rightkit-job-queue' public API are reachable through
//! `rightkit_job_queue::db`. This file names no crate but `rightkit_job_queue` (plus std, serde_json and
//! tempfile), so everything here compiles for a consumer that has no `rightkit-db` dependency
//! of its own.

use std::path::PathBuf;
use std::sync::Arc;

use rightkit_job_queue::db::{
    CacheProfile, ConnInit, DbError, DbOptions, DurabilityProfile, IntegrityCheck, JournalSelect,
    QuarantineStamp, RecoveryAction, RecoveryPolicy, SidecarPolicy, StorageProblem,
    UnreadableCause, UnreadablePolicy,
};
use rightkit_job_queue::rusqlite::Connection;
use rightkit_job_queue::{JobError, JobStore, NewJob, StoreConfig, DB_UNREADABLE};
use serde_json::json;

fn new_job() -> NewJob {
    NewJob {
        request_id: None,
        kind: "scan".into(),
        request: json!({}),
    }
}

/// Every option type the store's open functions are built from.
fn options_at(path: PathBuf) -> DbOptions {
    let init: ConnInit = Arc::new(|_c: &Connection| Ok(()));
    let mut options = DbOptions::new(path);
    options.durability = DurabilityProfile::Normal;
    options.profile = CacheProfile::Default;
    options.journal = JournalSelect::WalWithFallback;
    options.integrity = IntegrityCheck::Off;
    options.init = Some(init);
    options
}

#[test]
fn a_store_opens_from_rightkit_jobs_paths_alone() {
    let dir = tempfile::tempdir().unwrap();
    let options = options_at(dir.path().join("j.db"));

    let store = JobStore::open_with_config(&options, StoreConfig::new()).unwrap();
    let id = store.submit(&new_job()).unwrap().0.job_id;
    drop(store);

    let policy = RecoveryPolicy {
        quarantine_corrupt: true,
        quarantine_stamp: QuarantineStamp::Seconds,
        sidecars: SidecarPolicy::RenameSidecars,
        ..RecoveryPolicy::default()
    };
    let (store, moved) = JobStore::open_recovering(&options, StoreConfig::new(), policy).unwrap();
    assert!(moved.is_none());
    assert!(store.get(&id).is_ok());
    drop(store);

    let (store, recovery) = JobStore::open_recovering_unreadable(
        &options,
        StoreConfig::new(),
        policy,
        UnreadablePolicy::QuarantineAfter { attempts: 3 },
    )
    .unwrap();
    assert_eq!(recovery.quarantined, None);
    assert_eq!(recovery.action, RecoveryAction::OpenedNormally);
    assert!(store.get(&id).is_ok());
}

#[test]
fn a_corrupt_database_is_set_aside_by_policy_and_kept_otherwise() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("j.db");
    let garbage: Vec<u8> = (0..8192u32).map(|i| (i % 251) as u8 ^ 0x5a).collect();
    std::fs::write(&path, &garbage).unwrap();
    let options = options_at(path.clone());

    // The default policies move nothing and fail with the plain database code.
    let e = JobStore::open_recovering_unreadable(
        &options,
        StoreConfig::new(),
        RecoveryPolicy::default(),
        UnreadablePolicy::default(),
    )
    .err()
    .expect("a corrupt file is not opened");
    assert_eq!(e.code, "db");
    assert_eq!(std::fs::read(&path).unwrap(), garbage);

    let policy = RecoveryPolicy {
        quarantine_corrupt: true,
        ..RecoveryPolicy::default()
    };
    let (store, recovery) = JobStore::open_recovering_unreadable(
        &options,
        StoreConfig::new(),
        policy,
        UnreadablePolicy::default(),
    )
    .unwrap();
    assert_eq!(recovery.action, RecoveryAction::QuarantinedCorrupt);
    let moved = recovery.quarantined.expect("the old file was moved aside");
    assert_eq!(
        std::fs::read(&moved).unwrap(),
        garbage,
        "original bytes kept"
    );
    assert!(store.submit(&new_job()).is_ok());
}

#[test]
fn the_database_error_type_converts_with_its_codes() {
    fn code(e: DbError) -> String {
        JobError::from(e).code
    }
    assert_eq!(code(DbError::ReadPoolClosed), "db");
    let unreadable = DbError::Unreadable {
        path: PathBuf::from("/data/jobs.sqlite"),
        cause: UnreadableCause::StorageUnavailable(StorageProblem::DiskFull),
        consecutive_failures: 0,
        quarantine_after: None,
        source: Box::new(DbError::InvalidDatabasePath),
    };
    assert_eq!(code(unreadable), DB_UNREADABLE);
}

#[test]
fn the_root_re_export_is_the_same_type() {
    fn takes(_: RecoveryPolicy) {}
    takes(rightkit_job_queue::RecoveryPolicy::default());
}
