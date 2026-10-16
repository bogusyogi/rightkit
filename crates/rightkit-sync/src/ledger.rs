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
    /// True when stored bytes existed but could not be decoded. The ledger
    /// restarts empty, which forces a full zone replay; remote tombstones
    /// are replayed with it, so deletions are not lost.
    pub recovered_from_corruption: bool,
}

impl LedgerStore {
    pub fn open(storage: Box<dyn LedgerStorage>) -> SyncResult<Self> {
        let bytes = storage.load().map_err(SyncError::Persistence)?;
        let (ledger, corrupt) = match bytes {
            None => (Ledger::default(), false),
            Some(b) => match serde_json::from_slice::<Ledger>(&b) {
                Ok(l) => (l, false),
                Err(_) => (Ledger::default(), true),
            },
        };
        Ok(Self {
            storage,
            ledger,
            recovered_from_corruption: corrupt,
        })
    }

    pub fn ledger(&self) -> &Ledger {
        &self.ledger
    }

    /// Mutate then persist; a failed write rolls the in-memory change back.
    pub fn mutate<R>(&mut self, change: impl FnOnce(&mut Ledger) -> R) -> SyncResult<R> {
        let before = self.ledger.clone();
        let out = change(&mut self.ledger);
        let bytes =
            serde_json::to_vec(&self.ledger).map_err(|e| SyncError::Persistence(e.to_string()))?;
        if let Err(e) = self.storage.save(&bytes) {
            self.ledger = before;
            return Err(SyncError::Persistence(e));
        }
        Ok(out)
    }
}
