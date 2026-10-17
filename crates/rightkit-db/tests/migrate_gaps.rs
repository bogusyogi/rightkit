//! Migration options: backup name, a check on the backup before the first step, backup retention,
//! the pending marker, and the newer-version probe. Real SQLite files.

use rightkit_db::{
    migrate, pending_marker, stored_version_is_newer, user_version, write_pending_marker, DbError,
    DbOptions, MigrateOptions, Migration, VersionStorage,
};
use std::fs;
use std::path::{Path, PathBuf};

fn migrations() -> Vec<Migration> {
    vec![
        Migration::sql(1, "create", "CREATE TABLE t(v INTEGER);"),
        Migration::sql(2, "add_w", "ALTER TABLE t ADD COLUMN w INTEGER;"),
    ]
}

/// A database path in a fresh directory, canonicalized (macOS spells /var as /private/var).
fn fresh(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().canonicalize().unwrap().join("app.sqlite")
}

/// A writer at schema version 1, so the next migrate has an existing database to back up.
fn at_version_one(path: &Path) -> rightkit_db::rusqlite::Connection {
    let conn = DbOptions::new(path).open_writer().unwrap();
    migrate(
        &conn,
        Some(path),
        &migrations()[..1],
        &MigrateOptions::default(),
    )
    .unwrap();
    conn
}

fn named(current: i64, target: i64) -> String {
    format!("snapshot-v{current}-to-v{target}.bak")
}

fn escaping(_: i64, _: i64) -> String {
    "../escape.bak".into()
}

fn same_as_database(_: i64, _: i64) -> String {
    "app.sqlite".into()
}

fn refuse(_: &Path) -> std::result::Result<(), String> {
    Err("row count changed".into())
}

fn accept(path: &Path) -> std::result::Result<(), String> {
    if path.exists() {
        Ok(())
    } else {
        Err("backup missing".into())
    }
}

#[test]
fn the_default_backup_is_named_for_the_target_version() {
    let dir = tempfile::tempdir().unwrap();
    let path = fresh(&dir);
    let conn = at_version_one(&path);
    let report = migrate(
        &conn,
        Some(&path),
        &migrations(),
        &MigrateOptions::default(),
    )
    .unwrap();
    let backup = report.backup.expect("a backup was taken");
    assert_eq!(
        backup.file_name().unwrap().to_string_lossy(),
        "app.sqlite.pre-v2.bak"
    );
    assert_eq!(user_version(&conn).unwrap(), 2);
}

#[test]
fn a_custom_backup_name_is_used() {
    let dir = tempfile::tempdir().unwrap();
    let path = fresh(&dir);
    let conn = at_version_one(&path);
    let options = MigrateOptions {
        backup_name: Some(named),
        ..MigrateOptions::default()
    };
    let report = migrate(&conn, Some(&path), &migrations(), &options).unwrap();
    assert_eq!(
        report.backup,
        Some(path.with_file_name("snapshot-v1-to-v2.bak"))
    );
    assert!(path.with_file_name("snapshot-v1-to-v2.bak").exists());
}

#[test]
fn a_backup_name_that_is_not_a_plain_other_file_name_is_refused_before_any_step() {
    let namers: [fn(i64, i64) -> String; 2] = [escaping, same_as_database];
    for namer in namers {
        let dir = tempfile::tempdir().unwrap();
        let path = fresh(&dir);
        let conn = at_version_one(&path);
        let options = MigrateOptions {
            backup_name: Some(namer),
            ..MigrateOptions::default()
        };
        let error = migrate(&conn, Some(&path), &migrations(), &options).unwrap_err();
        assert!(matches!(error, DbError::Backup(_)), "{error:?}");
        assert_eq!(user_version(&conn).unwrap(), 1);
    }
}

fn live_wal(_: i64, _: i64) -> String {
    "app.sqlite-wal".into()
}
fn live_shm(_: i64, _: i64) -> String {
    "app.sqlite-shm".into()
}
fn live_journal(_: i64, _: i64) -> String {
    "app.sqlite-journal".into()
}
fn restore_journal(_: i64, _: i64) -> String {
    "app.sqlite.restore-journal".into()
}
fn pending_marker_name(_: i64, _: i64) -> String {
    "app.sqlite.migrating".into()
}
fn quarantine_journal(_: i64, _: i64) -> String {
    "app.sqlite.quarantine-journal".into()
}
fn upper_wal(_: i64, _: i64) -> String {
    "APP.SQLITE-WAL".into()
}

#[test]
fn a_backup_name_that_is_a_live_sidecar_or_recovery_file_is_refused_and_nothing_is_overwritten() {
    let namers: [fn(i64, i64) -> String; 7] = [
        live_wal,
        live_shm,
        live_journal,
        restore_journal,
        pending_marker_name,
        quarantine_journal,
        upper_wal,
    ];
    for namer in namers {
        let dir = tempfile::tempdir().unwrap();
        let path = fresh(&dir);
        let conn = at_version_one(&path);
        let wal = path.with_file_name("app.sqlite-wal");
        let before = fs::read(&wal).ok();
        let options = MigrateOptions {
            backup_name: Some(namer),
            ..MigrateOptions::default()
        };
        let error = migrate(&conn, Some(&path), &migrations(), &options).unwrap_err();
        assert!(matches!(error, DbError::Backup(_)), "{error:?}");
        assert_eq!(user_version(&conn).unwrap(), 1, "no step ran");
        assert!(!path.with_file_name("app.sqlite.migrating").exists());
        assert!(!path
            .with_file_name("app.sqlite.quarantine-journal")
            .exists());
        assert_eq!(fs::read(&wal).ok(), before, "the live WAL was not replaced");
    }
}

#[test]
fn a_failed_backup_check_stops_the_migration_and_keeps_the_backup() {
    let dir = tempfile::tempdir().unwrap();
    let path = fresh(&dir);
    let conn = at_version_one(&path);
    let options = MigrateOptions {
        verify_backup: Some(refuse),
        ..MigrateOptions::default()
    };
    let error = migrate(&conn, Some(&path), &migrations(), &options).unwrap_err();
    match error {
        DbError::Backup(message) => assert!(message.contains("row count changed"), "{message}"),
        other => panic!("expected DbError::Backup, got {other:?}"),
    }
    assert_eq!(user_version(&conn).unwrap(), 1, "no step ran");
    assert!(path.with_file_name("app.sqlite.pre-v2.bak").exists());
}

#[test]
fn a_passing_backup_check_lets_the_migration_run() {
    let dir = tempfile::tempdir().unwrap();
    let path = fresh(&dir);
    let conn = at_version_one(&path);
    let options = MigrateOptions {
        verify_backup: Some(accept),
        ..MigrateOptions::default()
    };
    migrate(&conn, Some(&path), &migrations(), &options).unwrap();
    assert_eq!(user_version(&conn).unwrap(), 2);
}

#[test]
fn retention_keeps_only_the_newest_backups_of_this_database() {
    let dir = tempfile::tempdir().unwrap();
    let path = fresh(&dir);
    let conn = at_version_one(&path);
    let folder = path.parent().unwrap().to_path_buf();
    for stray in [
        "app.sqlite.pre-v7.bak",
        "app.sqlite.pre-v8.bak",
        "other.sqlite.pre-v1.bak",
        "app.sqlite.notes.bak",
    ] {
        fs::write(folder.join(stray), b"old").unwrap();
    }
    let options = MigrateOptions {
        backup_retention: Some(1),
        ..MigrateOptions::default()
    };
    migrate(&conn, Some(&path), &migrations(), &options).unwrap();
    assert!(!folder.join("app.sqlite.pre-v7.bak").exists());
    assert!(!folder.join("app.sqlite.pre-v8.bak").exists());
    assert!(
        folder.join("app.sqlite.pre-v2.bak").exists(),
        "the new backup"
    );
    assert!(folder.join("other.sqlite.pre-v1.bak").exists());
    assert!(folder.join("app.sqlite.notes.bak").exists());
}

#[test]
fn write_pending_marker_round_trips_through_pending_marker() {
    let dir = tempfile::tempdir().unwrap();
    let path = fresh(&dir);
    write_pending_marker(&path, 3, 4).unwrap();
    let marker = pending_marker(&path).expect("the marker was written");
    assert_eq!((marker.from, marker.to), (3, 4));
    assert_eq!(marker.pid, std::process::id());
    assert!(rightkit_db::pending_marker_started_at(&path).unwrap() > 0);
}

#[test]
fn a_marker_without_a_start_time_reads_as_zero() {
    let dir = tempfile::tempdir().unwrap();
    let path = fresh(&dir);
    fs::write(
        path.with_file_name("app.sqlite.migrating"),
        "from=1\nto=2\npid=5\n",
    )
    .unwrap();
    let marker = pending_marker(&path).expect("the marker parses");
    assert_eq!(rightkit_db::pending_marker_started_at(&path), Some(0));
    assert_eq!(marker.pid, 5);
}

#[test]
fn stored_version_is_newer_reads_without_migrating() {
    let dir = tempfile::tempdir().unwrap();
    let path = fresh(&dir);
    let conn = at_version_one(&path);
    migrate(
        &conn,
        Some(&path),
        &migrations(),
        &MigrateOptions::default(),
    )
    .unwrap();
    let storage = VersionStorage::UserVersion;
    assert!(!stored_version_is_newer(&conn, &storage, &migrations()).unwrap());
    assert!(stored_version_is_newer(&conn, &storage, &migrations()[..1]).unwrap());
    assert_eq!(user_version(&conn).unwrap(), 2, "the probe wrote nothing");
}
