//! App-namespaced journal kill switch and per-host rollback fallback (CodeRight's
//! `wal_refusal_fallback` contract).

use rightkit_db::{journal_mode, per_host_path, DbOptions};

fn unique_env(tag: &str) -> String {
    format!("RKDB_TEST_{tag}_{}", std::process::id())
}

#[test]
fn the_apps_kill_switch_forces_truncate_on_a_per_host_sibling_without_creating_the_request() {
    let dir = tempfile::tempdir().unwrap();
    let requested = dir.path().join("store.db");
    let env = unique_env("TRUNCATE");
    std::env::set_var(&env, "truncate");
    let options = DbOptions::new(&requested)
        .with_journal_env(&env)
        .with_per_host_fallback(true);

    let sibling = per_host_path(&requested);
    assert_eq!(options.effective_path(), sibling);
    let writer = options.open_writer().unwrap();
    writer
        .execute_batch("CREATE TABLE t(x); INSERT INTO t VALUES (1);")
        .unwrap();
    assert_eq!(journal_mode(&writer).unwrap(), "truncate");
    assert!(sibling.exists(), "the per-host sibling holds the data");
    if sibling != requested {
        assert!(!requested.exists(), "the requested path is not created");
    }
    let reader = options.open_reader().unwrap();
    let n: i64 = reader
        .query_row("SELECT count(*) FROM t", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1, "readers open the same sibling");
    std::env::remove_var(&env);
}

#[test]
fn without_per_host_fallback_the_kill_switch_keeps_the_requested_file() {
    let dir = tempfile::tempdir().unwrap();
    let requested = dir.path().join("plain.db");
    let env = unique_env("DELETE");
    std::env::set_var(&env, "delete");
    let options = DbOptions::new(&requested).with_journal_env(&env);
    assert_eq!(options.effective_path(), requested);
    let writer = options.open_writer().unwrap();
    assert_eq!(journal_mode(&writer).unwrap(), "delete");
    assert!(requested.exists());
    std::env::remove_var(&env);
}

#[test]
fn local_disks_keep_wal_on_the_requested_file_even_with_fallback_enabled() {
    let dir = tempfile::tempdir().unwrap();
    let requested = dir.path().join("local.db");
    let options = DbOptions::new(&requested)
        .with_journal_env(unique_env("UNSET"))
        .with_per_host_fallback(true);
    let writer = options.open_writer().unwrap();
    assert_eq!(journal_mode(&writer).unwrap(), "wal");
    assert!(requested.exists());
    assert!(!per_host_path(&requested).exists() || per_host_path(&requested) == requested);
}

#[test]
fn a_runtime_wal_refusal_never_deletes_data_another_connection_committed_into_the_main_file() {
    use rightkit_db::rusqlite::Connection;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let dir = tempfile::tempdir().unwrap();
    let requested = dir.path().join("shared.db");
    let sibling = per_host_path(&requested);
    // Sibling-only (kill switch) layouts never touch the main file; this test needs the
    // WAL-then-fall-back path on the requested file itself.
    if sibling == requested {
        return;
    }
    let env = unique_env("RUNTIME_REFUSAL");
    std::env::remove_var(&env);
    let main = requested.clone();
    let fired = Arc::new(AtomicBool::new(false));
    let hook_fired = fired.clone();
    let mut options = DbOptions::new(&requested)
        .with_journal_env(&env)
        .with_per_host_fallback(true);
    // `init` runs on the first (main-file) connection before its journal setup. A deferred
    // BEGIN makes `journal_mode=WAL` fail ("within a transaction"), which is the runtime
    // refusal; while the creator is in that window another connection commits rollback-journal
    // data into the same main file and closes, with no sidecar left behind.
    options.init = Some(Arc::new(move |conn| {
        if !hook_fired.swap(true, Ordering::SeqCst) {
            let other = Connection::open(&main)?;
            other.execute_batch(
                "CREATE TABLE survivor(x INTEGER); INSERT INTO survivor VALUES (42);",
            )?;
            drop(other);
            conn.execute_batch("BEGIN;")?;
        }
        Ok(())
    }));

    let writer = options.open_writer().unwrap();
    assert!(fired.load(Ordering::SeqCst), "the concurrent commit ran");
    assert_eq!(
        journal_mode(&writer).unwrap(),
        "truncate",
        "the refusal landed the writer on the rollback-journaled per-host sibling"
    );
    assert!(sibling.exists());
    drop(writer);

    assert!(
        requested.exists(),
        "the main file holding another connection's committed data is retained"
    );
    let check = Connection::open(&requested).unwrap();
    let kept: i64 = check
        .query_row("SELECT x FROM survivor", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        kept, 42,
        "the concurrent committed row survived the fallback"
    );
}

#[test]
fn a_runtime_wal_refusal_retains_the_requested_file_and_its_sidecars_and_writes_the_sibling() {
    use rightkit_db::rusqlite::Connection;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    let dir = tempfile::tempdir().unwrap();
    let requested = dir.path().join("retained.db");
    let sibling = per_host_path(&requested);
    // Sibling-only (kill switch) layouts never touch the main file; this test needs the
    // WAL-then-fall-back path on the requested file itself.
    if sibling == requested {
        return;
    }
    let sidecar = |suffix: &str| {
        let mut name = requested.as_os_str().to_os_string();
        name.push(suffix);
        std::path::PathBuf::from(name)
    };
    let sentinels = [
        ("-wal", b"sentinel-wal".as_slice()),
        ("-shm", b"sentinel-shm".as_slice()),
        ("-journal", b"sentinel-journal".as_slice()),
    ];
    let env = unique_env("RETAINED_SIDECARS");
    std::env::remove_var(&env);
    let main = requested.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let hook_calls = calls.clone();
    let planted: Vec<(std::path::PathBuf, &'static [u8])> = sentinels
        .iter()
        .map(|(suffix, bytes)| (sidecar(suffix), *bytes))
        .collect();
    let hook_planted = planted.clone();
    let mut options = DbOptions::new(&requested)
        .with_journal_env(&env)
        .with_per_host_fallback(true);
    // `init` runs once per connection the open creates. Call 0 is the main-file connection, whose
    // deferred BEGIN makes `journal_mode=WAL` fail (the runtime refusal) while another connection
    // commits rollback-journal data into the same main file. Call 1 is the per-host sibling
    // connection, opened only after the main-file connection is dropped: the sidecars of the
    // requested file are planted there, so that anything the fallback does to the requested file's
    // siblings after that point would show.
    options.init = Some(Arc::new(move |conn| {
        match hook_calls.fetch_add(1, Ordering::SeqCst) {
            0 => {
                let other = Connection::open(&main)?;
                other.execute_batch(
                    "CREATE TABLE survivor(x INTEGER); INSERT INTO survivor VALUES (7);",
                )?;
                drop(other);
                conn.execute_batch("BEGIN;")?;
            }
            1 => {
                for (path, bytes) in &hook_planted {
                    std::fs::write(path, bytes).map_err(|error| {
                        rightkit_db::rusqlite::Error::ToSqlConversionFailure(Box::new(error))
                    })?;
                }
            }
            _ => {}
        }
        Ok(())
    }));

    let writer = options.open_writer().unwrap();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "the main-file connection and the sibling connection each ran init once"
    );
    assert_eq!(
        journal_mode(&writer).unwrap(),
        "truncate",
        "the refusal landed the writer on the rollback-journaled per-host sibling"
    );
    assert!(
        sibling.exists(),
        "the writer's file is the per-host sibling"
    );
    writer
        .execute_batch("CREATE TABLE on_sibling(x INTEGER); INSERT INTO on_sibling VALUES (1);")
        .unwrap();
    drop(writer);

    assert!(
        requested.exists(),
        "the requested main file is retained after the refusal"
    );
    for (path, bytes) in &planted {
        assert_eq!(
            std::fs::read(path).unwrap(),
            *bytes,
            "{} is retained byte for byte; no cleanup runs on the requested file's siblings",
            path.display()
        );
    }
    // The sentinels are not real SQLite sidecars; remove them before SQLite opens the file again.
    for (path, _) in &planted {
        std::fs::remove_file(path).unwrap();
    }
    let check = Connection::open(&requested).unwrap();
    let kept: i64 = check
        .query_row("SELECT x FROM survivor", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        kept, 7,
        "data committed into the retained main file survived"
    );
    let on_requested: i64 = check
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name = 'on_sibling'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        on_requested, 0,
        "the writer's tables are on the sibling, not the requested file"
    );
}
