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
