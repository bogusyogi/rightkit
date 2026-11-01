//! History — pure store contract + in-memory store + helpers.
//!
//! Disk-backed stores (JSON file, SQLCipher) live in the host application and
//! implement the `HistoryStore` trait
//! defined here. The migration-gate decision (`migration_verified`) and the
//! cap helper (`trim_records`) are pure and unit-tested here.

use crate::delivery::DeliveryRecord;
use serde::{Deserialize, Serialize};

pub const MAX_HISTORY_ITEMS: usize = 200;
pub const DEFAULT_HISTORY_PAGE_SIZE: u32 = 50;
pub const MAX_HISTORY_PAGE_SIZE: u32 = 200;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum HistoryRetention {
    #[default]
    #[serde(rename = "forever")]
    Forever,
    #[serde(rename = "7_days")]
    SevenDays,
    #[serde(rename = "30_days")]
    ThirtyDays,
    #[serde(rename = "90_days")]
    NinetyDays,
    #[serde(rename = "1_year")]
    OneYear,
}

impl HistoryRetention {
    pub fn cutoff_ms(self, now_ms: u64) -> Option<u64> {
        const DAY_MS: u64 = 24 * 60 * 60 * 1_000;
        let days = match self {
            Self::Forever => return None,
            Self::SevenDays => 7,
            Self::ThirtyDays => 30,
            Self::NinetyDays => 90,
            Self::OneYear => 365,
        };
        Some(now_ms.saturating_sub(days * DAY_MS))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryFilter {
    #[serde(default)]
    pub search: String,
    #[serde(default)]
    pub from_ms: Option<u64>,
    #[serde(default)]
    pub until_ms: Option<u64>,
    #[serde(default)]
    pub app: String,
    /// Outcome scope for the two history tabs. `false` (default) = the
    /// Dictations tab: exclude both `DeliveryOutcome::Cancelled` (raw/L0,
    /// never delivered) and `DeliveryOutcome::Draft` (in-progress crash-recovery
    /// snapshots from `TranscriptPartial`). `true` = the Cancelled tab:
    /// EXCLUSIVELY cancelled records (drafts stay hidden). This is a scope, not
    /// an additive include — the tabs are mutually exclusive, so the count and
    /// pagination reflect exactly the rows shown (no client-side re-filtering).
    /// Defaults to `false` (serde default) so callers that omit it get Dictations.
    #[serde(default)]
    pub cancelled_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryQuery {
    #[serde(flatten)]
    pub filter: HistoryFilter,
    #[serde(default)]
    pub offset: u32,
    #[serde(default = "default_history_page_size")]
    pub limit: u32,
}

impl Default for HistoryQuery {
    fn default() -> Self {
        Self {
            filter: HistoryFilter::default(),
            offset: 0,
            limit: DEFAULT_HISTORY_PAGE_SIZE,
        }
    }
}

impl HistoryQuery {
    pub fn bounded_limit(&self) -> usize {
        self.limit.clamp(1, MAX_HISTORY_PAGE_SIZE) as usize
    }
}

fn default_history_page_size() -> u32 {
    DEFAULT_HISTORY_PAGE_SIZE
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryPage {
    pub items: Vec<DeliveryRecord>,
    pub total: usize,
    pub has_more: bool,
}

pub trait HistoryStore: Send {
    /// Re-encrypt the store under a freshly generated key. Only encrypted
    /// stores support this; the default refuses.
    fn rotate_encryption_key(&mut self) -> Result<(), HistoryError> {
        Err(HistoryError::new("this history store is not encrypted"))
    }
    fn push(&mut self, record: DeliveryRecord) -> Result<(), HistoryError>;
    /// Persist a related set with one store-level commit when supported.
    /// Default preserves existing store semantics for lightweight/test stores.
    fn push_batch(&mut self, records: Vec<DeliveryRecord>) -> Result<(), HistoryError> {
        for record in records {
            self.push(record)?;
        }
        Ok(())
    }
    /// Persist a session's final record and remove its crash-recovery draft
    /// row. The draft goes only after the final record is stored, so a failed
    /// write never loses both. Stores with transactions override this to make
    /// the replacement atomic.
    fn finalize_session(
        &mut self,
        draft_delivery_id: &str,
        record: DeliveryRecord,
    ) -> Result<(), HistoryError> {
        self.push(record)?;
        let _ = self.delete(draft_delivery_id);
        Ok(())
    }
    fn recent(&self) -> &[DeliveryRecord];
    /// Look up a record by id without mutating the store. Returns a clone so
    /// callers can diff-then-write without racing the borrow checker. The
    /// WS4 vocab hook uses this to grab the old transcript under the same
    /// lock the upcoming `update_transcript` takes, so the extracted
    /// suggestions never observe a torn read.
    fn find(&self, delivery_id: &str) -> Option<DeliveryRecord>;
    fn update_transcript(
        &mut self,
        delivery_id: &str,
        transcript: String,
    ) -> Result<bool, HistoryError>;

    /// Update only when this device owns the record's origin. Foreign synced
    /// rows are view/copy-only; enforcing that here prevents any command or
    /// alternate UI from bypassing origin authority.
    fn update_transcript_scoped(
        &mut self,
        delivery_id: &str,
        transcript: String,
        allowed_origins: &[HistoryOrigin],
    ) -> Result<ScopedUpdateOutcome, HistoryError> {
        let Some(record) = self.find(delivery_id) else {
            return Ok(ScopedUpdateOutcome::NotFound);
        };
        if !allowed_origins.contains(&record_origin(&record)) {
            return Ok(ScopedUpdateOutcome::Rejected);
        }
        Ok(if self.update_transcript(delivery_id, transcript)? {
            ScopedUpdateOutcome::Updated
        } else {
            ScopedUpdateOutcome::NotFound
        })
    }
    /// Create-or-replace the single draft row for `session_id` with the
    /// latest committed-transcript text (crash-recovery draft history rows).
    /// Exactly one row per session — repeated calls during a recording
    /// update the same row, never accumulate. See
    /// `crate::delivery::draft_delivery_id` / `DeliveryRecord::new_draft`.
    fn upsert_draft(&mut self, session_id: &str, text: String) -> Result<(), HistoryError>;
    fn delete(&mut self, delivery_id: &str) -> Result<bool, HistoryError>;
    fn query(&self, query: &HistoryQuery) -> Result<HistoryPage, HistoryError>;
    fn delete_matching(&mut self, filter: &HistoryFilter) -> Result<usize, HistoryError>;
    fn prune_before(&mut self, cutoff_ms: u64) -> Result<usize, HistoryError>;
    fn clear(&mut self) -> Result<(), HistoryError>;

    /// HR-HISTORY-006/007 data-layer enforcement: delete `delivery_id` only
    /// when its origin (see `record_origin`) is one of `allowed_origins`.
    /// This is the DATA-LAYER gate — a foreign-origin row must be rejected
    /// here even if a caller's UI failed to hide its delete affordance. On
    /// desktop, `allowed_origins` is always `&[HistoryOrigin::Mac]` (this
    /// device may only tombstone its own Mac-origin rows; iPhone/Watch rows
    /// synced in via CloudKit stay strictly view-only). Default impl built
    /// only from the existing `find`/`delete`, so every `HistoryStore`
    /// implementer (`FileHistoryStore`, `SQLCipherHistoryStore`,
    /// `MemoryHistoryStore`) gets the same enforcement for free without a
    /// per-store override.
    fn delete_scoped(
        &mut self,
        delivery_id: &str,
        allowed_origins: &[HistoryOrigin],
    ) -> Result<ScopedDeleteOutcome, HistoryError> {
        let Some(record) = self.find(delivery_id) else {
            return Ok(ScopedDeleteOutcome::NotFound);
        };
        if !allowed_origins.contains(&record_origin(&record)) {
            return Ok(ScopedDeleteOutcome::Rejected);
        }
        Ok(if self.delete(delivery_id)? {
            ScopedDeleteOutcome::Deleted
        } else {
            ScopedDeleteOutcome::NotFound
        })
    }

    /// HR-HISTORY-011 scope + HR-HISTORY-006/007 enforcement combined:
    /// "Delete all"/"Delete selected"/filtered-clear must only ever remove
    /// records this device is authorized to tombstone. First snapshots every
    /// authorized id through stable pagination, then deletes. Separating read
    /// & mutation prevents offset skips while ensuring a foreign-only first
    /// page cannot hide authorized records on later pages.
    fn delete_matching_scoped(
        &mut self,
        filter: &HistoryFilter,
        allowed_origins: &[HistoryOrigin],
    ) -> Result<usize, HistoryError> {
        let mut ids = Vec::new();
        let mut offset = 0u32;
        loop {
            let page = self.query(&HistoryQuery {
                filter: filter.clone(),
                offset,
                limit: MAX_HISTORY_PAGE_SIZE,
            })?;
            if page.items.is_empty() {
                break;
            }
            for record in &page.items {
                if allowed_origins.contains(&record_origin(record)) {
                    ids.push(record.delivery_id.clone());
                }
            }
            if !page.has_more {
                break;
            }
            offset = offset.saturating_add(page.items.len() as u32);
        }
        let mut deleted = 0usize;
        for id in ids {
            if self.delete(&id)? {
                deleted += 1;
            }
        }
        Ok(deleted)
    }
}

/// Where a history record originated, for HR-HISTORY-006/007 deletion
/// authority. Mirrors the desktop frontend's `historyDevice()`
/// (`src/pages/history/historyUtils.ts`): a record with no `source` (or an
/// explicit `DeliverySource::Dictation`/`FileTranscription`) was produced on
/// THIS Mac and defaults to `Mac`; a `DeliverySource::CloudSync { device }`
/// record carries the originating device verbatim from CloudKit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryOrigin {
    Mac,
    IPhone,
    Watch,
    Unknown,
}

/// Every Mac-side record this device is ever allowed to tombstone: only its
/// own `Mac`-origin rows (HR-HISTORY-006 — "Mac may publish tombstones only
/// for Mac-origin records"). A single named constant so every call site
/// enforces the exact same scope instead of each re-typing the slice.
pub const MAC_DELETE_AUTHORITY: &[HistoryOrigin] = &[HistoryOrigin::Mac];

/// Outcome of an origin-scoped single-row delete — distinguishes "nothing to
/// delete" from "refused because this device does not own that row" so the
/// caller can surface an accurate message instead of a generic failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopedDeleteOutcome {
    Deleted,
    /// The record does not exist (already gone, or never existed) —
    /// deleting is idempotent, same convention as the plain `delete()`.
    NotFound,
    /// The record exists but its origin is not in the caller's
    /// `allowed_origins` — a foreign-origin row stays exactly as-is.
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopedUpdateOutcome {
    Updated,
    NotFound,
    Rejected,
}

pub fn record_origin(record: &DeliveryRecord) -> HistoryOrigin {
    match record.source.as_ref() {
        Some(crate::delivery::DeliverySource::CloudSync { device }) => {
            match device.trim().to_ascii_lowercase().as_str() {
                "iphone" => HistoryOrigin::IPhone,
                "watch" => HistoryOrigin::Watch,
                "mac" => HistoryOrigin::Mac,
                _ => HistoryOrigin::Unknown,
            }
        }
        _ => HistoryOrigin::Mac,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryError(String);

impl HistoryError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl std::fmt::Display for HistoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for HistoryError {}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryHistoryStore {
    records: Vec<DeliveryRecord>,
}

impl MemoryHistoryStore {
    pub fn push(&mut self, record: DeliveryRecord) -> Result<(), HistoryError> {
        <Self as HistoryStore>::push(self, record)
    }

    pub fn recent(&self) -> &[DeliveryRecord] {
        <Self as HistoryStore>::recent(self)
    }

    pub fn query(&self, query: &HistoryQuery) -> Result<HistoryPage, HistoryError> {
        <Self as HistoryStore>::query(self, query)
    }

    pub fn delete_matching(&mut self, filter: &HistoryFilter) -> Result<usize, HistoryError> {
        <Self as HistoryStore>::delete_matching(self, filter)
    }

    pub fn upsert_draft(&mut self, session_id: &str, text: String) -> Result<(), HistoryError> {
        <Self as HistoryStore>::upsert_draft(self, session_id, text)
    }
}

impl HistoryStore for MemoryHistoryStore {
    fn push(&mut self, record: DeliveryRecord) -> Result<(), HistoryError> {
        self.records.insert(0, record);
        trim_records(&mut self.records);
        Ok(())
    }

    fn recent(&self) -> &[DeliveryRecord] {
        &self.records
    }

    fn find(&self, delivery_id: &str) -> Option<DeliveryRecord> {
        self.records
            .iter()
            .find(|r| r.delivery_id == delivery_id)
            .cloned()
    }

    fn update_transcript(
        &mut self,
        delivery_id: &str,
        transcript: String,
    ) -> Result<bool, HistoryError> {
        if let Some(record) = self
            .records
            .iter_mut()
            .find(|r| r.delivery_id == delivery_id)
        {
            record.transcript = transcript;
            return Ok(true);
        }
        Ok(false)
    }

    fn upsert_draft(&mut self, session_id: &str, text: String) -> Result<(), HistoryError> {
        let delivery_id = crate::delivery::draft_delivery_id(session_id);
        if let Some(existing) = self
            .records
            .iter_mut()
            .find(|r| r.delivery_id == delivery_id)
        {
            existing.transcript = text;
            existing.delivered_at_ms = crate::delivery::now_ms();
            return Ok(());
        }
        self.records
            .insert(0, DeliveryRecord::new_draft(session_id, text));
        trim_records(&mut self.records);
        Ok(())
    }

    fn delete(&mut self, delivery_id: &str) -> Result<bool, HistoryError> {
        let before = self.records.len();
        self.records
            .retain(|record| record.delivery_id != delivery_id);
        Ok(self.records.len() != before)
    }

    fn query(&self, query: &HistoryQuery) -> Result<HistoryPage, HistoryError> {
        Ok(query_records(&self.records, query))
    }

    fn delete_matching(&mut self, filter: &HistoryFilter) -> Result<usize, HistoryError> {
        let before = self.records.len();
        self.records
            .retain(|record| !record_matches(record, filter));
        Ok(before - self.records.len())
    }

    fn prune_before(&mut self, cutoff_ms: u64) -> Result<usize, HistoryError> {
        let before = self.records.len();
        self.records
            .retain(|record| record.delivered_at_ms >= cutoff_ms);
        Ok(before - self.records.len())
    }

    fn clear(&mut self) -> Result<(), HistoryError> {
        self.records.clear();
        Ok(())
    }
}

pub fn query_records(records: &[DeliveryRecord], query: &HistoryQuery) -> HistoryPage {
    let matching: Vec<_> = records
        .iter()
        .filter(|record| record_matches(record, &query.filter))
        .cloned()
        .collect();
    let total = matching.len();
    let offset = query.offset as usize;
    let limit = query.bounded_limit();
    let items = matching.into_iter().skip(offset).take(limit).collect();
    HistoryPage {
        items,
        total,
        has_more: offset.saturating_add(limit) < total,
    }
}

pub fn record_matches(record: &DeliveryRecord, filter: &HistoryFilter) -> bool {
    if filter.cancelled_only {
        // Cancelled tab: only cancelled records (drafts excluded too).
        if !matches!(record.outcome, crate::delivery::DeliveryOutcome::Cancelled) {
            return false;
        }
    } else if matches!(
        record.outcome,
        crate::delivery::DeliveryOutcome::Cancelled | crate::delivery::DeliveryOutcome::Draft
    ) {
        // Dictations tab: hide both cancelled and in-progress drafts.
        return false;
    }

    if filter
        .from_ms
        .is_some_and(|from| record.delivered_at_ms < from)
        || filter
            .until_ms
            .is_some_and(|until| record.delivered_at_ms >= until)
    {
        return false;
    }

    if !filter.app.trim().is_empty()
        && !history_target_label(record).eq_ignore_ascii_case(filter.app.trim())
    {
        return false;
    }

    let search = filter.search.trim().to_ascii_lowercase();
    if search.is_empty() {
        return true;
    }
    let haystack = serde_json::to_string(record)
        .unwrap_or_default()
        .to_ascii_lowercase();
    haystack.contains(&search)
}

pub fn history_target_label(record: &DeliveryRecord) -> String {
    match record.source.as_ref() {
        Some(crate::delivery::DeliverySource::FileTranscription { file_name }) => {
            if file_name.is_empty() {
                "File transcription".to_string()
            } else {
                format!("File · {file_name}")
            }
        }
        _ => record
            .target
            .process_name
            .clone()
            .or_else(|| record.target.window_title.clone())
            .unwrap_or_else(|| "Unknown app".to_string()),
    }
}

/// Cap the in-memory ring to `MAX_HISTORY_ITEMS` (newest-first; truncates the
/// tail). `pub` so the host's disk-backed stores reuse it.
pub fn trim_records(records: &mut Vec<DeliveryRecord>) {
    if records.len() > MAX_HISTORY_ITEMS {
        records.truncate(MAX_HISTORY_ITEMS);
    }
}

/// Pure migration-gate decision. Returns true when it's safe to shred the
/// plaintext `history.json`: the SQLCipher DB row count must equal the number
/// of DISTINCT `delivery_id`s in the JSON. `INSERT OR IGNORE` collapses
/// duplicate ids (and drops any record whose serialization failed), so compare
/// against the distinct count — not `records.len()`. If any record failed to
/// migrate, `row_count < distinct` and the caller keeps the `.bak`.
pub fn migration_verified(records: &[DeliveryRecord], db_row_count: i64) -> bool {
    let distinct: std::collections::HashSet<&str> =
        records.iter().map(|r| r.delivery_id.as_str()).collect();
    db_row_count >= 0 && db_row_count == distinct.len() as i64
}

#[cfg(test)]
#[path = "history_tests.rs"]
mod tests;
