//! End-to-end database checks: real SQLite/SQLCipher files, migrations, backups, recovery,
//! and a child process killed while a migration owns its marker.

use rightkit_db::{migrate, pending_marker, rollback_to, DbError, DbOptions, Migration, MigrateOptions, RecoveryPolicy};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

fn migrations() -> Vec<Migration> {
    vec![
        Migration::sql(1, "items", "CREATE TABLE items(id INTEGER PRIMARY KEY, body TEXT NOT NULL);")
            .with_down("DROP TABLE items;"),
        Migration::sql(2, "index", "CREATE INDEX items_body ON items(body);")
            .with_down("DROP INDEX items_body;"),
    ]
}

fn wait_ready(child: &mut Child) {
    let output = child.stdout.take().expect("child stdout");
    for line in BufReader::new(output).lines() {
        if line.expect("child output").trim_end().ends_with("READY") {
            return;
        }
    }
    panic!("child exited before READY");
}

fn kill9(mut child: Child) {
    child.kill().expect("kill child");
    let _ = child.wait();
}

fn blocking_migration(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    conn.execute_batch("CREATE TABLE items(id INTEGER PRIMARY KEY);")?;
    println!("READY");
    std::io::stdout().flush().expect("flush READY");
    std::thread::sleep(Duration::from_secs(120));
    Ok(())
}

fn spawn_migration_helper(path: &Path) -> Child {
    Command::new(std::env::current_exe().expect("test executable"))
        .args(["--exact", "migration_helper", "--nocapture", "--test-threads=1"])
        .env("RKDB_PATH", path)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn migration helper")
}

/// Child role used by `migration_marker_survives_sigkill`.
#[test]
fn migration_helper() {
    let Ok(path) = std::env::var("RKDB_PATH") else { return };
    let options = DbOptions::new(PathBuf::from(path));
    let migration = Migration::code(1, "blocking", blocking_migration);
    let _ = options.open_writer_migrated(&[migration], &MigrateOptions::default());
}

#[test]
fn migrations_are_transactional_idempotent_and_backed_up() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.sqlite");
    let options = DbOptions::new(&path);
    let (conn, first) = options
        .open_writer_migrated(&migrations()[..1], &MigrateOptions::default())
        .unwrap();
    assert_eq!(first.applied, vec![1]);
    conn.execute("INSERT INTO items(body) VALUES ('kept')", []).unwrap();
    drop(conn);

    let (conn, second) = options
        .open_writer_migrated(&migrations(), &MigrateOptions::default())
        .unwrap();
    assert_eq!(second.applied, vec![2]);
    assert!(second.backup.as_ref().is_some_and(|p| p.exists()));
    let count: i64 = conn
        .query_row("SELECT count(*) FROM items WHERE body = 'kept'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
    let again = migrate(&conn, Some(&path), &migrations(), &MigrateOptions::default()).unwrap();
    assert!(again.applied.is_empty());
    assert_eq!(again.from, 2);
}

#[test]
fn failed_step_rolls_back_and_marker_is_reported_then_cleared() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("failed.sqlite");
    let options = DbOptions::new(&path);
    let conn = options.open_writer().unwrap();
    let first = [Migration::sql(1, "items", "CREATE TABLE items(id INTEGER PRIMARY KEY);")
        .with_down("DROP TABLE items;")];
    migrate(&conn, Some(&path), &first, &MigrateOptions::default()).unwrap();
    let failing = [
        Migration::sql(1, "items", "CREATE TABLE items(id INTEGER PRIMARY KEY);")
            .with_down("DROP TABLE items;"),
        Migration::sql(2, "broken", "INSERT INTO no_such_table VALUES (1);")
            .with_down("DELETE FROM no_such_table;"),
    ];
    let error = migrate(&conn, Some(&path), &failing, &MigrateOptions::default()).unwrap_err();
    assert!(matches!(error, DbError::MigrationFailed { version: 2, .. }));
    assert_eq!(rightkit_db::user_version(&conn).unwrap(), 1);
    assert!(pending_marker(&path).is_some());

    let fixed = [
        Migration::sql(1, "items", "CREATE TABLE items(id INTEGER PRIMARY KEY);")
            .with_down("DROP TABLE items;"),
        Migration::sql(2, "index", "CREATE INDEX items_id ON items(id);")
            .with_down("DROP INDEX items_id;"),
    ];
    let report = migrate(&conn, Some(&path), &fixed, &MigrateOptions::default()).unwrap();
    assert_eq!(report.interrupted_previous.unwrap().from, 1);
    assert_eq!(report.applied, vec![2]);
    assert!(pending_marker(&path).is_none());
}

#[test]
fn newer_database_requires_forward_compatibility_probe() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("newer.sqlite");
    let options = DbOptions::new(&path);
    let conn = options.open_writer().unwrap();
    conn.execute_batch("PRAGMA user_version = 9;").unwrap();
    let one = [Migration::sql(1, "items", "CREATE TABLE items(id INTEGER);")];
    assert!(matches!(
        migrate(&conn, Some(&path), &one, &MigrateOptions::default()),
        Err(DbError::NewerDatabase { current: 9, max: 1 })
    ));
    let compatible = MigrateOptions {
        forward_compatible: Some(|_| true),
        ..MigrateOptions::default()
    };
    let report = migrate(&conn, Some(&path), &one, &compatible).unwrap();
    assert!(report.forward_compatible);
    assert!(report.applied.is_empty());
}

#[test]
fn rollback_requires_down_steps_and_reverts_in_reverse_order() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollback.sqlite");
    let options = DbOptions::new(&path);
    let conn = options.open_writer().unwrap();
    let all = migrations();
    migrate(&conn, Some(&path), &all, &MigrateOptions::default()).unwrap();
    let no_down = [
        Migration::sql(1, "items", "CREATE TABLE items(id INTEGER);").with_down("DROP TABLE items;"),
        Migration::sql(2, "index", "CREATE INDEX items_id ON items(id);")
    ];
    assert!(matches!(
        rollback_to(&conn, &no_down, 0),
        Err(DbError::Rollback(_))
    ));
    let reverted = rollback_to(&conn, &all, 0).unwrap();
    assert_eq!(reverted, vec![2, 1]);
    assert_eq!(rightkit_db::user_version(&conn).unwrap(), 0);
}

#[test]
fn migration_marker_survives_sigkill() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("killed.sqlite");
    let mut child = spawn_migration_helper(&path);
    wait_ready(&mut child);
    assert!(pending_marker(&path).is_some());
    kill9(child);
    let marker = pending_marker(&path).expect("marker remains after SIGKILL");
    assert_eq!(marker.to, 1);
}

#[test]
fn corrupt_plain_database_is_quarantined_without_deletion() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("corrupt.sqlite");
    std::fs::write(&path, b"garbage, not sqlite").unwrap();
    let outcome = DbOptions::new(&path)
        .open_writer_recovering(RecoveryPolicy { quarantine_corrupt: true, quarantine_keyed: false })
        .unwrap();
    let moved = outcome.quarantined.unwrap();
    assert!(moved.exists());
    assert_eq!(std::fs::read(moved).unwrap(), b"garbage, not sqlite");
    assert!(path.exists());
    assert!(outcome.conn.is_autocommit());
}

#[cfg(feature = "sqlcipher")]
#[test]
fn sqlcipher_rekey_and_candidate_recovery_round_trip() {
    use zeroize::Zeroizing;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cipher.sqlite");
    let old = "b2xkLWtleQ==";
    let new = "bmV3LWtleQ==";
    let options = DbOptions::new(&path).with_passphrase(old);
    let conn = options.open_writer().unwrap();
    conn.execute_batch("CREATE TABLE secret(value TEXT); INSERT INTO secret VALUES ('kept');")
        .unwrap();
    rightkit_db::rekey(&conn, new).unwrap();
    drop(conn);
    assert!(DbOptions::new(&path).with_passphrase(old).open_reader().is_err());
    let new_options = DbOptions::new(&path).with_passphrase(new);
    let conn = new_options.open_reader().unwrap();
    assert_eq!(conn.query_row("SELECT value FROM secret", [], |r| r.get::<_, String>(0)).unwrap(), "kept");
    drop(conn);
    let candidates = [Zeroizing::new(old.to_string()), Zeroizing::new(new.to_string())];
    let (conn, index) = new_options.open_writer_with_key_candidates(&candidates).unwrap();
    assert_eq!(index, 1);
    assert_eq!(conn.query_row("SELECT count(*) FROM secret", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
}
