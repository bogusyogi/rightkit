use rightkit_db::{
    quarantine_database_opts, DbOptions, MoveMode, QuarantineOptions, QuarantineStamp,
    SidecarPolicy,
};
use std::fs;
use std::path::PathBuf;

fn fixture(label: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "rightkit-db-qopts-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let db = dir.join("app.sqlite");
    fs::write(&db, b"damaged bytes").unwrap();
    (dir, db)
}

#[test]
fn marker_is_written_before_the_move_and_names_both_paths() {
    let (dir, db) = fixture("marker");
    let marker = dir.join("history-recovery.json");
    let options = QuarantineOptions {
        name: QuarantineStamp::Custom(|_| ".unreadable-fixed".into()),
        marker: Some(marker.clone()),
        ..QuarantineOptions::default()
    };
    let dest = quarantine_database_opts(&db, &options).unwrap();
    assert_eq!(dest, dir.join("app.sqlite.unreadable-fixed"));
    assert!(!db.exists(), "original moved away");
    assert_eq!(fs::read(&dest).unwrap(), b"damaged bytes");
    let text = fs::read_to_string(&marker).unwrap();
    // The marker is JSON, so a Windows path appears with its backslashes escaped.
    let json = |p: &std::path::Path| p.display().to_string().replace('\\', "\\\\");
    assert!(
        text.contains("\"schema\":\"rightkit-db-quarantine.v1\""),
        "{text}"
    );
    assert!(
        text.contains(&format!("\"original\":\"{}\"", json(&db))),
        "{text}"
    );
    assert!(
        text.contains(&format!("\"quarantined\":\"{}\"", json(&dest))),
        "{text}"
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn hard_link_move_leaves_the_destination_and_removes_the_original() {
    let (dir, db) = fixture("hardlink");
    fs::write(dir.join("app.sqlite-wal"), b"wal bytes").unwrap();
    let options = QuarantineOptions {
        move_mode: MoveMode::HardLinkThenRemove,
        ..QuarantineOptions::default()
    };
    let dest = quarantine_database_opts(&db, &options).unwrap();
    assert!(!db.exists());
    assert!(!dir.join("app.sqlite-wal").exists());
    assert_eq!(fs::read(&dest).unwrap(), b"damaged bytes");
    assert_eq!(
        fs::read(format!("{}-wal", dest.display())).unwrap(),
        b"wal bytes"
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn delete_sidecars_policy_still_applies_with_options() {
    let (dir, db) = fixture("delete-sidecars");
    fs::write(dir.join("app.sqlite-shm"), b"shm").unwrap();
    let options = QuarantineOptions {
        sidecars: SidecarPolicy::DeleteSidecars,
        ..QuarantineOptions::default()
    };
    quarantine_database_opts(&db, &options).unwrap();
    assert!(!dir.join("app.sqlite-shm").exists());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn secure_delete_option_is_applied_at_open() {
    let (dir, _db) = fixture("secure-delete");
    let path = dir.join("secure.sqlite");
    let mut options = DbOptions::new(&path);
    options.secure_delete = Some(true);
    let conn = options.open_writer().unwrap();
    let on: i64 = conn
        .query_row("PRAGMA secure_delete", [], |row| row.get(0))
        .unwrap();
    assert_eq!(on, 1);
    drop(conn);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn seconds_stamp_takes_a_numbered_name_when_the_plain_one_is_taken() {
    let (dir, db) = fixture("seconds-collision");
    // Occupy this second's name and the next one's, so whichever second the quarantine reads, its
    // plain name is taken and it must take `-1`.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    for second in [now, now + 1] {
        fs::write(dir.join(format!("app.sqlite.corrupt-{second}")), b"earlier").unwrap();
    }
    let options = QuarantineOptions {
        name: QuarantineStamp::Seconds,
        ..QuarantineOptions::default()
    };
    let dest = quarantine_database_opts(&db, &options).unwrap();
    let name = dest.file_name().unwrap().to_string_lossy().into_owned();
    assert!(name.ends_with("-1"), "{name}");
    assert_eq!(fs::read(&dest).unwrap(), b"damaged bytes");
    for second in [now, now + 1] {
        assert_eq!(
            fs::read(dir.join(format!("app.sqlite.corrupt-{second}"))).unwrap(),
            b"earlier",
            "an earlier quarantine is never overwritten"
        );
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn seconds_stamp_quarantines_twice_in_one_run_without_error() {
    let (dir, db) = fixture("seconds-twice");
    let options = QuarantineOptions {
        name: QuarantineStamp::Seconds,
        ..QuarantineOptions::default()
    };
    let first = quarantine_database_opts(&db, &options).unwrap();
    fs::write(&db, b"damaged again").unwrap();
    let second = quarantine_database_opts(&db, &options).unwrap();
    assert_ne!(first, second);
    assert_eq!(fs::read(&first).unwrap(), b"damaged bytes");
    assert_eq!(fs::read(&second).unwrap(), b"damaged again");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn optimize_on_open_is_on_by_default_and_can_be_switched_off() {
    let (dir, _db) = fixture("optimize-off");
    assert!(DbOptions::new(dir.join("default.sqlite")).optimize_on_open);

    let path = dir.join("quiet.sqlite");
    let options = DbOptions::new(&path).without_optimize_on_open();
    assert!(!options.optimize_on_open);
    let conn = options.open_writer().unwrap();
    conn.execute_batch("CREATE TABLE t (x INTEGER);").unwrap();
    drop(conn);

    let mut explicit = DbOptions::new(dir.join("explicit.sqlite"));
    explicit.optimize_on_open = false;
    // Closed before the directory is removed: Windows refuses to delete an open database file.
    drop(explicit.open_writer().unwrap());
    fs::remove_dir_all(dir).unwrap();
}

fn custom_stamp(_: std::time::SystemTime) -> String {
    ".bad-fixed".into()
}

#[test]
fn custom_stamp_takes_a_numbered_name_when_the_plain_one_is_taken() {
    let (dir, db) = fixture("custom-collision");
    fs::write(dir.join("app.sqlite.bad-fixed"), b"earlier").unwrap();
    let options = QuarantineOptions {
        name: QuarantineStamp::Custom(custom_stamp),
        ..QuarantineOptions::default()
    };
    let dest = quarantine_database_opts(&db, &options).unwrap();
    assert_eq!(dest, dir.join("app.sqlite.bad-fixed-1"));
    assert_eq!(
        fs::read(dir.join("app.sqlite.bad-fixed")).unwrap(),
        b"earlier"
    );
    assert_eq!(fs::read(&dest).unwrap(), b"damaged bytes");
    fs::remove_dir_all(dir).unwrap();
}

/// A directory where a sidecar file should be makes its removal fail after the main file moved.
fn block_with_directory(path: &std::path::Path) {
    fs::create_dir(path).unwrap();
    fs::write(path.join("inside"), b"x").unwrap();
}

#[test]
fn a_sidecar_failure_after_the_main_move_rolls_the_main_file_back() {
    let (dir, db) = fixture("rollback");
    block_with_directory(&dir.join("app.sqlite-wal"));
    let error = quarantine_database_opts(
        &db,
        &QuarantineOptions {
            sidecars: SidecarPolicy::DeleteSidecars,
            marker: Some(dir.join("recovery.json")),
            ..QuarantineOptions::default()
        },
    )
    .unwrap_err();
    assert!(!error.to_string().is_empty());
    assert_eq!(
        fs::read(&db).unwrap(),
        b"damaged bytes",
        "main file is back"
    );
    let moved: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains(".corrupt-"))
        .collect();
    assert!(moved.is_empty(), "nothing is left at a quarantine name");
    assert!(
        !dir.join("recovery.json").exists(),
        "an undone move leaves no marker"
    );
}

#[test]
fn a_stop_after_a_deleted_sidecar_keeps_the_move_and_the_marker() {
    // The -wal is deleted first, then the -shm blocks: the delete cannot be undone, so the main
    // file stays quarantined and the failure names the split instead of claiming it is untouched.
    let (dir, db) = fixture("stranded");
    fs::write(dir.join("app.sqlite-wal"), b"wal").unwrap();
    block_with_directory(&dir.join("app.sqlite-shm"));
    let marker = dir.join("recovery.json");
    let error = quarantine_database_opts(
        &db,
        &QuarantineOptions {
            sidecars: SidecarPolicy::DeleteSidecars,
            marker: Some(marker.clone()),
            ..QuarantineOptions::default()
        },
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("quarantine incomplete"),
        "{error}"
    );
    assert!(!db.exists(), "the main file was moved aside");
    assert!(marker.exists(), "the marker is kept for recovery");
}
