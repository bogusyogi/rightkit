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
