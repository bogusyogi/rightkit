//! The durable sync ledger: change token, tombstones, remote versions,
//! offline queues. One atomic document, so token and version cache can never
//! disagree (iOS keeps them separate and needs a replay rule for that).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::error::{SyncError, SyncResult};
use crate::record::RawRecord;
use crate::storage::LedgerStorage;
use crate::traits::Extras;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RemoteVersion {
    pub record_name: String,
    pub updated_at_ms: i64,
}

/// A change of the signed-in account or sync scope that the app has not yet consented to.
/// `from` is the scope the local store last synced with; `to` is the scope now signed in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsentRequest {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Ledger {
    /// Account scope this ledger belongs to (`AuthProvider::account_scope`).
    pub scope: Option<String>,
    /// Set while sync is paused on a scope change awaiting consent
    /// (`SyncConfig::require_consent_on_scope_change`). Durable with the bound `scope`.
    pub pending_consent: Option<ConsentRequest>,
    /// Item ids that existed locally when the user chose `ConsentChoice::StartFresh`: they stay
    /// on this device and are never published to the new account.
    pub withheld: BTreeSet<String>,
    pub change_token: Option<Vec<u8>>,
    pub has_synced: bool,
    /// Item id -> deletion time. Blocks re-import and re-upload forever.
    pub tombstones: BTreeMap<String, i64>,
    /// Tombstones still to be written to the zone.
    pub pending_tombstones: BTreeSet<String>,
    /// Last version known to exist remotely (survives cursor-only deltas).
    pub remote_versions: BTreeMap<String, RemoteVersion>,
    /// Logical id -> every record name seen for it (legacy duplicates).
    pub aliases: BTreeMap<String, BTreeSet<String>>,
    /// Offline queue: ids with local changes not yet confirmed uploaded.
    pub pending_uploads: BTreeSet<String>,
    /// Downloaded records whose local apply was deferred.
    pub pending_apply: BTreeMap<String, RawRecord>,
    /// Wall-clock ms before which no pass runs (backoff / server hint).
    pub retry_after_ms: Option<i64>,
    pub consecutive_failures: u32,
    pub quarantined_total: u64,
    /// One-shot migration markers (e.g. a full replay already done).
    pub flags: BTreeSet<String>,
    pub extras: Extras,
}

impl Ledger {
    /// A deletion anywhere: forget pending work for the id, block it.
    pub fn bury(&mut self, id: &str, deleted_at_ms: i64) {
        self.tombstones.insert(id.to_string(), deleted_at_ms);
        self.pending_uploads.remove(id);
        self.pending_apply.remove(id);
    }
}

pub struct LedgerStore {
    storage: Box<dyn LedgerStorage>,
    ledger: Ledger,
    /// The bytes last persisted (or loaded intact): the rollback source. `None` while the
    /// in-memory ledger is the empty default that was never written or was reset after
    /// corruption. Keeping the encoded form instead of cloning the ledger per mutation bounds
    /// the cost to one compact buffer that each successful save replaces.
    saved: Option<Vec<u8>>,
    /// True when stored bytes existed but could not be decoded. The ledger
    /// restarts empty, which forces a full zone replay; remote tombstones
    /// are replayed with it, so deletions are not lost.
    pub recovered_from_corruption: bool,
}

impl LedgerStore {
    pub fn open(storage: Box<dyn LedgerStorage>) -> SyncResult<Self> {
        let bytes = storage.load().map_err(SyncError::Persistence)?;
        let (ledger, saved, corrupt) = match bytes {
            None => (Ledger::default(), None, false),
            Some(b) => match serde_json::from_slice::<Ledger>(&b) {
                Ok(l) => (l, Some(b), false),
                Err(_) => (Ledger::default(), None, true),
            },
        };
        Ok(Self {
            storage,
            ledger,
            saved,
            recovered_from_corruption: corrupt,
        })
    }

    pub fn ledger(&self) -> &Ledger {
        &self.ledger
    }

    /// Mutate then persist; a failed write rolls the in-memory change back.
    pub fn mutate<R>(&mut self, change: impl FnOnce(&mut Ledger) -> R) -> SyncResult<R> {
        let out = change(&mut self.ledger);
        let bytes = match serde_json::to_vec(&self.ledger) {
            Ok(b) => b,
            Err(e) => {
                self.rollback();
                return Err(SyncError::Persistence(e.to_string()));
            }
        };
        if let Err(e) = self.storage.save(&bytes) {
            self.rollback();
            return Err(SyncError::Persistence(e));
        }
        self.saved = Some(bytes);
        Ok(out)
    }

    /// Restore the in-memory ledger to the last persisted state (the decode `open` uses).
    fn rollback(&mut self) {
        self.ledger = match &self.saved {
            None => Ledger::default(),
            Some(b) => serde_json::from_slice(b).unwrap_or_default(),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    /// Fails every save after the first `ok_saves` succeed.
    struct Flaky {
        inner: MemoryStorage,
        ok_saves: u32,
        seen: Arc<AtomicU32>,
    }

    impl LedgerStorage for Flaky {
        fn load(&self) -> Result<Option<Vec<u8>>, String> {
            self.inner.load()
        }
        fn save(&self, bytes: &[u8]) -> Result<(), String> {
            if self.seen.fetch_add(1, Ordering::SeqCst) >= self.ok_saves {
                return Err("disk full".into());
            }
            self.inner.save(bytes)
        }
    }

    fn flaky_over(mem: &MemoryStorage, ok_saves: u32) -> LedgerStore {
        LedgerStore::open(Box::new(Flaky {
            inner: mem.clone(),
            ok_saves,
            seen: Arc::new(AtomicU32::new(0)),
        }))
        .unwrap()
    }

    fn encoded(l: &Ledger) -> Vec<u8> {
        serde_json::to_vec(l).unwrap()
    }

    #[test]
    fn failed_save_restores_the_exact_prior_state_mid_apply() {
        let mem = MemoryStorage::default();
        let mut store = flaky_over(&mem, 2);
        store
            .mutate(|l| {
                l.pending_uploads.insert("a".into());
                l.bury("gone", 7);
                l.change_token = Some(vec![1, 2, 3]);
            })
            .unwrap();
        store
            .mutate(|l| {
                l.pending_uploads.insert("b".into());
                l.flags.insert("x".into());
            })
            .unwrap();
        let before = encoded(store.ledger());
        let stored_before = mem.load().unwrap();

        // Partial apply: the closure changes several fields, then the save fails.
        let err = store
            .mutate(|l| {
                l.pending_uploads.clear();
                l.tombstones.clear();
                l.change_token = None;
                l.has_synced = true;
                l.consecutive_failures = 9;
            })
            .unwrap_err();
        assert!(matches!(err, SyncError::Persistence(_)));
        assert_eq!(encoded(store.ledger()), before);
        assert_eq!(mem.load().unwrap(), stored_before);

        // Repeated failures keep restoring the same state.
        assert!(store.mutate(|l| l.retry_after_ms = Some(5)).is_err());
        assert_eq!(encoded(store.ledger()), before);
    }

    #[test]
    fn failed_first_save_restores_the_empty_ledger() {
        let mem = MemoryStorage::default();
        let mut store = flaky_over(&mem, 0);
        assert!(store.mutate(|l| l.has_synced = true).is_err());
        assert_eq!(encoded(store.ledger()), encoded(&Ledger::default()));
        assert!(mem.load().unwrap().is_none());
    }

    #[test]
    fn rollback_after_corrupt_open_restores_the_empty_ledger() {
        let mem = MemoryStorage::default();
        mem.save(b"not json").unwrap();
        let mut store = flaky_over(&mem, 0);
        assert!(store.recovered_from_corruption);
        assert!(store.mutate(|l| l.has_synced = true).is_err());
        assert!(!store.ledger().has_synced);
    }

    #[test]
    fn rollback_after_open_of_a_stored_ledger_restores_it() {
        let mem = MemoryStorage::default();
        let mut seeded = Ledger::default();
        seeded.pending_uploads.insert("keep".into());
        mem.save(&encoded(&seeded)).unwrap();
        let mut store = flaky_over(&mem, 0);
        assert!(store.mutate(|l| l.pending_uploads.clear()).is_err());
        assert!(store.ledger().pending_uploads.contains("keep"));
    }
}
