//! Free disk space for the volume that holds the database.
use std::path::Path;

/// Bytes available to this user on the volume containing `db_path`, or `None`
/// when it cannot be determined (compaction then refuses to run).
pub fn free_bytes(db_path: &str) -> Option<u64> {
    let dir = Path::new(db_path)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    rightkit_fs::free_bytes(dir).ok()
}
