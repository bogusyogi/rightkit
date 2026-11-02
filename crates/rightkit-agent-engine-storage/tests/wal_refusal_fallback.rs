use rightkit_agent_engine_storage::Storage;
use rightkit_db::{journal_mode, per_host_path};

#[test]
fn rollback_mode_opens_at_truncate_effective_path() {
    let dir = tempfile::tempdir().expect("temporary database directory");
    let requested = dir.path().join("agent.db");
    let previous = std::env::var_os("RIGHTKIT_SQLITE_JOURNAL_MODE");
    std::env::set_var("RIGHTKIT_SQLITE_JOURNAL_MODE", "truncate");

    let expected = per_host_path(&requested);
    assert_ne!(
        expected, requested,
        "test host must provide a discriminator"
    );

    let storage = Storage::open(&requested).expect("fallback storage opens");
    let opened: String = storage
        .conn()
        .query_row("PRAGMA database_list", [], |row| row.get(2))
        .expect("read effective SQLite path");
    // SQLite reports the resolved path (macOS: /var is /private/var).
    assert_eq!(
        std::fs::canonicalize(&opened).expect("opened path exists"),
        std::fs::canonicalize(&expected).expect("expected path exists")
    );

    let report = journal_mode(storage.conn()).expect("read fallback journal mode");
    assert_eq!(report, "truncate");
    assert!(
        expected.exists(),
        "fallback database must exist at per-host path"
    );
    assert!(
        !requested.exists(),
        "per-host fallback must leave requested database unopened"
    );
    match previous {
        Some(value) => std::env::set_var("RIGHTKIT_SQLITE_JOURNAL_MODE", value),
        None => std::env::remove_var("RIGHTKIT_SQLITE_JOURNAL_MODE"),
    }
}
