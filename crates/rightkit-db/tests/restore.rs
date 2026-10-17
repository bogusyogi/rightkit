//! Restore transaction: the previous store is kept, put back on failure, and a half-done
//! quarantine is recovered from the journal.

use rightkit_db::{
    recover_interrupted_restore, restore_database, DbError, DbOptions, InterruptedRestore,
    RestoreOptions,
};
use std::fs;
use std::path::{Path, PathBuf};

fn lock() -> rightkit_fs::LockOptions {
    rightkit_fs::LockOptions::default()
}

fn dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "rightkit-db-restore-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// A real database with one `marks` row holding `value`, checkpointed into the main file.
fn make_db(path: &Path, value: &str) {
    let conn = DbOptions::new(path).open_writer().unwrap();
    conn.execute_batch("CREATE TABLE IF NOT EXISTS marks(v TEXT NOT NULL);")
        .unwrap();
    conn.execute("DELETE FROM marks", []).unwrap();
    conn.execute("INSERT INTO marks(v) VALUES (?1)", [value])
        .unwrap();
    rightkit_db::checkpoint_truncate(&conn).unwrap();
}

fn read_mark(path: &Path) -> String {
    let conn = DbOptions::new(path).open_read_only().unwrap();
    conn.query_row("SELECT v FROM marks", [], |r| r.get(0))
        .unwrap()
}

fn kept(dir: &Path, infix: &str) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            let n = p.file_name().unwrap().to_string_lossy();
            n.contains(infix)
                && !n.ends_with("-wal")
                && !n.ends_with("-shm")
                && !n.ends_with("-journal")
        })
        .collect();
    found.sort();
    found
}

#[test]
fn success_keeps_the_previous_store_and_returns_its_path() {
    let d = dir("ok");
    let live = d.join("app.sqlite");
    let replacement = d.join("incoming.sqlite");
    make_db(&live, "old");
    make_db(&replacement, "new");
    let receipt = restore_database(
        &DbOptions::new(&live),
        &replacement,
        &RestoreOptions::default(),
        &lock(),
    )
    .unwrap();
    assert_eq!(read_mark(&live), "new");
    let previous = receipt.previous.expect("a store existed before");
    assert!(previous
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with("app.sqlite.pre-restore-"));
    assert_eq!(read_mark(&previous), "old");
    assert_eq!(kept(&d, ".pre-restore-"), vec![previous]);
    assert!(!d.join("app.sqlite.restore-journal").exists());
    assert_eq!(
        read_mark(&replacement),
        "new",
        "the replacement file is not consumed"
    );
}

#[test]
fn restoring_into_an_empty_place_has_no_previous() {
    let d = dir("fresh");
    let live = d.join("app.sqlite");
    let replacement = d.join("incoming.sqlite");
    make_db(&replacement, "new");
    let receipt = restore_database(
        &DbOptions::new(&live),
        &replacement,
        &RestoreOptions::default(),
        &lock(),
    )
    .unwrap();
    assert_eq!(receipt.previous, None);
    assert_eq!(read_mark(&live), "new");
    assert!(kept(&d, ".pre-restore-").is_empty());
}

#[test]
fn a_failed_check_puts_the_previous_store_back_and_keeps_the_bad_copy() {
    let d = dir("verify");
    let live = d.join("app.sqlite");
    let replacement = d.join("incoming.sqlite");
    make_db(&live, "old");
    make_db(&replacement, "new");
    let verify = |_: &DbOptions| -> std::result::Result<(), String> {
        Err("row count mismatch".to_string())
    };
    let options = RestoreOptions {
        verify: Some(&verify),
        ..RestoreOptions::default()
    };
    let error =
        restore_database(&DbOptions::new(&live), &replacement, &options, &lock()).unwrap_err();
    let DbError::RestoreFailed {
        message,
        previous,
        failed_copy,
        rolled_back,
    } = error
    else {
        panic!("expected RestoreFailed, got {error:?}");
    };
    assert!(rolled_back);
    assert!(message.contains("row count mismatch"), "{message}");
    assert_eq!(
        previous, None,
        "the previous store is at the live path again"
    );
    assert_eq!(read_mark(&live), "old");
    let bad = failed_copy.expect("the restored copy is kept");
    assert!(bad
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with("app.sqlite.restore-failed-"));
    assert_eq!(read_mark(&bad), "new");
    assert!(kept(&d, ".pre-restore-").is_empty());
    assert!(!d.join("app.sqlite.restore-journal").exists());
}

#[test]
fn a_damaged_replacement_is_refused_before_anything_moves() {
    let d = dir("damaged");
    let live = d.join("app.sqlite");
    let replacement = d.join("incoming.sqlite");
    make_db(&live, "old");
    fs::write(&replacement, vec![0xAB; 8192]).unwrap();
    let error = restore_database(
        &DbOptions::new(&live),
        &replacement,
        &RestoreOptions::default(),
        &lock(),
    )
    .unwrap_err();
    assert!(
        !matches!(error, DbError::RestoreFailed { .. }),
        "refused before the transaction began: {error:?}"
    );
    assert_eq!(read_mark(&live), "old");
    assert!(kept(&d, ".pre-restore-").is_empty());
    assert!(!d.join("app.sqlite.restore-journal").exists());
}

#[test]
fn restoring_the_live_file_onto_itself_is_refused() {
    let d = dir("self");
    let live = d.join("app.sqlite");
    make_db(&live, "old");
    let error = restore_database(
        &DbOptions::new(&live),
        &live,
        &RestoreOptions::default(),
        &lock(),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        DbError::RestoreFailed {
            rolled_back: true,
            ..
        }
    ));
    assert_eq!(read_mark(&live), "old");
}

fn journal(d: &Path, state: &str, previous: &str) {
    fs::write(
        d.join("app.sqlite.restore-journal"),
        format!("rightkit-db-restore.v1\nstate={state}\nprevious={previous}\n"),
    )
    .unwrap();
}

#[test]
fn recovery_puts_back_a_store_whose_quarantine_stopped_part_way() {
    let d = dir("partial");
    let live = d.join("app.sqlite");
    // The crash happened after the main file moved and before its -wal did.
    fs::write(d.join("app.sqlite.pre-restore-5"), b"main").unwrap();
    fs::write(d.join("app.sqlite-wal"), b"wal").unwrap();
    journal(&d, "quarantining", "app.sqlite.pre-restore-5");
    let outcome = recover_interrupted_restore(&DbOptions::new(&live), &lock()).unwrap();
    assert_eq!(
        outcome,
        Some(InterruptedRestore::PutBack {
            from: d.join("app.sqlite.pre-restore-5")
        })
    );
    assert_eq!(fs::read(&live).unwrap(), b"main");
    assert_eq!(fs::read(d.join("app.sqlite-wal")).unwrap(), b"wal");
    assert!(!d.join("app.sqlite.pre-restore-5").exists());
    assert!(!d.join("app.sqlite.restore-journal").exists());
}

#[test]
fn recovery_moves_back_sidecars_that_already_moved() {
    let d = dir("moved");
    let live = d.join("app.sqlite");
    for (suffix, body) in [("", "main"), ("-wal", "wal"), ("-shm", "shm")] {
        fs::write(d.join(format!("app.sqlite.pre-restore-7{suffix}")), body).unwrap();
    }
    journal(&d, "placed", "app.sqlite.pre-restore-7");
    recover_interrupted_restore(&DbOptions::new(&live), &lock()).unwrap();
    assert_eq!(fs::read(&live).unwrap(), b"main");
    assert_eq!(fs::read(d.join("app.sqlite-wal")).unwrap(), b"wal");
    assert_eq!(fs::read(d.join("app.sqlite-shm")).unwrap(), b"shm");
    assert!(kept(&d, ".pre-restore-").is_empty());
}

#[test]
fn recovery_keeps_a_placed_replacement_and_the_kept_previous() {
    let d = dir("placed");
    let live = d.join("app.sqlite");
    fs::write(&live, b"new").unwrap();
    fs::write(d.join("app.sqlite.pre-restore-9"), b"old").unwrap();
    journal(&d, "quarantining", "app.sqlite.pre-restore-9");
    let outcome = recover_interrupted_restore(&DbOptions::new(&live), &lock()).unwrap();
    assert_eq!(
        outcome,
        Some(InterruptedRestore::KeptPlaced {
            previous: d.join("app.sqlite.pre-restore-9")
        })
    );
    assert_eq!(fs::read(&live).unwrap(), b"new");
    assert_eq!(
        fs::read(d.join("app.sqlite.pre-restore-9")).unwrap(),
        b"old"
    );
    assert!(!d.join("app.sqlite.restore-journal").exists());
}

#[test]
fn recovery_does_nothing_without_a_journal_and_never_adopts_a_stray_kept_file() {
    let d = dir("stray");
    let live = d.join("app.sqlite");
    fs::write(d.join("app.sqlite.pre-restore-3"), b"old").unwrap();
    assert_eq!(
        recover_interrupted_restore(&DbOptions::new(&live), &lock()).unwrap(),
        None
    );
    assert!(!live.exists());
    assert!(d.join("app.sqlite.pre-restore-3").exists());
}

#[test]
fn a_journal_cannot_point_recovery_at_another_file() {
    let d = dir("evil");
    let live = d.join("app.sqlite");
    fs::write(d.join("secret.txt"), b"x").unwrap();
    journal(&d, "placed", "../secret.txt");
    let error = recover_interrupted_restore(&DbOptions::new(&live), &lock()).unwrap_err();
    assert!(matches!(error, DbError::RestoreFailed { .. }));
    assert!(d.join("secret.txt").exists());
    assert!(
        d.join("app.sqlite.restore-journal").exists(),
        "left for a person to look at"
    );
}

#[test]
fn a_restore_first_finishes_an_interrupted_one() {
    let d = dir("first");
    let live = d.join("app.sqlite");
    let replacement = d.join("incoming.sqlite");
    // Crash state: the previous store sits at its kept name, the live path is empty.
    make_db(&d.join("app.sqlite.pre-restore-1"), "old");
    journal(&d, "quarantining", "app.sqlite.pre-restore-1");
    make_db(&replacement, "new");
    let receipt = restore_database(
        &DbOptions::new(&live),
        &replacement,
        &RestoreOptions::default(),
        &lock(),
    )
    .unwrap();
    assert_eq!(read_mark(&live), "new");
    // The recovered store was put back, then kept again by this restore: two kept stores in all
    // would mean the old one was lost, so the receipt's path must hold "old".
    assert_eq!(read_mark(&receipt.previous.unwrap()), "old");
}

#[test]
fn recovery_finishes_sidecars_when_the_main_file_is_already_back() {
    // A put-back stopped between its renames: the previous main file is live again, its -wal and
    // -shm still sit at the kept name. The journal must not be dropped before they move back.
    let d = dir("half-put-back");
    let live = d.join("app.sqlite");
    fs::write(&live, b"main").unwrap();
    fs::write(d.join("app.sqlite.pre-restore-4-wal"), b"wal").unwrap();
    fs::write(d.join("app.sqlite.pre-restore-4-shm"), b"shm").unwrap();
    journal(&d, "placed", "app.sqlite.pre-restore-4");
    let outcome = recover_interrupted_restore(&DbOptions::new(&live), &lock()).unwrap();
    assert_eq!(
        outcome,
        Some(InterruptedRestore::PutBack {
            from: d.join("app.sqlite.pre-restore-4")
        })
    );
    assert_eq!(fs::read(&live).unwrap(), b"main");
    assert_eq!(fs::read(d.join("app.sqlite-wal")).unwrap(), b"wal");
    assert_eq!(fs::read(d.join("app.sqlite-shm")).unwrap(), b"shm");
    assert!(kept(&d, ".pre-restore-").is_empty());
    assert!(!d.join("app.sqlite.restore-journal").exists());
}

#[test]
fn recovery_keeps_the_journal_when_a_sidecar_cannot_move_back() {
    let d = dir("half-put-back-blocked");
    let live = d.join("app.sqlite");
    fs::write(&live, b"main").unwrap();
    fs::write(d.join("app.sqlite-wal"), b"someone else's").unwrap();
    fs::write(d.join("app.sqlite.pre-restore-6-wal"), b"wal").unwrap();
    journal(&d, "placed", "app.sqlite.pre-restore-6");
    let error = recover_interrupted_restore(&DbOptions::new(&live), &lock()).unwrap_err();
    assert!(matches!(error, DbError::RestoreFailed { .. }));
    assert_eq!(
        fs::read(d.join("app.sqlite.pre-restore-6-wal")).unwrap(),
        b"wal"
    );
    assert_eq!(
        fs::read(d.join("app.sqlite-wal")).unwrap(),
        b"someone else's"
    );
    assert!(d.join("app.sqlite.restore-journal").exists());
}
