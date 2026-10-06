//! Adapter traits. Apps implement these; the engine owns the algorithm.

use std::collections::BTreeMap;

use crate::error::SyncResult;
use crate::record::RawRecord;

/// Immutable identity + version of one syncable item.
#[derive(Debug, Clone, PartialEq)]
pub struct ItemHeader {
    pub id: String,
    /// Immutable origin timestamp; ordering key for publication.
    pub created_at_ms: i64,
    /// Version used for last-writer-wins.
    pub updated_at_ms: i64,
    pub origin_device: Option<String>,
}

/// Domain <-> wire mapping for the one syncable item type.
pub trait RecordMapper: Send + Sync {
    type Item: Clone + Send + Sync;

    fn record_type(&self) -> &str;
    fn header(&self, item: &Self::Item) -> ItemHeader;
    /// Record name for an item id (default: the id itself).
    fn record_name(&self, id: &str) -> String {
        id.to_string()
    }
    /// Write the domain fields. The engine has already set `id`, `createdAt`
    /// and `updatedAt` from `header` and cleared `deletedAt`.
    fn encode(&self, item: &Self::Item, record: &mut RawRecord);
    /// Decode a live (non-tombstone) record; `Err` quarantines the record.
    fn decode(&self, record: &RawRecord) -> Result<Self::Item, String>;
    /// Extra fields a tombstone keeps beyond `id`/`updatedAt`/`deletedAt`.
    fn tombstone_keep_fields(&self) -> &[&str] {
        &[]
    }
}

/// Result of landing one downloaded item locally.
#[derive(Debug, Clone, PartialEq)]
pub enum ApplyOutcome {
    Applied,
    /// Could not land yet (engine busy, dependency missing). The engine
    /// stashes the raw record durably and retries every pass, because the
    /// change feed never replays it.
    Deferred(String),
}

/// The app's local data. All calls are synchronous.
pub trait LocalStore<T>: Send + Sync {
    fn all(&self) -> SyncResult<Vec<T>>;
    /// Remove a locally held item because of a tombstone. Idempotent.
    fn remove(&self, id: &str) -> SyncResult<()>;
    /// Upsert downloaded items; one outcome per input, in order. `Err`
    /// aborts the pass WITHOUT advancing the change token.
    fn apply_remote(&self, items: &[T]) -> SyncResult<Vec<ApplyOutcome>>;
}

/// Last-writer-wins by default. Tombstones and duplicate-alias reduction are
/// fixed by the shared contract and not pluggable here.
pub trait ConflictPolicy<T>: Send + Sync {
    /// Should the local item overwrite the remote live record?
    fn upload_local_over_remote(
        &self,
        local: &ItemHeader,
        _local_item: &T,
        remote: &ItemHeader,
        _remote_item: &T,
    ) -> bool {
        local.updated_at_ms > remote.updated_at_ms
    }
    /// Should the remote live record replace the local item?
    fn accept_remote_over_local(&self, local: &ItemHeader, remote: &ItemHeader) -> bool {
        remote.updated_at_ms > local.updated_at_ms
    }
}

pub struct LastWriterWins;
impl<T> ConflictPolicy<T> for LastWriterWins {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SavePolicy {
    /// Full-field overwrite (CloudKit `.allKeys`). The only policy the
    /// engine uses: a write never needs a read first, and deletion is
    /// authoritative.
    Overwrite,
}

#[derive(Debug, Default)]
pub struct ChangePage {
    pub records: Vec<RawRecord>,
    /// Record names hard-deleted server-side.
    pub deleted_record_names: Vec<String>,
    pub new_token: Option<Vec<u8>>,
    pub more_coming: bool,
}

#[derive(Debug, Clone)]
pub struct RecordOutcome {
    pub record_name: String,
    pub result: SyncResult<()>,
}

/// The cloud zone. Method set mirrors the iOS `CloudSyncService`.
pub trait Backend: Send + Sync {
    /// Create zone/subscription (idempotent).
    fn prepare(&self) -> SyncResult<()>;
    /// ONE delta page since `since` (None = first fetch). Report an expired
    /// token as `SyncError::TokenExpired` and a missing zone as `ZoneMissing`.
    fn fetch_changes(&self, since: Option<&[u8]>) -> SyncResult<ChangePage>;
    /// Point read, used to obtain a change tag for a known record.
    fn fetch_record(&self, record_name: &str) -> SyncResult<Option<RawRecord>>;
    /// Non-atomic batch save: one outcome per input. `Err` = whole call failed.
    fn save_records(
        &self,
        records: &[RawRecord],
        policy: SavePolicy,
    ) -> SyncResult<Vec<RecordOutcome>>;
    fn max_batch(&self) -> usize {
        200
    }
}

/// Account state and session credentials. `generation` changes on account
/// switch; an in-flight pass observing a different generation cancels itself.
pub trait AuthProvider: Send + Sync {
    /// Master switch (user toggle).
    fn enabled(&self) -> bool {
        true
    }
    fn availability(&self) -> SyncResult<()>;
    fn generation(&self) -> u64 {
        0
    }
    /// Bearer value for HTTP-style backends, fetched per pass by the app's
    /// own secure store; CloudKit backends return None.
    fn bearer(&self) -> SyncResult<Option<String>> {
        Ok(None)
    }
}

pub struct AlwaysAvailable;
impl AuthProvider for AlwaysAvailable {
    fn availability(&self) -> SyncResult<()> {
        Ok(())
    }
}

pub trait Clock: Send + Sync {
    fn now_ms(&self) -> i64;
}

pub struct SystemClock;
impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64
    }
}

/// Free-form diagnostics hook (no logging dependency baked in).
pub trait SyncObserver: Send + Sync {
    fn event(&self, name: &'static str, detail: &str);
}

pub struct NullObserver;
impl SyncObserver for NullObserver {
    fn event(&self, _name: &'static str, _detail: &str) {}
}

/// Durable app-owned scratch the engine persists inside its ledger.
pub type Extras = BTreeMap<String, serde_json::Value>;

/// Optional domain extension for record types the engine does not own
/// (device announcements, request queues, ...).
pub trait PassHooks: Send + Sync {
    /// Records to write at the start of a pass (e.g. device announce).
    /// A failure to write them fails the pass.
    fn outbound(&self, _extras: &mut Extras) -> Vec<RawRecord> {
        Vec::new()
    }
    /// Records of other types seen in this pass's delta. Returned records are
    /// written back; a failed write is reported and retried because the hook
    /// must be idempotent over the ledger extras.
    fn on_foreign_records(&self, _records: &[RawRecord], _extras: &mut Extras) -> Vec<RawRecord> {
        Vec::new()
    }
}

pub struct NoHooks;
impl PassHooks for NoHooks {}
