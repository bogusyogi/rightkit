//! rightkit-db (default features: plain SQLite, no SQLCipher): open a WAL writer, migrate in two steps with the
//! pre-migration backup, prove a failing step rolls back, read through the read pool (which must refuse writes), take an
//! online backup and restore it into a damaged copy, and checkpoint the WAL.
use rightkit_db::{
    checkpoint_truncate, journal_mode, migrate, online_backup_to, pending_marker, user_version,
    write_txn, DbError, DbOptions, MigrateOptions, Migration,
};
use serde_json::{json, Value};

fn ensure(cond: bool, what: &str) -> Result<(), String> {
    if cond {
        Ok(())
    } else {
        Err(format!("rightkit-db: {what}"))
    }
}

fn e<T: std::fmt::Display>(what: &str) -> impl FnOnce(T) -> String + '_ {
    move |err| format!("rightkit-db: {what}: {err}")
}

fn migrations() -> Vec<Migration> {
    vec![
        Migration::sql(
            1,
            "items",
            "CREATE TABLE items(id INTEGER PRIMARY KEY, body TEXT NOT NULL);",
        )
        .with_down("DROP TABLE items;"),
        Migration::sql(2, "index", "CREATE INDEX items_body ON items(body);")
            .with_down("DROP INDEX items_body;"),
    ]
}

pub fn probe(ctx: &super::Ctx) -> Result<Value, String> {
    let dir = std::fs::canonicalize(ctx.dir("db")?).map_err(e("canonicalize"))?;
    let path = dir.join("app.sqlite");
    let options = DbOptions::new(&path);

    // First open applies migration 1 on a new store; the second open migrates to 2 with a pre-migration backup.
    let (conn, first) = options
        .open_writer_migrated(&migrations()[..1], &MigrateOptions::default())
        .map_err(e("open+migrate v1"))?;
    ensure(
        first.applied == [1],
        &format!("first migration applied {:?}", first.applied),
    )?;
    conn.execute("INSERT INTO items(body) VALUES ('kept')", [])
        .map_err(e("insert"))?;
    let mode = journal_mode(&conn).map_err(e("journal_mode"))?;
    ensure(mode == "wal", &format!("journal mode is {mode}, not wal"))?;
    drop(conn);
    let (mut conn, second) = options
        .open_writer_migrated(&migrations(), &MigrateOptions::default())
        .map_err(e("open+migrate v2"))?;
    ensure(
        second.applied == [2],
        &format!("second migration applied {:?}", second.applied),
    )?;
    ensure(
        second.backup.as_ref().is_some_and(|p| p.exists()),
        "the pre-migration backup was not written",
    )?;
    ensure(
        user_version(&conn).map_err(e("user_version"))? == 2,
        "user_version is not 2",
    )?;
    let again = migrate(
        &conn,
        Some(&path),
        &migrations(),
        &MigrateOptions::default(),
    )
    .map_err(e("re-migrate"))?;
    ensure(
        again.applied.is_empty(),
        "migrating an up-to-date store applied steps",
    )?;

    // A transaction that rolls back leaves nothing; one that commits persists.
    {
        let tx = write_txn(&mut conn).map_err(e("write_txn"))?;
        tx.execute("INSERT INTO items(body) VALUES ('rolled-back')", [])
            .map_err(e("tx insert"))?;
    }
    {
        let tx = write_txn(&mut conn).map_err(e("write_txn 2"))?;
        tx.execute("INSERT INTO items(body) VALUES ('committed')", [])
            .map_err(e("tx insert 2"))?;
        tx.commit().map_err(e("commit"))?;
    }
    let count: i64 = conn
        .query_row("SELECT count(*) FROM items", [], |r| r.get(0))
        .map_err(e("count"))?;
    ensure(
        count == 2,
        &format!("expected 2 rows (kept, committed), found {count}"),
    )?;

    // Read pool: sees committed rows, refuses writes.
    let pool = options.open_read_pool(2).map_err(e("open_read_pool"))?;
    {
        let reader = pool.get().map_err(e("pool get"))?;
        let bodies: Vec<String> = reader
            .prepare("SELECT body FROM items ORDER BY body")
            .map_err(e("prepare"))?
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(e("query_map"))?
            .collect::<Result<_, _>>()
            .map_err(e("collect"))?;
        ensure(
            bodies == ["committed", "kept"],
            &format!("reader saw {bodies:?}"),
        )?;
        ensure(
            reader
                .execute("INSERT INTO items(body) VALUES ('forbidden')", [])
                .is_err(),
            "the read pool accepted a write",
        )?;
    }
    pool.close();

    // A failing step rolls back, keeps the old version and leaves a pending marker; a fixed list then completes.
    let bad_path = dir.join("bad.sqlite");
    let bad_options = DbOptions::new(&bad_path);
    let bad = bad_options.open_writer().map_err(e("open bad"))?;
    let ok1 = [
        Migration::sql(1, "items", "CREATE TABLE items(id INTEGER PRIMARY KEY);")
            .with_down("DROP TABLE items;"),
    ];
    migrate(&bad, Some(&bad_path), &ok1, &MigrateOptions::default()).map_err(e("bad v1"))?;
    let failing = [
        Migration::sql(1, "items", "CREATE TABLE items(id INTEGER PRIMARY KEY);")
            .with_down("DROP TABLE items;"),
        Migration::sql(2, "broken", "INSERT INTO no_such_table VALUES (1);")
            .with_down("DELETE FROM no_such_table;"),
    ];
    match migrate(&bad, Some(&bad_path), &failing, &MigrateOptions::default()) {
        Err(DbError::MigrationFailed { version: 2, .. }) => {}
        other => {
            return Err(format!(
            "rightkit-db: a broken migration step did not fail as MigrationFailed v2: {other:?}"
        ))
        }
    }
    ensure(
        user_version(&bad).map_err(e("user_version bad"))? == 1,
        "a failed step moved user_version",
    )?;
    ensure(
        pending_marker(&bad_path).is_some(),
        "no pending marker after a failed step",
    )?;
    drop(bad);

    // Online backup, then restore into a store whose data was lost.
    let backup = dir.join("backup.sqlite");
    online_backup_to(&options, &conn, &backup, None).map_err(e("online_backup_to"))?;
    ensure(backup.is_file(), "backup file missing")?;
    conn.execute("DELETE FROM items", []).map_err(e("delete"))?;
    drop(conn);
    options.restore_from(&backup).map_err(e("restore_from"))?;
    let restored = options.open_writer().map_err(e("reopen"))?;
    let after: i64 = restored
        .query_row("SELECT count(*) FROM items", [], |r| r.get(0))
        .map_err(e("count after restore"))?;
    ensure(
        after == 2,
        &format!("restore brought back {after} rows, expected 2"),
    )?;
    let status = checkpoint_truncate(&restored).map_err(e("checkpoint_truncate"))?;
    ensure(
        status.busy == 0,
        &format!("a WAL checkpoint with no other connection was busy: {status:?}"),
    )?;

    Ok(
        json!({ "journal_mode": mode, "schema_version": 2, "rows_after_restore": after, "checkpoint_log_frames": status.log_frames }),
    )
}
