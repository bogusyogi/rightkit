//! Opt-in recovery of a database that cannot be read: injected open errors through
//! `open_writer_recovering_via`, real SQLite files for everything else.

use rightkit_db::rusqlite::{self, ffi, Connection};
use rightkit_db::{
    classify_open_failure, unreadable_counter_path, DbError, DbOptions, RecoveryAction,
    RecoveryPolicy, StorageProblem, UnreadableCause, UnreadablePolicy,
};
use std::path::{Path, PathBuf};

const PERM: i32 = 3;
const BUSY: i32 = 5;
const READONLY: i32 = 8;
const FULL: i32 = 13;
const CANTOPEN: i32 = 14;
const IOERR_READ: i32 = 266;
const IOERR_SHORT_READ: i32 = 522;
const IOERR_LOCK: i32 = 3850;
const IOERR_WRITE: i32 = 778;

fn sqlite_err(code: i32) -> DbError {
    DbError::Sqlite(rusqlite::Error::SqliteFailure(ffi::Error::new(code), None))
}

fn healthy(dir: &Path) -> PathBuf {
    let db = dir.join("app.sqlite");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "PRAGMA journal_mode=DELETE; CREATE TABLE t(x TEXT); INSERT INTO t VALUES ('kept');",
    )
    .unwrap();
    db
}

/// Every call fails with `code` until `fail_times` calls have failed; then it opens for real.
fn flaky<'a>(
    opts: &'a DbOptions,
    code: i32,
    fail_times: usize,
    calls: &'a mut usize,
) -> impl FnMut() -> rightkit_db::Result<Connection> + 'a {
    move || {
        *calls += 1;
        if *calls <= fail_times {
            Err(sqlite_err(code))
        } else {
            opts.open_writer()
        }
    }
}

fn quarantine_after(n: u32) -> UnreadablePolicy {
    UnreadablePolicy::QuarantineAfter { attempts: n }
}

fn unreadable(e: DbError) -> (UnreadableCause, u32, Option<u32>) {
    match e {
        DbError::Unreadable {
            cause,
            consecutive_failures,
            quarantine_after,
            ..
        } => (cause, consecutive_failures, quarantine_after),
        other => panic!("expected Unreadable, got {other:?}"),
    }
}

fn corrupt_siblings(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.to_string_lossy().contains(".corrupt-"))
        .collect()
}

#[test]
fn default_policy_fails_with_a_typed_cause_and_touches_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let db = healthy(dir.path());
    let before = std::fs::read(&db).unwrap();
    let opts = DbOptions::new(&db);
    for code in [PERM, IOERR_READ, IOERR_SHORT_READ] {
        let e = opts
            .open_writer_recovering_via(
                RecoveryPolicy::default(),
                UnreadablePolicy::default(),
                || Err(sqlite_err(code)),
            )
            .unwrap_err();
        let (cause, failures, after) = unreadable(e);
        assert!(cause.counts_toward_quarantine());
        assert_eq!((failures, after), (0, None), "Fail never counts");
    }
    assert_eq!(std::fs::read(&db).unwrap(), before);
    assert!(!unreadable_counter_path(&db).exists());
    assert!(corrupt_siblings(dir.path()).is_empty());
}

#[test]
fn quarantine_happens_only_at_the_configured_count_and_preserves_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let db = healthy(dir.path());
    let original = std::fs::read(&db).unwrap();
    let opts = DbOptions::new(&db);
    for expected in [1u32, 2] {
        let e = opts
            .open_writer_recovering_via(RecoveryPolicy::default(), quarantine_after(3), || {
                Err(sqlite_err(PERM))
            })
            .unwrap_err();
        let (cause, failures, after) = unreadable(e);
        assert_eq!(cause, UnreadableCause::PermissionDenied);
        assert_eq!((failures, after), (expected, Some(3)));
        assert_eq!(std::fs::read(&db).unwrap(), original, "not moved yet");
    }
    let mut calls = 0;
    let out = opts
        .open_writer_recovering_via(
            RecoveryPolicy::default(),
            quarantine_after(3),
            flaky(&opts, PERM, 1, &mut calls),
        )
        .unwrap();
    assert_eq!(
        out.action,
        RecoveryAction::QuarantinedUnreadable {
            cause: UnreadableCause::PermissionDenied,
            consecutive_failures: 3
        }
    );
    let moved = out.quarantined.expect("the old file is reported");
    assert_eq!(
        std::fs::read(&moved).unwrap(),
        original,
        "renamed, not deleted"
    );
    let fresh: i64 = out
        .conn
        .query_row("SELECT COUNT(*) FROM sqlite_master", [], |r| r.get(0))
        .unwrap();
    assert_eq!(fresh, 0, "a fresh empty database took its place");
    assert!(!unreadable_counter_path(&db).exists(), "count cleared");
}

#[test]
fn a_successful_open_resets_the_consecutive_count() {
    let dir = tempfile::tempdir().unwrap();
    let db = healthy(dir.path());
    let opts = DbOptions::new(&db);
    for _ in 0..2 {
        let _ =
            opts.open_writer_recovering_via(RecoveryPolicy::default(), quarantine_after(3), || {
                Err(sqlite_err(IOERR_READ))
            });
    }
    assert!(unreadable_counter_path(&db).exists());
    let ok = opts
        .open_writer_recovering_unreadable(RecoveryPolicy::default(), quarantine_after(3))
        .unwrap();
    assert_eq!(ok.action, RecoveryAction::OpenedNormally);
    assert!(ok.quarantined.is_none());
    assert!(!unreadable_counter_path(&db).exists());
    drop(ok);
    let e = opts
        .open_writer_recovering_via(RecoveryPolicy::default(), quarantine_after(3), || {
            Err(sqlite_err(IOERR_READ))
        })
        .unwrap_err();
    assert_eq!(unreadable(e).1, 1, "counting restarted");
}

#[test]
fn storage_unavailable_never_quarantines_however_often_it_repeats() {
    let dir = tempfile::tempdir().unwrap();
    let db = healthy(dir.path());
    let before = std::fs::read(&db).unwrap();
    let opts = DbOptions::new(&db);
    let cases = [
        (FULL, StorageProblem::DiskFull),
        (READONLY, StorageProblem::ReadOnlyFilesystem),
        (BUSY, StorageProblem::LockedOrBusy),
        (IOERR_LOCK, StorageProblem::LockedOrBusy),
        (IOERR_WRITE, StorageProblem::Other),
    ];
    for (code, problem) in cases {
        for _ in 0..5 {
            let e = opts
                .open_writer_recovering_via(RecoveryPolicy::default(), quarantine_after(1), || {
                    Err(sqlite_err(code))
                })
                .unwrap_err();
            let (cause, failures, after) = unreadable(e);
            assert_eq!(cause, UnreadableCause::StorageUnavailable(problem));
            assert_eq!((failures, after), (0, None));
        }
    }
    assert_eq!(std::fs::read(&db).unwrap(), before);
    assert!(!unreadable_counter_path(&db).exists());
    assert!(corrupt_siblings(dir.path()).is_empty());
}

#[test]
fn cannot_open_on_a_missing_file_is_storage_not_a_read_failure() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("absent.sqlite");
    let opts = DbOptions::new(&db);
    let e = opts
        .open_writer_recovering_via(RecoveryPolicy::default(), quarantine_after(1), || {
            Err(sqlite_err(CANTOPEN))
        })
        .unwrap_err();
    assert_eq!(
        unreadable(e).0,
        UnreadableCause::StorageUnavailable(StorageProblem::FileMissing)
    );
}

#[cfg(unix)]
#[test]
fn cannot_open_with_a_file_we_may_not_read_is_permission_denied() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let db = healthy(dir.path());
    let original = std::fs::read(&db).unwrap();
    std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::File::open(&db).is_ok() {
        // Running as root: permissions do not apply, so the case cannot be built.
        std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o600)).unwrap();
        return;
    }
    let opts = DbOptions::new(&db);
    let cause = classify_open_failure(&sqlite_err(CANTOPEN), &db);
    assert_eq!(cause, Some(UnreadableCause::PermissionDenied));
    // One attempt allowed: the unreadable file is renamed (directory permissions allow it).
    let out = opts
        .open_writer_recovering_via(RecoveryPolicy::default(), quarantine_after(1), {
            let mut first = true;
            let o = opts.clone();
            move || {
                if std::mem::take(&mut first) {
                    Err(sqlite_err(CANTOPEN))
                } else {
                    o.open_writer()
                }
            }
        })
        .unwrap();
    let moved = out.quarantined.unwrap();
    std::fs::set_permissions(&moved, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(std::fs::read(&moved).unwrap(), original);
}

#[test]
fn corruption_still_follows_the_recovery_policy() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("app.sqlite");
    std::fs::write(&db, b"this is not a database, not even close to one").unwrap();
    let opts = DbOptions::new(&db);

    // Not allowed by the policy: the original SQLite error, as before.
    let e = opts
        .open_writer_recovering_unreadable(RecoveryPolicy::default(), quarantine_after(1))
        .unwrap_err();
    assert!(matches!(e, DbError::Sqlite(_)), "got {e:?}");
    assert!(db.exists());

    let policy = RecoveryPolicy {
        quarantine_corrupt: true,
        ..RecoveryPolicy::default()
    };
    let out = opts
        .open_writer_recovering_unreadable(policy, UnreadablePolicy::Fail)
        .unwrap();
    assert_eq!(out.action, RecoveryAction::QuarantinedCorrupt);
    assert!(out.quarantined.unwrap().exists());
}

#[test]
fn unrelated_errors_pass_through_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let db = healthy(dir.path());
    let opts = DbOptions::new(&db);
    let e = opts
        .open_writer_recovering_via(RecoveryPolicy::default(), quarantine_after(1), || {
            Err(DbError::NoKeyOpened)
        })
        .unwrap_err();
    assert!(matches!(e, DbError::NoKeyOpened));
    assert!(!unreadable_counter_path(&db).exists());
}
