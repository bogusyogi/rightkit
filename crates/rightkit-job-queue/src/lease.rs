//! File-based visibility leases. A client that is visible (a dashboard page, a live view) renews a
//! lease so pollers in other processes speed up while it looks; a hidden client simply stops
//! renewing and the lease expires on its own. Each lease is `<folder>/<client>`, rewritten on every
//! renew and alive while its modification time is under [`TTL`] old, so expiry needs no message and
//! readers in any language only list the folder and compare times.

use std::path::Path;
use std::time::{Duration, SystemTime};

/// A lease is alive this long after its last renew.
pub const TTL: Duration = Duration::from_secs(10);
/// Lease files older than this are deleted on the next renew.
pub const PRUNE_AFTER: Duration = Duration::from_secs(60);
/// The longest client name kept, after unsafe characters are dropped.
pub const MAX_CLIENT_LEN: usize = 64;

fn age(entry: &std::fs::DirEntry, now: SystemTime) -> Option<Duration> {
    entry
        .metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| now.duration_since(t).ok())
}

/// The file name a client's lease uses: ASCII letters, digits, `-` and `_` only, at most
/// [`MAX_CLIENT_LEN`] characters. Empty when nothing usable is left.
pub fn client_file_name(client: &str) -> String {
    client
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(MAX_CLIENT_LEN)
        .collect()
}

/// Renew `client`'s lease in `folder` (created when missing) and delete leases older than
/// [`PRUNE_AFTER`]. Returns whether the lease was recorded.
pub fn renew(folder: &Path, client: &str) -> bool {
    let name = client_file_name(client);
    if name.is_empty() || std::fs::create_dir_all(folder).is_err() {
        return false;
    }
    if let Ok(entries) = std::fs::read_dir(folder) {
        let now = SystemTime::now();
        for entry in entries.flatten() {
            if age(&entry, now).is_some_and(|age| age > PRUNE_AFTER) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    std::fs::write(folder.join(name), b"1").is_ok()
}

/// True while any client's lease in `folder` is alive.
pub fn any_alive(folder: &Path) -> bool {
    let now = SystemTime::now();
    std::fs::read_dir(folder).is_ok_and(|entries| {
        entries
            .flatten()
            .any(|entry| age(&entry, now).is_some_and(|age| age < TTL))
    })
}
