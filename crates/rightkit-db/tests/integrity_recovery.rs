//! Open-time integrity checks and quarantine policy, against real SQLite files.

use rightkit_db::{
    quarantine_database_with, DbError, DbOptions, IntegrityCheck, QuarantineStamp, RecoveryPolicy,
    SidecarPolicy,
};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

fn healthy_database(path: &Path) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "PRAGMA journal_mode=DELETE;
         CREATE TABLE t(x TEXT);
         INSERT INTO t VALUES ('kept');",
    )
    .unwrap();
}

/// A database whose table root page (page 2; page 1 holds the schema) is overwritten with 0xFF.
/// The header stays readable, so an open with no check succeeds, but `quick_check` reports it.
fn damaged_database(path: &Path) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch("PRAGMA journal_mode=DELETE; CREATE TABLE t(x TEXT); BEGIN;")
        .unwrap();
    for _ in 0..300 {
        conn.execute("INSERT INTO t VALUES (?1)", [&"x".repeat(120)])
            .unwrap();
    }
    conn.execute_batch("COMMIT;").unwrap();
    drop(conn);

    let mut bytes = std::fs::read(path).unwrap();
    let page_size = match u16::from_be_bytes([bytes[16], bytes[17]]) as usize {
        1 => 65_536,
        size => size,
    };
    for byte in &mut bytes[page_size..2 * page_size] {
        *byte = 0xFF;
    }
    std::fs::write(path, &bytes).unwrap();

    let conn = Connection::open(path).unwrap();
    let verdict = conn
        .query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0))
        .unwrap_or_else(|error| error.to_string());
    assert_ne!(verdict, "ok", "fixture must fail quick_check");
}

fn with_integrity(path: &Path, integrity: IntegrityCheck) -> DbOptions {
    DbOptions {
        integrity,
        ..DbOptions::new(path)
    }
}

fn policy() -> RecoveryPolicy {
    RecoveryPolicy {
        quarantine_corrupt: true,
        ..Default::default()
    }
}

fn suffix_after(path: &Path, base: &str) -> String {
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    name.strip_prefix(&format!("{base}.corrupt-"))
        .unwrap_or_else(|| panic!("unexpected quarantine name {name}"))
        .to_string()
}

#[test]
fn integrity_check_defaults_to_off() {
    assert_eq!(IntegrityCheck::default(), IntegrityCheck::Off);
    assert_eq!(
        DbOptions::new("unused.sqlite").integrity,
        IntegrityCheck::Off
    );
}

#[test]
fn default_open_does_not_check_integrity() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.sqlite");
    damaged_database(&path);
    // Default options run no open-time integrity check, so the open never reports
    // DbError::Integrity. SQLite can still reject this fixture while switching journal mode,
    // which is SQLite's own corruption error, not a check.
    let result = DbOptions::new(&path).open_writer();
    assert!(
        !matches!(result, Err(rightkit_db::DbError::Integrity(_))),
        "default open must not run an integrity check"
    );
}

#[test]
fn quick_and_full_pass_on_a_healthy_database() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.sqlite");
    healthy_database(&path);
    for integrity in [
        IntegrityCheck::Off,
        IntegrityCheck::Quick,
        IntegrityCheck::Full,
    ] {
        with_integrity(&path, integrity).open_writer().unwrap();
    }
}

#[test]
fn quick_check_failure_is_an_open_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.sqlite");
    damaged_database(&path);
    let error = with_integrity(&path, IntegrityCheck::Quick)
        .open_writer()
        .unwrap_err();
    assert!(
        matches!(error, DbError::Integrity(_) | DbError::Sqlite(_)),
        "unexpected error: {error:?}"
    );
}

#[test]
fn full_check_and_legacy_flag_fail_on_damage() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.sqlite");
    damaged_database(&path);
    assert!(with_integrity(&path, IntegrityCheck::Full)
        .open_writer()
        .is_err());

    let mut legacy = DbOptions::new(&path);
    legacy.integrity_check_on_open = true;
    assert!(legacy.open_writer().is_err());
}

#[test]
fn quick_failure_is_quarantined_and_recreated() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.sqlite");
    damaged_database(&path);
    let options = with_integrity(&path, IntegrityCheck::Quick);

    let refused = options.open_writer_recovering(RecoveryPolicy::default());
    assert!(refused.is_err(), "recovery requires the quarantine policy");
    assert!(path.exists(), "a refused recovery moves nothing");

    let outcome = options.open_writer_recovering(policy()).unwrap();
    let moved = outcome.quarantined.expect("damaged file was quarantined");
    assert!(moved.exists());
    assert!(path.exists());
    let count: i64 = outcome
        .conn
        .query_row("SELECT count(*) FROM sqlite_master", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 0, "the replacement is a fresh, empty database");
}

#[test]
fn full_failure_is_quarantined_and_recreated() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.sqlite");
    damaged_database(&path);
    let outcome = with_integrity(&path, IntegrityCheck::Full)
        .open_writer_recovering(policy())
        .unwrap();
    assert!(outcome.quarantined.is_some());
}

#[test]
fn default_quarantine_name_uses_unix_milliseconds() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.sqlite");
    std::fs::write(&path, b"garbage").unwrap();
    let moved =
        quarantine_database_with(&path, QuarantineStamp::Millis, SidecarPolicy::default()).unwrap();
    let millis: u128 = suffix_after(&moved, "app.sqlite").parse().unwrap();
    assert!(millis > 1_000_000_000_000, "milliseconds since the epoch");
    assert_eq!(std::fs::read(&moved).unwrap(), b"garbage");
}

#[test]
fn seconds_stamp_is_selectable_through_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.sqlite");
    damaged_database(&path);
    let outcome = with_integrity(&path, IntegrityCheck::Quick)
        .open_writer_recovering(RecoveryPolicy {
            quarantine_corrupt: true,
            quarantine_stamp: QuarantineStamp::Seconds,
            ..Default::default()
        })
        .unwrap();
    let moved = outcome.quarantined.unwrap();
    let secs: u128 = suffix_after(&moved, "app.sqlite").parse().unwrap();
    assert!(
        (1_000_000_000..10_000_000_000).contains(&secs),
        "seconds since the epoch"
    );
}

fn fixed_suffix(_now: SystemTime) -> String {
    ".bad-fixed".into()
}

fn empty_suffix(_now: SystemTime) -> String {
    String::new()
}

fn escaping_suffix(_now: SystemTime) -> String {
    "/../escape".into()
}

#[test]
fn custom_stamp_formats_the_whole_suffix() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.sqlite");
    std::fs::write(&path, b"garbage").unwrap();
    let moved = quarantine_database_with(
        &path,
        QuarantineStamp::Custom(fixed_suffix),
        SidecarPolicy::RenameSidecars,
    )
    .unwrap();
    assert_eq!(moved, dir.path().join("app.sqlite.bad-fixed"));
    assert!(moved.exists());
    assert!(!path.exists());
}

#[test]
fn custom_stamp_rejects_empty_or_path_suffixes_without_moving() {
    let dir = tempfile::tempdir().unwrap();
    for formatter in [empty_suffix as fn(SystemTime) -> String, escaping_suffix] {
        let path = dir.path().join("app.sqlite");
        std::fs::write(&path, b"garbage").unwrap();
        let error = quarantine_database_with(
            &path,
            QuarantineStamp::Custom(formatter),
            SidecarPolicy::RenameSidecars,
        )
        .unwrap_err();
        assert!(matches!(error, DbError::InvalidQuarantineSuffix(_)));
        assert!(path.exists(), "a rejected suffix moves nothing");
        std::fs::remove_file(&path).unwrap();
    }
}

#[test]
fn quarantine_numbers_a_custom_stamp_instead_of_overwriting_an_existing_copy() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.sqlite");
    std::fs::write(&path, b"new garbage").unwrap();
    let existing = dir.path().join("app.sqlite.bad-fixed");
    std::fs::write(&existing, b"earlier evidence").unwrap();
    let moved = quarantine_database_with(
        &path,
        QuarantineStamp::Custom(fixed_suffix),
        SidecarPolicy::RenameSidecars,
    )
    .unwrap();
    assert_eq!(moved, dir.path().join("app.sqlite.bad-fixed-1"));
    assert_eq!(std::fs::read(&moved).unwrap(), b"new garbage");
    assert_eq!(std::fs::read(&existing).unwrap(), b"earlier evidence");
    assert!(!path.exists());
}

fn write_with_sidecars(path: &Path) -> (PathBuf, PathBuf) {
    std::fs::write(path, b"garbage").unwrap();
    let wal = dir_sidecar(path, "-wal");
    let shm = dir_sidecar(path, "-shm");
    std::fs::write(&wal, b"wal bytes").unwrap();
    std::fs::write(&shm, b"shm bytes").unwrap();
    (wal, shm)
}

fn dir_sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

#[test]
fn rename_sidecars_moves_them_beside_the_quarantined_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.sqlite");
    write_with_sidecars(&path);
    let moved = quarantine_database_with(
        &path,
        QuarantineStamp::Custom(fixed_suffix),
        SidecarPolicy::RenameSidecars,
    )
    .unwrap();
    assert_eq!(
        std::fs::read(dir_sidecar(&moved, "-wal")).unwrap(),
        b"wal bytes"
    );
    assert_eq!(
        std::fs::read(dir_sidecar(&moved, "-shm")).unwrap(),
        b"shm bytes"
    );
    assert!(!dir_sidecar(&path, "-wal").exists());
    assert!(!dir_sidecar(&path, "-shm").exists());
}

#[test]
fn delete_sidecars_removes_them_and_keeps_only_the_main_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.sqlite");
    write_with_sidecars(&path);
    let moved = quarantine_database_with(
        &path,
        QuarantineStamp::Custom(fixed_suffix),
        SidecarPolicy::DeleteSidecars,
    )
    .unwrap();
    assert_eq!(std::fs::read(&moved).unwrap(), b"garbage");
    assert!(!dir_sidecar(&path, "-wal").exists());
    assert!(!dir_sidecar(&path, "-shm").exists());
    assert!(!dir_sidecar(&moved, "-wal").exists());
    assert!(!dir_sidecar(&moved, "-shm").exists());
}
