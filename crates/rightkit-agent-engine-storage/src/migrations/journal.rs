//! Pending-migration crash marker, as the host app's daemon consumes it.
//!
//! The marker itself is `rightkit_db::migrate`'s `<db>.migrating` file: it is
//! written before a batch starts and removed once the batch commits in full,
//! and `rightkit_db::pending_marker` / `pending_marker_started_at` read it.
//! This module only reshapes that marker into the struct the daemon startup
//! report already exposes.
//!
//! `PRAGMA user_version` stays authoritative; the marker is diagnostic.

use rightkit_db::PendingMarker;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Snapshot of an in-flight (or, if stale, previously-interrupted) migration
/// batch, written next to the database file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PendingMigrationJournal {
    /// `user_version` the database was at when this batch started.
    pub from_version: i64,
    /// The highest version this binary will bring the database to.
    pub to_version: i64,
    /// Versions this batch intends to apply, in order (`from + 1 ..= to`).
    pub pending_versions: Vec<i64>,
    /// Unix seconds when the batch started; `0` for a marker written before
    /// RightKit recorded the start time.
    pub started_at_unix: u64,
    /// Process id that started the batch.
    pub pid: u32,
}

impl PendingMigrationJournal {
    fn from_marker(marker: PendingMarker, started_at_unix: u64) -> Self {
        Self {
            from_version: marker.from,
            to_version: marker.to,
            pending_versions: ((marker.from + 1)..=marker.to).collect(),
            started_at_unix,
            pid: marker.pid,
        }
    }
}

/// Read the pending-migration marker for `db_path`, if one exists. A marker
/// present after `Storage::open` returned means a previous attempt crashed
/// mid-batch (a completed batch removes it).
pub fn read_pending_journal(db_path: &Path) -> Option<PendingMigrationJournal> {
    let marker = rightkit_db::pending_marker(db_path)?;
    let started_at = rightkit_db::pending_marker_started_at(db_path).unwrap_or(0);
    Some(PendingMigrationJournal::from_marker(marker, started_at))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_then_read_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("agent.db");

        assert!(read_pending_journal(&db_path).is_none());

        rightkit_db::write_pending_marker(&db_path, 3, 5).unwrap();
        let journal = read_pending_journal(&db_path).expect("marker must be present");
        assert_eq!(journal.from_version, 3);
        assert_eq!(journal.to_version, 5);
        assert_eq!(journal.pending_versions, vec![4, 5]);
        assert_eq!(journal.pid, std::process::id());
        assert!(journal.started_at_unix > 0);
    }
}
