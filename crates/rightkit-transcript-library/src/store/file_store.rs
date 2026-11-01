//! JSON file-backed history store — the fallback used when SQLCipher /
//! Credential Manager is unavailable, so it is the last line of
//! recoverability. Every mutation persists atomically (tmp + fsync + rename);
//! that write-per-mutation is a deliberate durability guarantee.

use super::retention;
use crate::delivery::DeliveryRecord;
use crate::history::{
    query_records, record_matches, trim_records, HistoryError, HistoryFilter, HistoryPage,
    HistoryQuery, HistoryStore,
};
use std::fs;
use std::path::PathBuf;

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
    records: Vec<DeliveryRecord>,
}

impl FileHistoryStore {
    pub fn open_default() -> Result<Self, HistoryError> {
        let base = crate::host::host().data_root();
        fs::create_dir_all(&base)
            .map_err(|err| HistoryError::new(format!("create data dir failed: {err}")))?;
        Self::open(base.join("history.json"))
    }

    pub fn open(path: PathBuf) -> Result<Self, HistoryError> {
        let mut records = if path.exists() {
            let raw = fs::read_to_string(&path)
                .map_err(|err| HistoryError::new(format!("read history failed: {err}")))?;
            serde_json::from_str(&raw)
                .map_err(|err| HistoryError::new(format!("parse history failed: {err}")))?
        } else {
            Vec::new()
        };
        sort_records_newest_first(&mut records);
        trim_records(&mut records);

        Ok(Self { path, records })
    }

    /// Write the whole history file atomically: serialize, write to a sibling
    /// temp file, fsync it, rename over the real path and fsync the directory
    /// (rightkit-fs `replace_file_atomically`). A direct
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
    fn persist(&self) -> Result<(), HistoryError> {
        let raw = serde_json::to_string_pretty(&self.records)
            .map_err(|err| HistoryError::new(format!("serialize history failed: {err}")))?;
        super::atomic::replace_file_atomically(&self.path, raw.as_bytes())
            .map_err(|err| HistoryError::new(format!("write history failed: {err}")))
    }
}

impl HistoryStore for FileHistoryStore {
    fn push(&mut self, record: DeliveryRecord) -> Result<(), HistoryError> {
        self.records.push(record);
        sort_records_newest_first(&mut self.records);
        trim_records(&mut self.records);
        self.persist()
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
        self.records
            .retain(|item| !seen.contains(item.delivery_id.as_str()));
        self.records.extend(deduped);
        sort_records_newest_first(&mut self.records);
        trim_records(&mut self.records);
        self.persist()
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
            self.persist()?;
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
            sort_records_newest_first(&mut self.records);
            return self.persist();
        }
        self.records
            .push(DeliveryRecord::new_draft(session_id, text));
        sort_records_newest_first(&mut self.records);
        trim_records(&mut self.records);
        self.persist()
    }

    fn delete(&mut self, delivery_id: &str) -> Result<bool, HistoryError> {
        let before = self.records.len();
        self.records
            .retain(|record| record.delivery_id != delivery_id);
        let changed = self.records.len() != before;
        if changed {
            self.persist()?;
        }
        Ok(changed)
    }

    fn query(&self, query: &HistoryQuery) -> Result<HistoryPage, HistoryError> {
        Ok(query_records(&self.records, query))
    }

    fn delete_matching(&mut self, filter: &HistoryFilter) -> Result<usize, HistoryError> {
        let before = self.records.len();
        self.records
            .retain(|record| !record_matches(record, filter));
        let deleted = before - self.records.len();
        if deleted > 0 {
            self.persist()?;
        }
        Ok(deleted)
    }

    fn prune_before(&mut self, cutoff_ms: u64) -> Result<usize, HistoryError> {
        let deleted = retention::prune_records_before(&mut self.records, cutoff_ms);
        if deleted > 0 {
            self.persist()?;
        }
        Ok(deleted)
    }

    fn clear(&mut self) -> Result<(), HistoryError> {
        self.records.clear();
        self.persist()
    }
}
