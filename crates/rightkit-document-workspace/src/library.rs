//! Recent files, pins, per-document view state and the tab session, with the broker rules the
//! commands apply on top of the [`Db`] store.

use crate::documents::DocumentBackend;
use crate::error::{broker_error, WorkspaceError};
use crate::explorer::fs_io_error;
use crate::grants::PathGrants;
use crate::store::{Db, FileEntry, PinEntry};

pub fn clamp_recent_limit(limit: Option<i64>) -> i64 {
    limit.unwrap_or(20).clamp(1, 50)
}

#[derive(serde::Serialize)]
pub struct RecentEntry {
    #[serde(flatten)]
    entry: FileEntry,
    // Location metadata only. Opening still requires exchanging the one-shot token.
    display_path: String,
}

fn tokenized_entries(entries: Vec<FileEntry>, grants: &dyn PathGrants) -> Vec<RecentEntry> {
    entries
        .into_iter()
        .filter_map(|mut entry| {
            let token = grants.register_known_stored_path(&entry.path).ok()?;
            let display_path = entry.path.clone();
            entry.path = token;
            Some(RecentEntry {
                entry,
                display_path,
            })
        })
        .collect()
}

pub fn list_recent(
    db: &Db,
    grants: &dyn PathGrants,
    limit: Option<i64>,
) -> Result<Vec<RecentEntry>, WorkspaceError> {
    db.list_recent(clamp_recent_limit(limit))
        .map(|rows| tokenized_entries(rows, grants))
        .map_err(WorkspaceError::from)
}

pub fn clear_recent(db: &Db) -> Result<(), WorkspaceError> {
    db.clear_recent().map_err(WorkspaceError::from)
}

pub fn purge_user_paths(db: &Db) -> Result<(), WorkspaceError> {
    db.purge_user_paths().map_err(WorkspaceError::from)
}

pub fn is_pinned_path(
    db: &Db,
    grants: &dyn PathGrants,
    path: &str,
) -> Result<bool, WorkspaceError> {
    let path = grants.require_existing_file(path).map_err(broker_error)?;
    db.is_pinned(&path).map_err(WorkspaceError::from)
}

pub fn toggle_pin(
    db: &Db,
    grants: &dyn PathGrants,
    docs: &dyn DocumentBackend,
    path: &str,
    display_name: &str,
) -> Result<bool, WorkspaceError> {
    let path = grants.require_existing_file(path).map_err(broker_error)?;
    let format = docs.format_of(&path).to_string();
    db.toggle_pin(&path, display_name, &format)
        .map_err(WorkspaceError::from)
}

/// All pins (files AND directories) in stored pin order. File rows are returned with a live read
/// grant, so the row is directly openable the way an explorer row is. Directory pins hold no
/// grant (grants cover file content only). Rows whose target vanished since pinning are skipped.
pub fn list_pins(db: &Db, grants: &dyn PathGrants) -> Result<Vec<PinEntry>, WorkspaceError> {
    let rows = db.list_pins()?;
    let mut out = Vec::with_capacity(rows.len());
    for entry in rows {
        if entry.kind == "dir" {
            if std::path::Path::new(&entry.path).is_dir() {
                out.push(entry);
            }
        } else if let Ok(granted) =
            grants.grant_existing_file_path(std::path::Path::new(&entry.path))
        {
            out.push(PinEntry {
                path: granted,
                ..entry
            });
        }
    }
    Ok(out)
}

/// Pin or unpin a DIRECTORY (the explorer sidebar's pinned folders). No grant is involved; the
/// path is canonicalized and must exist and be a directory. Returns the new pinned state.
pub fn toggle_pin_dir(
    db: &Db,
    grants: &dyn PathGrants,
    app_name: &str,
    path: &str,
    display_name: &str,
) -> Result<bool, WorkspaceError> {
    let canon = std::fs::canonicalize(path).map_err(|e| fs_io_error(app_name, path, e))?;
    if !canon.is_dir() {
        return Err(WorkspaceError::FileNotFound(format!(
            "not a folder: {path}"
        )));
    }
    let path = grants.path_to_string(&canon);
    db.toggle_pin_dir(&path, display_name)
        .map_err(WorkspaceError::from)
}

pub fn record_recent(
    db: &Db,
    grants: &dyn PathGrants,
    docs: &dyn DocumentBackend,
    path: &str,
    display_name: &str,
) -> Result<(), WorkspaceError> {
    let path = grants.require_existing_file(path).map_err(broker_error)?;
    let format = docs.format_of(&path).to_string();
    db.record_recent(&path, display_name, &format)
        .map_err(WorkspaceError::from)
}

pub fn save_view_state(
    db: &Db,
    grants: &dyn PathGrants,
    docs: &dyn DocumentBackend,
    path: &str,
    last_view: &str,
    view_meta: Option<&str>,
) -> Result<(), WorkspaceError> {
    let path = grants.require_existing_file(path).map_err(broker_error)?;
    let format = docs.format_of(&path).to_string();
    let display_name = std::path::Path::new(&path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(&path)
        .to_string();
    db.save_view_state(&path, last_view, view_meta, &display_name, &format)
        .map_err(WorkspaceError::from)
}

pub fn save_session(
    db: &Db,
    grants: &dyn PathGrants,
    tabs: Vec<(String, bool)>,
) -> Result<(), WorkspaceError> {
    // Only persist paths that the broker recognizes (have a live grant). Silently skip others.
    let valid: Vec<(String, bool)> = tabs
        .into_iter()
        .filter(|(path, _)| grants.require_existing_file(path).is_ok())
        .collect();
    let refs: Vec<(&str, bool)> = valid.iter().map(|(p, a)| (p.as_str(), *a)).collect();
    db.save_session(&refs).map_err(WorkspaceError::from)
}

pub fn load_session(
    db: &Db,
    grants: &dyn PathGrants,
) -> Result<Vec<(String, bool)>, WorkspaceError> {
    let rows = db.load_session().map_err(WorkspaceError::from)?;
    // Map real paths to one-shot known tokens. Skip files that no longer exist.
    let mut result = Vec::new();
    for (path, active) in rows {
        if let Ok(token) = grants.register_known_stored_path(&path) {
            result.push((token, active));
        }
    }
    Ok(result)
}

pub fn clear_session(db: &Db) -> Result<(), WorkspaceError> {
    db.clear_session().map_err(WorkspaceError::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_limit_is_clamped() {
        assert_eq!(clamp_recent_limit(None), 20);
        assert_eq!(clamp_recent_limit(Some(0)), 1);
        assert_eq!(clamp_recent_limit(Some(500)), 50);
    }
}
