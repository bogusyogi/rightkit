// rightkit-db 0.5.1 registry consumer, outside the workspace.
//
// Exercises the plain-SQLite public API with the `bundled` feature: DbOptions::open_writer_migrated
// applies two versioned migrations once and a second open applies none; write_txn commits an insert
// that open_read_only reads back; open_existing_writer refuses a missing file without creating it;
// a read-only ReadPool serves the migrated file; open_in_memory answers a query.
// Not covered: the encrypted lifecycle (sqlcipher features, ProtectedKeyStore, rotation, relocation),
// which needs a SQLCipher build and key material.
// State: a temp directory removed on exit. No network, no OS keychain, no real user data.
use rightkit_db::{
    open_existing_writer, open_in_memory, open_read_only, user_version, write_txn, DbOptions,
    MigrateOptions, Migration, ReadPoolOptions,
};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

type Failure = Box<dyn std::error::Error>;

const MIGRATIONS: &[Migration] = &[
    Migration::sql(
        1,
        "create_items",
        "CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
    ),
    Migration::sql(2, "add_note", "ALTER TABLE items ADD COLUMN note TEXT"),
];

fn main() -> Result<(), Failure> {
    let dir: PathBuf = std::env::temp_dir().join(format!("rk-db-consumer-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let result = exercise(&dir);
    let _ = std::fs::remove_dir_all(&dir);
    result
}

fn exercise(dir: &Path) -> Result<(), Failure> {
    let path = dir.join("items.sqlite");
    let options = DbOptions::new(path.clone());
    let migrate = MigrateOptions {
        backup: false,
        ..MigrateOptions::default()
    };

    let (mut conn, first) = options.open_writer_migrated(MIGRATIONS, &migrate)?;
    assert_eq!(
        first.applied,
        vec![1, 2],
        "both migrations apply on a fresh file"
    );
    assert_eq!(user_version(&conn)?, 2, "schema version after migrations");
    {
        let tx = write_txn(&mut conn)?;
        tx.execute(
            "INSERT INTO items (id, name, note) VALUES (?1, ?2, ?3)",
            rightkit_db::rusqlite::params![1_i64, "alpha", "first"],
        )?;
        tx.commit()?;
    }
    drop(conn);

    let reader = open_read_only(&path)?;
    let name: String =
        reader.query_row("SELECT name FROM items WHERE id = 1", [], |row| row.get(0))?;
    assert_eq!(
        name, "alpha",
        "committed row reads back through open_read_only"
    );
    drop(reader);

    let (_again, second) = options.open_writer_migrated(MIGRATIONS, &migrate)?;
    assert!(
        second.applied.is_empty(),
        "reopening a migrated file must not reapply migrations"
    );

    let missing = dir.join("missing.sqlite");
    assert!(
        open_existing_writer(&missing).is_err(),
        "open_existing_writer must refuse a missing file"
    );
    assert!(!missing.exists(), "refused open must not create the file");

    let pool = options.open_read_pool_with(2, ReadPoolOptions::new().with_read_only(true))?;
    {
        let pooled = pool.get()?;
        let count: i64 = pooled.query_row("SELECT COUNT(*) FROM items", [], |row| row.get(0))?;
        assert_eq!(count, 1, "read pool sees the committed row");
    }
    pool.close_and_wait(Duration::from_secs(5))?;

    let memory = open_in_memory()?;
    let seven: i64 = memory.query_row("SELECT 7", [], |row| row.get(0))?;
    assert_eq!(seven, 7, "in-memory connection answers a query");

    println!("rightkit-db 0.5.1 registry consumer passed");
    Ok(())
}
