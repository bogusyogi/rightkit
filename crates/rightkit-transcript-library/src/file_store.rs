//! JSON file-backed history store: the fallback a host uses when its encrypted store is
//! unavailable, so it is the last line of recoverability. Every mutation persists the whole file
//! through the host's atomic writer (temp file, fsync, rename); that write-per-mutation is a
//! deliberate durability guarantee. The host chooses the file and supplies the writer.

use crate::delivery::DeliveryRecord;
use crate::history::{
    query_records, record_matches, trim_records, HistoryError, HistoryFilter, HistoryPage,
    HistoryQuery, HistoryStore,
};
use std::fs;
use std::path::{Path, PathBuf};

/// Whole-file atomic replacement: a reader must see the old file or the new one, never a mix.
pub type AtomicWrite = fn(&Path, &[u8]) -> std::io::Result<()>;

fn sort_records_newest_first(records: &mut [DeliveryRecord]) {
    records.sort_by(|left, right| {
        right
            .delivered_at_ms
            .cmp(&left.delivered_at_ms)
            .then_with(|| right.delivery_id.cmp(&left.delivery_id))
    });
}

#[derive(Debug)]
pub struct FileHistoryStore {
    path: PathBuf,
    write: AtomicWrite,
    records: Vec<DeliveryRecord>,
    state_uncertain: bool,
}

impl FileHistoryStore {
    /// Open `path` (created on first write), persisting through `write`. The parent directory is
    /// created here. An unreadable or unparseable file is an error, never an empty history.
    pub fn open(path: PathBuf, write: AtomicWrite) -> Result<Self, HistoryError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| HistoryError::new(format!("create data dir failed: {err}")))?;
        }
        let mut records = match fs::read_to_string(&path) {
            Ok(raw) => serde_json::from_str(&raw)
                .map_err(|err| HistoryError::new(format!("parse history failed: {err}")))?,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(err) => return Err(HistoryError::new(format!("read history failed: {err}"))),
        };
        sort_records_newest_first(&mut records);
        trim_records(&mut records);

        Ok(Self { path, write, records, state_uncertain: false })
    }

    /// Write the whole history file atomically through the host's writer. A direct
    /// `fs::write` truncates the destination before the new bytes land, so a
    /// crash/power-loss mid-write (or a write that fails partway) leaves a
    /// truncated or empty `history.json` that `open()` cannot parse — silently
    /// destroying every record this store has (this is the JSON fallback path,
    /// used when SQLCipher/Credential Manager is unavailable, so it is the
    /// last line of recoverability). `fs::rename` replaces the destination
    /// atomically on both POSIX and Windows (`MoveFileExW` with
    /// `MOVEFILE_REPLACE_EXISTING`), so the prior valid file is retained
    /// exactly until the new one is fully durable — the write either fully
    /// lands or the prior contents are still recoverable on next open.
    fn persist_candidate(&mut self, candidate: Vec<DeliveryRecord>) -> Result<(), HistoryError> {
        if self.state_uncertain {
            return Err(HistoryError::new("history state is uncertain after failed write"));
        }
        let raw = serde_json::to_string_pretty(&candidate)
            .map_err(|err| HistoryError::new(format!("serialize history failed: {err}")))?;
        match (self.write)(&self.path, raw.as_bytes()) {
            Ok(()) => { self.records = candidate; Ok(()) }
            Err(err) => {
                // Some atomic writers can report failure after rename (for example, directory
                // sync failure). Reconcile from disk before accepting another mutation.
                match fs::read_to_string(&self.path).ok()
                    .and_then(|contents| serde_json::from_str::<Vec<DeliveryRecord>>(&contents).ok()) {
                    Some(mut disk_records) => {
                        sort_records_newest_first(&mut disk_records);
                        trim_records(&mut disk_records);
                        self.records = disk_records;
                    }
                    None => self.state_uncertain = true,
                }
                Err(HistoryError::new(format!("write history failed: {err}")))
            }
        }
    }
}

impl HistoryStore for FileHistoryStore {
    fn push(&mut self, record: DeliveryRecord) -> Result<(), HistoryError> {
        let mut candidate = self.records.clone();
        candidate.push(record);
        sort_records_newest_first(&mut candidate);
        trim_records(&mut candidate);
        self.persist_candidate(candidate)
    }

    fn push_batch(&mut self, records: Vec<DeliveryRecord>) -> Result<(), HistoryError> {
        // Retain-once instead of per-record: the old loop was O(batch × total).
        // Within the batch itself the last occurrence of a delivery_id wins,
        // matching the old insert-per-record semantics.
        let mut seen = std::collections::HashSet::new();
        let mut deduped: Vec<DeliveryRecord> = Vec::with_capacity(records.len());
        for record in records.into_iter().rev() {
            if seen.insert(record.delivery_id.clone()) {
                deduped.push(record);
            }
        }
        let mut candidate = self.records.clone();
        candidate
            .retain(|item| !seen.contains(item.delivery_id.as_str()));
        candidate.extend(deduped);
        sort_records_newest_first(&mut candidate);
        trim_records(&mut candidate);
        self.persist_candidate(candidate)
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
        let mut candidate = self.records.clone();
        if let Some(record) = candidate
            .iter_mut()
            .find(|r| r.delivery_id == delivery_id)
        {
            record.transcript = transcript;
            self.persist_candidate(candidate)?;
            return Ok(true);
        }
        Ok(false)
    }

    fn upsert_draft(&mut self, session_id: &str, text: String) -> Result<(), HistoryError> {
        let delivery_id = crate::delivery::draft_delivery_id(session_id);
        let mut candidate = self.records.clone();
        if let Some(existing) = candidate
            .iter_mut()
            .find(|r| r.delivery_id == delivery_id)
        {
            existing.transcript = text;
            existing.delivered_at_ms = crate::delivery::now_ms();
            sort_records_newest_first(&mut candidate);
            return self.persist_candidate(candidate);
        }
        candidate
            .push(DeliveryRecord::new_draft(session_id, text));
        sort_records_newest_first(&mut candidate);
        trim_records(&mut candidate);
        self.persist_candidate(candidate)
    }

    fn delete(&mut self, delivery_id: &str) -> Result<bool, HistoryError> {
        let mut candidate = self.records.clone();
        let before = candidate.len();
        candidate
            .retain(|record| record.delivery_id != delivery_id);
        let changed = candidate.len() != before;
        if changed {
            self.persist_candidate(candidate)?;
        }
        Ok(changed)
    }

    fn query(&self, query: &HistoryQuery) -> Result<HistoryPage, HistoryError> {
        Ok(query_records(&self.records, query))
    }

    fn delete_matching(&mut self, filter: &HistoryFilter) -> Result<usize, HistoryError> {
        let mut candidate = self.records.clone();
        let before = candidate.len();
        candidate
            .retain(|record| !record_matches(record, filter));
        let deleted = before - candidate.len();
        if deleted > 0 {
            self.persist_candidate(candidate)?;
        }
        Ok(deleted)
    }

    fn prune_before(&mut self, cutoff_ms: u64) -> Result<usize, HistoryError> {
        let mut candidate = self.records.clone();
        let before = candidate.len();
        candidate
            .retain(|record| record.delivered_at_ms >= cutoff_ms);
        let deleted = before - candidate.len();
        if deleted > 0 {
            self.persist_candidate(candidate)?;
        }
        Ok(deleted)
    }

    fn clear(&mut self) -> Result<(), HistoryError> {
        self.persist_candidate(Vec::new())
    }
}
