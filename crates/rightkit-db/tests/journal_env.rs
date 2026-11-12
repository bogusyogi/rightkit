//! The environment kill-switch picks among the modes that may fall back; it never weakens strict WAL.
//! One test, because the environment is process-wide.
#![cfg(feature = "cipher")]

use rightkit_db::{journal_mode, DbOptions, JournalSelect, JOURNAL_ENV};

fn mode_of(journal: JournalSelect, dir: &std::path::Path, name: &str) -> String {
    let mut options = DbOptions::new(dir.join(name));
    options.journal = journal;
    let conn = options.open_writer().expect("open");
    journal_mode(&conn).expect("journal mode")
}

#[test]
fn environment_switch_never_overrides_strict_wal() {
    let dir = std::env::temp_dir().join(format!("rk-db-journal-env-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var(JOURNAL_ENV, "delete");
    // Strict is WAL or fail whatever the environment says.
    assert_eq!(mode_of(JournalSelect::WalStrict, &dir, "strict.db"), "wal");
    // The fallback mode still honours the kill switch.
    assert_eq!(
        mode_of(JournalSelect::WalWithFallback, &dir, "fallback.db"),
        "delete"
    );
    std::env::set_var(JOURNAL_ENV, "truncate");
    assert_eq!(mode_of(JournalSelect::WalStrict, &dir, "strict2.db"), "wal");
    std::env::remove_var(JOURNAL_ENV);
    assert_eq!(
        mode_of(JournalSelect::WalWithFallback, &dir, "plain.db"),
        "wal"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
