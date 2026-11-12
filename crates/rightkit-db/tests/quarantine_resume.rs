//! An interrupted quarantine is settled from its journal before the database opens. Each state a
//! crash can leave is produced from a real journaled quarantine, then reopened.
#![cfg(feature = "cipher")]

use rightkit_db::{
    quarantine_database_opts, resume_quarantine, DbOptions, MigrateOptions, QuarantineOptions,
    QuarantineResume, RecoveryPolicy,
};
use std::path::{Path, PathBuf};

fn journal_of(db: &Path) -> PathBuf {
    PathBuf::from(format!("{}.quarantine-journal", db.display()))
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    PathBuf::from(format!("{}{suffix}", path.display()))
}

struct Fixture {
    dir: PathBuf,
    db: PathBuf,
    dest: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A store with a main file and `-wal`/`-shm`, moved aside by a real journaled quarantine; the
/// journal stays (a marker is never removed by `quarantine_database_opts`), so the files can then
/// be put back into the state any crash point leaves.
fn quarantined(tag: &str) -> Fixture {
    let dir = std::env::temp_dir().join(format!("rk-db-resume-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("store.db");
    std::fs::write(&db, b"main").unwrap();
    std::fs::write(sidecar(&db, "-wal"), b"wal").unwrap();
    std::fs::write(sidecar(&db, "-shm"), b"shm").unwrap();
    let dest = quarantine_database_opts(
        &db,
        &QuarantineOptions {
            marker: Some(journal_of(&db)),
            ..QuarantineOptions::default()
        },
    )
    .unwrap();
    Fixture { dir, db, dest }
}

fn read(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap()
}

#[test]
fn no_journal_is_nothing_to_do() {
    let f = quarantined("none");
    std::fs::remove_file(journal_of(&f.db)).unwrap();
    assert_eq!(resume_quarantine(&f.db).unwrap(), QuarantineResume::None);
    assert_eq!(read(&f.dest), b"main");
}

#[test]
fn crash_after_the_journal_before_the_first_move_rolls_back() {
    let f = quarantined("before-move");
    for suffix in ["", "-wal", "-shm"] {
        std::fs::rename(sidecar(&f.dest, suffix), sidecar(&f.db, suffix)).unwrap();
    }
    assert_eq!(
        resume_quarantine(&f.db).unwrap(),
        QuarantineResume::RolledBack
    );
    assert_eq!(read(&f.db), b"main");
    assert_eq!(read(&sidecar(&f.db, "-wal")), b"wal");
    assert!(!journal_of(&f.db).exists());
    assert!(!f.dest.exists());
}

#[test]
fn crash_after_the_main_move_finishes_the_sidecars() {
    let f = quarantined("after-main");
    for suffix in ["-wal", "-shm"] {
        std::fs::rename(sidecar(&f.dest, suffix), sidecar(&f.db, suffix)).unwrap();
    }
    assert_eq!(
        resume_quarantine(&f.db).unwrap(),
        QuarantineResume::Completed {
            quarantined: f.dest.clone()
        }
    );
    assert!(!f.db.exists() && !sidecar(&f.db, "-wal").exists() && !sidecar(&f.db, "-shm").exists());
    assert_eq!(read(&f.dest), b"main");
    assert_eq!(read(&sidecar(&f.dest, "-wal")), b"wal");
    assert_eq!(read(&sidecar(&f.dest, "-shm")), b"shm");
    assert!(!journal_of(&f.db).exists());
    // Settled: asking again finds nothing.
    assert_eq!(resume_quarantine(&f.db).unwrap(), QuarantineResume::None);
}

#[test]
fn crash_between_the_last_move_and_the_journal_removal_completes() {
    let f = quarantined("before-journal-removal");
    assert!(journal_of(&f.db).exists());
    assert_eq!(
        resume_quarantine(&f.db).unwrap(),
        QuarantineResume::Completed {
            quarantined: f.dest.clone()
        }
    );
    assert_eq!(read(&f.dest), b"main");
    assert!(!journal_of(&f.db).exists());
}

#[test]
fn a_link_move_that_stopped_before_its_unlink_drops_only_the_original_name() {
    let f = quarantined("link");
    std::fs::hard_link(&f.dest, &f.db).unwrap();
    assert!(matches!(
        resume_quarantine(&f.db).unwrap(),
        QuarantineResume::Completed { .. }
    ));
    assert!(!f.db.exists());
    assert_eq!(read(&f.dest), b"main");
}

#[test]
fn unrelated_bytes_at_both_names_fail_closed_and_keep_everything() {
    let f = quarantined("both");
    std::fs::write(&f.db, b"someone else").unwrap();
    assert!(resume_quarantine(&f.db).is_err());
    assert_eq!(read(&f.db), b"someone else");
    assert_eq!(read(&f.dest), b"main");
    assert!(journal_of(&f.db).exists());
}

#[test]
fn an_unreadable_or_foreign_journal_fails_closed() {
    let f = quarantined("garbage");
    let journal = journal_of(&f.db);
    std::fs::write(&journal, b"not json").unwrap();
    assert!(resume_quarantine(&f.db).is_err());
    // A journal that names a file outside this store is never acted on.
    std::fs::write(
        &journal,
        format!(
            "{{\"schema\":\"rightkit-db-quarantine.v1\",\"original\":{:?},\"quarantined\":\"/etc/passwd\",\"unixMs\":1}}",
            f.db.to_string_lossy()
        ),
    )
    .unwrap();
    assert!(resume_quarantine(&f.db).is_err());
    assert_eq!(read(&f.dest), b"main");
    assert!(journal.exists());
}

#[test]
fn the_migrated_open_settles_an_interrupted_quarantine_first() {
    let f = quarantined("open");
    for suffix in ["-wal", "-shm"] {
        std::fs::rename(sidecar(&f.dest, suffix), sidecar(&f.db, suffix)).unwrap();
    }
    // The old `-wal` is live at the original name beside a missing main file: opening now would
    // replay it into a new database. The open settles the quarantine first instead.
    let opened = DbOptions::new(&f.db)
        .open_writer_migrated_or_degrade(&[], &MigrateOptions::default(), RecoveryPolicy::default())
        .unwrap();
    let degraded = opened
        .degraded
        .clone()
        .expect("reports the finished quarantine");
    assert_eq!(degraded.quarantined.as_deref(), Some(f.dest.as_path()));
    assert!(!degraded.in_memory && !degraded.quarantine_incomplete);
    assert_eq!(read(&sidecar(&f.dest, "-wal")), b"wal");
    assert_eq!(read(&f.dest), b"main");
    assert!(!journal_of(&f.db).exists());
    drop(opened);
}

#[test]
fn the_migrated_open_refuses_a_journal_it_cannot_settle() {
    let f = quarantined("open-refuse");
    std::fs::write(&f.db, b"someone else").unwrap();
    let result = DbOptions::new(&f.db).open_writer_migrated_or_degrade(
        &[],
        &MigrateOptions::default(),
        RecoveryPolicy::default(),
    );
    assert!(result.is_err());
    assert_eq!(read(&f.db), b"someone else");
    assert_eq!(read(&f.dest), b"main");
}
