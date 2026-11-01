//! Verified pre-migration backup of the database file, beside it in the data folder:
//! `<db file name>.pre-v<from>-<UTC stamp>.bak`.
//!
//! RightKit's `migrate` takes the copy (keyed online backup, named and pruned through
//! `MigrateOptions`, see `migrations::migrate_with`). The free-space probe and the check that
//! reopens the copy with the key are `rightkit_db::backup_check`'s.
use rightkit_db::backup_check;
use rightkit_db::rusqlite::Connection;
use std::path::Path;

pub(crate) use backup_check::{check_free_space_on, FreeBytes};

/// Backups kept per database file, newest first (the one just made counts).
pub(crate) const KEEP_BACKUPS: usize = 2;

/// The platform probe: bytes available on the volume holding the given path.
pub(crate) fn platform_free_bytes(db_path: &str) -> Option<u64> {
    backup_check::free_bytes(db_path)
}

/// Number of stored messages, or `None` when the table does not exist yet.
pub(crate) fn message_count(conn: &Connection) -> Result<Option<i64>, String> {
    backup_check::row_count(conn, "messages")
}

/// Re-open the copy with the key and compare it against the source.
pub(crate) fn verify(
    path: &Path,
    key: &str,
    from: i64,
    messages: Option<i64>,
) -> Result<(), String> {
    backup_check::verify(path, key, from, messages, "messages")
}
