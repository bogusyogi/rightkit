//! Durable checkpoint metadata store: one JSON file per checkpoint under
//! `<workspace>/<state-dir>/checkpoints/<id>.json`. Deliberately simple and
//! dependency-free (no `rightkit-agent-engine-storage` dependency from this leaf crate)
//! so a checkpoint outlives the process and is inspectable by hand.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CheckpointRecord {
    /// Same as `commit_oid` today; kept as a distinct field so the id
    /// surface can evolve (e.g. short-hash aliasing) without a schema break.
    pub id: String,
    pub commit_oid: String,
    pub parent_oid: Option<String>,
    pub label: Option<String>,
    pub created_at_unix: u64,
}

pub struct CheckpointStore {
    dir: PathBuf,
}

impl CheckpointStore {
    pub fn new(workspace_root: &Path) -> Self {
        Self {
            dir: workspace_root.join(".rightkit").join("checkpoints"),
        }
    }

    pub fn append(&self, record: &CheckpointRecord) -> Result<(), String> {
        std::fs::create_dir_all(&self.dir)
            .map_err(|error| format!("Failed to create checkpoint store dir: {error}"))?;
        // Checkpoint metadata must never be captured by a later snapshot nor
        // deleted as "untracked" by a restore of an earlier one: a restore to
        // turn N would otherwise erase the records for turns N+1.. and make
        // them unrestorable. A self-ignoring `.gitignore` keeps the whole
        // directory out of the private-index scan and out of
        // `remove_untracked` (ignored files are not removed).
        let ignore = self.dir.join(".gitignore");
        if !ignore.exists() {
            let _ = std::fs::write(&ignore, "*\n");
        }
        let path = self.dir.join(format!("{}.json", sanitize(&record.id)));
        let encoded = serde_json::to_vec_pretty(record)
            .map_err(|error| format!("Failed to serialize checkpoint record: {error}"))?;
        std::fs::write(&path, encoded)
            .map_err(|error| format!("Failed to write checkpoint record: {error}"))
    }

    pub fn find(&self, id: &str) -> Result<Option<CheckpointRecord>, String> {
        let path = self.dir.join(format!("{}.json", sanitize(id)));
        if !path.exists() {
            return Ok(None);
        }
        let bytes = std::fs::read(&path)
            .map_err(|error| format!("Failed to read checkpoint record: {error}"))?;
        let record = serde_json::from_slice(&bytes)
            .map_err(|error| format!("Corrupt checkpoint record: {error}"))?;
        Ok(Some(record))
    }

    pub fn list(&self) -> Result<Vec<CheckpointRecord>, String> {
        if !self.dir.exists() {
            return Ok(Vec::new());
        }
        let mut records = Vec::new();
        let entries = std::fs::read_dir(&self.dir)
            .map_err(|error| format!("Failed to list checkpoint store dir: {error}"))?;
        for entry in entries {
            let entry =
                entry.map_err(|error| format!("Failed to read checkpoint entry: {error}"))?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let bytes = std::fs::read(&path)
                .map_err(|error| format!("Failed to read checkpoint record: {error}"))?;
            let record: CheckpointRecord = serde_json::from_slice(&bytes).map_err(|error| {
                format!("Corrupt checkpoint record at {}: {error}", path.display())
            })?;
            records.push(record);
        }
        records.sort_by_key(|record| record.created_at_unix);
        Ok(records)
    }
}

/// Checkpoint ids are git object ids (hex), but sanitize defensively before
/// using one as a filename component.
fn sanitize(id: &str) -> String {
    id.chars().filter(|c| c.is_ascii_alphanumeric()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn append_then_find_round_trips() {
        let temp = TempDir::new().unwrap();
        let store = CheckpointStore::new(temp.path());
        let record = CheckpointRecord {
            id: "abc123".to_string(),
            commit_oid: "abc123".to_string(),
            parent_oid: None,
            label: Some("test".to_string()),
            created_at_unix: 42,
        };
        store.append(&record).unwrap();
        let found = store.find("abc123").unwrap().unwrap();
        assert_eq!(found, record);
    }

    #[test]
    fn find_missing_returns_none() {
        let temp = TempDir::new().unwrap();
        let store = CheckpointStore::new(temp.path());
        assert_eq!(store.find("nope").unwrap(), None);
    }

    #[test]
    fn list_returns_records_sorted_by_creation_time() {
        let temp = TempDir::new().unwrap();
        let store = CheckpointStore::new(temp.path());
        store
            .append(&CheckpointRecord {
                id: "second".to_string(),
                commit_oid: "second".to_string(),
                parent_oid: None,
                label: None,
                created_at_unix: 200,
            })
            .unwrap();
        store
            .append(&CheckpointRecord {
                id: "first".to_string(),
                commit_oid: "first".to_string(),
                parent_oid: None,
                label: None,
                created_at_unix: 100,
            })
            .unwrap();
        let listed = store.list().unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].id, "first");
        assert_eq!(listed[1].id, "second");
    }
}
