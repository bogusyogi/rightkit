//! Durable cross-session memory store (ENG-81).
//!
//! Every write passes through [`screening::screen`] first; a rejected
//! candidate never reaches the map, so secrets/credentials and volatile
//! junk cannot be persisted, retrieved, or synced. The store is
//! backend-neutral (an in-memory `BTreeMap` keyed by id) so a daemon or the
//! mobile runtime can wrap it with whatever persistence layer (SQLite,
//! MemRight, etc.) it already owns without this crate taking a DB
//! dependency.

use crate::screening::{self, RejectReason, ScreeningVerdict};
use serde::{Deserialize, Serialize};

/// A persisted memory row of the mobile-only local store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryRecord {
    pub id: String,
    pub scope: String,
    pub content: String,
    pub digest: String,
    pub subject_key: String,
    pub created_at: String,
    pub access_count: u32,
    /// User-controlled hard pin: never evicted by capacity/age policy,
    /// mirroring `TranscriptMessage::pinned` semantics (ENG-104) but scoped
    /// to durable memory rather than the live transcript.
    pub pinned: bool,
}

/// Caller-supplied fields for a new memory write. `id`, `digest`, and
/// `subject_key` stay caller-computed so the store never re-implements hashing/keyword-normalization
/// that the daemon's existing pipeline owns.
#[derive(Debug, Clone)]
pub struct NewMemory {
    pub id: String,
    pub scope: String,
    pub content: String,
    pub digest: String,
    pub subject_key: String,
    pub created_at: String,
}

/// Why a write did not reach the store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "reason")]
pub enum WriteRejection {
    /// Failed screening; carries the specific reason for caller diagnostics
    /// (e.g. surfacing "not saved: looked like a credential" in the UI).
    Screened { detail: RejectReason },
    /// An id collision with an existing record. Callers should use
    /// delete-then-write for intentional
    /// overwrites; a bare `write` never silently clobbers.
    DuplicateId,
}

impl From<RejectReason> for WriteRejection {
    fn from(detail: RejectReason) -> Self {
        WriteRejection::Screened { detail }
    }
}

/// Durable, screened memory store.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryStore {
    records: std::collections::BTreeMap<String, MemoryRecord>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Screen and insert a new memory. Rejects duplicates and anything that
    /// fails [`screening::screen`]; both are reported, never silently
    /// dropped, so a UI can tell the user why a candidate wasn't saved.
    pub fn write(&mut self, candidate: NewMemory) -> Result<&MemoryRecord, WriteRejection> {
        if self.records.contains_key(&candidate.id) {
            return Err(WriteRejection::DuplicateId);
        }
        match screening::screen(&candidate.content) {
            ScreeningVerdict::Reject { reason } => Err(WriteRejection::from(reason)),
            ScreeningVerdict::Admit => {
                let record = MemoryRecord {
                    id: candidate.id.clone(),
                    scope: candidate.scope,
                    content: candidate.content,
                    digest: candidate.digest,
                    subject_key: candidate.subject_key,
                    created_at: candidate.created_at,
                    access_count: 0,
                    pinned: false,
                };
                self.records.insert(candidate.id.clone(), record);
                Ok(self.records.get(&candidate.id).expect("just inserted"))
            }
        }
    }

    /// Explicit user delete. Always allowed, including for pinned rows --
    /// pinning protects a memory from automatic eviction, not from the user
    /// deliberately removing it.
    pub fn delete(&mut self, id: &str) -> Option<MemoryRecord> {
        self.records.remove(id)
    }

    pub fn get(&self, id: &str) -> Option<&MemoryRecord> {
        self.records.get(id)
    }

    pub fn set_pinned(&mut self, id: &str, pinned: bool) -> bool {
        if let Some(record) = self.records.get_mut(id) {
            record.pinned = pinned;
            true
        } else {
            false
        }
    }

    pub fn record_access(&mut self, id: &str) {
        if let Some(record) = self.records.get_mut(id) {
            record.access_count = record.access_count.saturating_add(1);
        }
    }

    /// All records in a scope (or every record when `scope` is `None`),
    /// unordered (callers rank).
    pub fn list(&self, scope: Option<&str>) -> Vec<&MemoryRecord> {
        self.records
            .values()
            .filter(|record| match scope {
                Some(s) => record.scope == s,
                None => true,
            })
            .collect()
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_memory(id: &str, content: &str) -> NewMemory {
        NewMemory {
            id: id.to_string(),
            scope: "global".to_string(),
            content: content.to_string(),
            digest: format!("digest-{id}"),
            subject_key: "subject".to_string(),
            created_at: "2026-08-29T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn write_admits_durable_fact() {
        let mut store = MemoryStore::new();
        let result = store.write(new_memory("m1", "Adrian prefers pnpm for this workspace."));
        assert!(result.is_ok());
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn write_rejects_credential_without_persisting() {
        let mut store = MemoryStore::new();
        let result = store.write(new_memory("m2", concat!("api_key=sk", "-abcdef1234567890")));
        assert_eq!(
            result.unwrap_err(),
            WriteRejection::Screened {
                detail: RejectReason::Credential
            }
        );
        assert!(store.is_empty());
    }

    #[test]
    fn write_rejects_volatile_junk() {
        let mut store = MemoryStore::new();
        let result = store.write(new_memory("m3", "loading..."));
        assert_eq!(
            result.unwrap_err(),
            WriteRejection::Screened {
                detail: RejectReason::VolatileJunk
            }
        );
        assert!(store.is_empty());
    }

    #[test]
    fn duplicate_id_rejected_without_clobbering() {
        let mut store = MemoryStore::new();
        store
            .write(new_memory("m1", "Adrian prefers pnpm for this workspace."))
            .unwrap();
        let second = store.write(new_memory("m1", "A completely different durable fact."));
        assert_eq!(second.unwrap_err(), WriteRejection::DuplicateId);
        assert_eq!(store.get("m1").unwrap().access_count, 0);
    }

    #[test]
    fn delete_removes_even_pinned_record() {
        let mut store = MemoryStore::new();
        store
            .write(new_memory("m1", "Adrian prefers pnpm for this workspace."))
            .unwrap();
        assert!(store.set_pinned("m1", true));
        assert!(store.delete("m1").is_some());
        assert!(store.get("m1").is_none());
    }

    #[test]
    fn list_filters_by_scope() {
        let mut store = MemoryStore::new();
        store
            .write(new_memory("m1", "Adrian prefers pnpm for this workspace."))
            .unwrap();
        let mut other = new_memory("m2", "The desktop UI lives in apps/myapp-desktop.");
        other.scope = "project".to_string();
        store.write(other).unwrap();

        assert_eq!(store.list(Some("global")).len(), 1);
        assert_eq!(store.list(Some("project")).len(), 1);
        assert_eq!(store.list(None).len(), 2);
    }
}
