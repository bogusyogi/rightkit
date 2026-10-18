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
