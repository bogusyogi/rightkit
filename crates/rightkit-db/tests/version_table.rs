//! `VersionStorage::VersionTable` and legacy version adoption, against a fixture shaped like
//! ViewRight's `_migrations(version INTEGER PRIMARY KEY, applied_at INTEGER)` table.

use rightkit_db::{
    migrate, migration_version, rollback_to, rollback_to_with_storage, user_version, DbError,
    LegacyVersionTable, MigrateOptions, Migration, VersionStorage,
};
use rusqlite::Connection;
use std::path::Path;

fn migrations() -> Vec<Migration> {
    vec![
        Migration::sql(
            1,
            "items",
            "CREATE TABLE items(id INTEGER PRIMARY KEY, body TEXT NOT NULL);",
        )
        .with_down("DROP TABLE items;"),
        Migration::sql(2, "tags", "CREATE TABLE tags(name TEXT);").with_down("DROP TABLE tags;"),
        Migration::sql(3, "notes", "CREATE TABLE notes(body TEXT);").with_down("DROP TABLE notes;"),
        Migration::sql(4, "extra", "CREATE TABLE extra(x INTEGER);").with_down("DROP TABLE extra;"),
    ]
}

/// The legacy app: `_migrations` rows for `versions`, plus the tables migrations 1-3 create. The
/// plain `CREATE TABLE` statements make a re-run of those migrations fail, so a test sees it.
fn legacy_app(path: &Path, versions: &[i64]) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE _migrations (version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL);
         CREATE TABLE items(id INTEGER PRIMARY KEY, body TEXT NOT NULL);
         CREATE TABLE tags(name TEXT);
         CREATE TABLE notes(body TEXT);",
    )
    .unwrap();
    for version in versions {
        conn.execute(
            "INSERT INTO _migrations(version, applied_at) VALUES (?1, 1700000000)",
            [version],
        )
        .unwrap();
    }
}

fn legacy_table() -> LegacyVersionTable {
    LegacyVersionTable {
        table: "_migrations".into(),
        version_column: "version".into(),
    }
}

fn stored_versions(conn: &Connection, table: &str) -> Vec<i64> {
    let mut statement = conn
        .prepare(&format!("SELECT version FROM {table} ORDER BY version"))
        .unwrap();
    statement
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn no_backup() -> MigrateOptions {
    MigrateOptions {
        backup: false,
        ..Default::default()
    }
}

#[test]
fn version_table_default_names_and_row_tracking() {
    assert_eq!(
        VersionStorage::version_table("schema_versions"),
        VersionStorage::VersionTable {
            table: "schema_versions".into(),
            version_column: "version".into(),
            applied_at_column: "applied_at".into(),
        }
    );

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.sqlite");
    let conn = Connection::open(&path).unwrap();
    let storage = VersionStorage::version_table("schema_versions");
    let options = MigrateOptions {
        version_storage: storage.clone(),
        ..no_backup()
    };

    let report = migrate(&conn, Some(&path), &migrations(), &options).unwrap();
    assert_eq!(report.applied, vec![1, 2, 3, 4]);
    assert_eq!(stored_versions(&conn, "schema_versions"), vec![1, 2, 3, 4]);
    assert_eq!(migration_version(&conn, &storage).unwrap(), 4);
    assert_eq!(user_version(&conn).unwrap(), 0, "user_version is untouched");
    let applied_at: i64 = conn
        .query_row(
            "SELECT applied_at FROM schema_versions WHERE version = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(applied_at > 1_700_000_000, "applied_at stores unix seconds");

    let reverted = rollback_to_with_storage(&conn, &migrations(), 2, &storage).unwrap();
    assert_eq!(reverted, vec![4, 3]);
    assert_eq!(stored_versions(&conn, "schema_versions"), vec![1, 2]);
    assert_eq!(migration_version(&conn, &storage).unwrap(), 2);

    let report = migrate(&conn, Some(&path), &migrations(), &options).unwrap();
    assert_eq!(report.from, 2);
    assert_eq!(report.applied, vec![3, 4]);
}

#[test]
fn version_table_rejects_unsafe_identifiers() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.sqlite");
    let conn = Connection::open(&path).unwrap();
    let options = MigrateOptions {
        version_storage: VersionStorage::VersionTable {
            table: "versions; DROP TABLE items".into(),
            version_column: "version".into(),
            applied_at_column: "applied_at".into(),
        },
        ..no_backup()
    };
    assert!(matches!(
        migrate(&conn, Some(&path), &migrations(), &options),
        Err(DbError::InvalidMigrations(_))
    ));
}

#[test]
fn legacy_import_adopts_version_once_and_skips_applied_migrations() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.sqlite");
    legacy_app(&path, &[1, 2, 3]);
    let conn = Connection::open(&path).unwrap();
    let options = no_backup().import_legacy_versions(legacy_table());

    // Migrations 1-3 would fail if re-run (their tables already exist), so only 4 applies.
    let report = migrate(&conn, Some(&path), &migrations(), &options).unwrap();
    assert_eq!(report.from, 3);
    assert_eq!(report.applied, vec![4]);
    assert_eq!(user_version(&conn).unwrap(), 4);

    // Idempotent: a second open adopts nothing and applies nothing.
    let again = migrate(&conn, Some(&path), &migrations(), &options).unwrap();
    assert_eq!(again.from, 4);
    assert!(again.applied.is_empty());

    // The legacy table is read, never written.
    assert_eq!(stored_versions(&conn, "_migrations"), vec![1, 2, 3]);
}

#[test]
fn legacy_import_seeds_version_table_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.sqlite");
    legacy_app(&path, &[1, 2, 3]);
    let conn = Connection::open(&path).unwrap();
    let storage = VersionStorage::version_table("schema_versions");
    let options = MigrateOptions {
        version_storage: storage.clone(),
        ..no_backup()
    }
    .import_legacy_versions(legacy_table());

    let report = migrate(&conn, Some(&path), &migrations(), &options).unwrap();
    assert_eq!(report.applied, vec![4]);
    assert_eq!(stored_versions(&conn, "schema_versions"), vec![1, 2, 3, 4]);
    assert_eq!(migration_version(&conn, &storage).unwrap(), 4);
}

#[test]
fn legacy_import_is_a_no_op_without_or_with_an_empty_legacy_table() {
    let dir = tempfile::tempdir().unwrap();

    let absent = dir.path().join("absent.sqlite");
    let conn = Connection::open(&absent).unwrap();
    let options = no_backup().import_legacy_versions(legacy_table());
    let report = migrate(&conn, Some(&absent), &migrations(), &options).unwrap();
    assert_eq!(report.applied, vec![1, 2, 3, 4]);
    drop(conn);

    let empty = dir.path().join("empty.sqlite");
    let conn = Connection::open(&empty).unwrap();
    conn.execute_batch(
        "CREATE TABLE _migrations (version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL);",
    )
    .unwrap();
    let report = migrate(&conn, Some(&empty), &migrations(), &options).unwrap();
    assert_eq!(report.applied, vec![1, 2, 3, 4]);
}

#[test]
fn legacy_version_above_build_is_refused_without_writing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("newer.sqlite");
    legacy_app(&path, &[1, 9]);
    let conn = Connection::open(&path).unwrap();
    let options = no_backup().import_legacy_versions(legacy_table());
    assert!(matches!(
        migrate(&conn, Some(&path), &migrations(), &options),
        Err(DbError::NewerDatabase { current: 9, max: 4 })
    ));
    assert_eq!(user_version(&conn).unwrap(), 0, "nothing was seeded");
}

#[test]
fn legacy_import_does_not_undo_a_later_rollback() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rolled.sqlite");
    legacy_app(&path, &[1, 2, 3]);
    let conn = Connection::open(&path).unwrap();
    let options = no_backup().import_legacy_versions(legacy_table());
    migrate(&conn, Some(&path), &migrations(), &options).unwrap();
    assert_eq!(user_version(&conn).unwrap(), 4);

    let reverted = rollback_to(&conn, &migrations(), 2).unwrap();
    assert_eq!(reverted, vec![4, 3]);

    // The legacy table still says 3, but the configured storage is above zero, so it is not
    // consulted again. Migrations 3 and 4 run again from version 2.
    let report = migrate(&conn, Some(&path), &migrations(), &options).unwrap();
    assert_eq!(report.from, 2);
    assert_eq!(report.applied, vec![3, 4]);
}

#[test]
fn legacy_table_name_is_identifier_checked() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ident.sqlite");
    legacy_app(&path, &[1]);
    let conn = Connection::open(&path).unwrap();
    let options = no_backup().import_legacy_versions(LegacyVersionTable {
        table: "_migrations; DROP TABLE items".into(),
        version_column: "version".into(),
    });
    assert!(matches!(
        migrate(&conn, Some(&path), &migrations(), &options),
        Err(DbError::InvalidMigrations(_))
    ));
}
