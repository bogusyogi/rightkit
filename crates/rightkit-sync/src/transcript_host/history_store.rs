//! A `HistorySession` over a transcript-library `HistoryStore`. Moved from HeardRight
//! `cloud_sync.rs` (`HrHistory`, `history_row`, `synced_from`, `cloud_transcript_to_delivery_record`).
//!
//! The windowed key walk, the by-id read, the delete and the synced upsert are the same for any app
//! whose history is a `HistoryStore`; the only host value is the prefix of the row id a synced
//! record is stored under (stable per CloudKit record id, NOT a fresh delivery id: repeated pulls
//! refresh the row).

use rightkit_transcript_library::delivery::{
    CopyFallbackReason, DeliveryOutcome, DeliveryRecord, DeliverySource, TargetSnapshot,
};
use rightkit_transcript_library::history::{
    HistoryFilter, HistoryQuery, HistoryStore, MAX_HISTORY_PAGE_SIZE,
};

use super::{HistoryKey, HistoryRow, HistorySession, KeyWindow};
use crate::transcripts::CloudTranscript;

pub struct StoreSession {
    store: Box<dyn HistoryStore>,
    /// Prefix of the store row id for a synced record (`<prefix><record id>`).
    row_id_prefix: String,
    /// The row count the first window of the walk in progress saw. A later window that sees a
    /// different count means history changed during the walk, and the walk fails instead of
    /// handing out an unstable view (the store pages by offset).
    walk_total: Option<usize>,
}

impl StoreSession {
    pub fn new(store: Box<dyn HistoryStore>, row_id_prefix: impl Into<String>) -> Self {
        Self {
            store,
            row_id_prefix: row_id_prefix.into(),
            walk_total: None,
        }
    }
}

/// The device a row was received from, `None` for a row recorded on this device.
fn synced_from(record: &DeliveryRecord) -> Option<String> {
    match &record.source {
        Some(DeliverySource::CloudSync { device }) => Some(device.clone()),
        _ => None,
    }
}

fn history_row(record: DeliveryRecord) -> HistoryRow {
    let synced_from = synced_from(&record);
    HistoryRow {
        delivery_id: record.delivery_id,
        text: record.transcript,
        raw_text: record.raw_transcript,
        created_at_ms: record.delivered_at_ms,
        synced_from,
    }
}

impl HistorySession for StoreSession {
    /// A window of keys. The cursor is the store's offset (history is ordered by the store's own
    /// query); the walk fails when the row count moves between windows.
    fn key_window(&mut self, cursor: Option<&str>, limit: usize) -> Result<KeyWindow, String> {
        let mut offset: u32 = match cursor {
            None => {
                self.walk_total = None;
                0
            }
            Some(text) => text
                .parse::<u32>()
                .map_err(|_| "history cursor is not an offset".to_string())?,
        };
        let mut keys: Vec<HistoryKey> = Vec::new();
        let mut more = false;
        while keys.len() < limit {
            let want = (limit - keys.len()).min(MAX_HISTORY_PAGE_SIZE as usize) as u32;
            let page = self
                .store
                .query(&HistoryQuery {
                    filter: HistoryFilter::default(),
                    offset,
                    limit: want,
                })
                .map_err(|err| err.to_string())?;
            match self.walk_total {
                None => self.walk_total = Some(page.total),
                Some(total) if total != page.total => {
                    return Err("local history changed while it was read".to_string());
                }
                Some(_) => {}
            }
            let count = page.items.len() as u32;
            more = page.has_more && count > 0;
            for record in page.items {
                let origin = synced_from(&record);
                keys.push(HistoryKey {
                    delivery_id: record.delivery_id,
                    synced_from: origin,
                });
            }
            offset = offset
                .checked_add(count)
                .ok_or_else(|| "history pagination overflow".to_string())?;
            if !more {
                break;
            }
        }
        Ok(KeyWindow {
            keys,
            next: more.then(|| offset.to_string()),
        })
    }

    /// The rows with these ids. `HistoryStore::find` has no error channel: a row the store
    /// cannot read is left out like a row that is gone.
    fn rows_by_id(&mut self, delivery_ids: &[String]) -> Result<Vec<HistoryRow>, String> {
        Ok(delivery_ids
            .iter()
            .filter_map(|delivery_id| self.store.find(delivery_id))
            .map(history_row)
            .collect())
    }

    fn delete(&mut self, delivery_ids: &[String]) -> Result<(), String> {
        for delivery_id in delivery_ids {
            self.store
                .delete(delivery_id)
                .map_err(|err| err.to_string())?;
        }
        Ok(())
    }

    fn insert_synced(&mut self, items: &[CloudTranscript]) -> Result<(), String> {
        let records = items
            .iter()
            .map(|item| synced_record(&self.row_id_prefix, item))
            .collect();
        self.store
            .push_batch(records)
            .map_err(|err| err.to_string())?;
        Ok(())
    }
}

fn synced_record(row_id_prefix: &str, item: &CloudTranscript) -> DeliveryRecord {
    let target = TargetSnapshot {
        process_id: None,
        process_name: None,
        window_title: None,
        window_handle: None,
        focused_control_handle: None,
        foreground_target: None,
        focused_text_input: None,
        is_elevated: None,
        stop_continuity: None,
        exe_path: None,
    };
    let mut record = DeliveryRecord::new(
        // Stable per CloudKit record id, NOT a fresh delivery id: repeated pulls refresh the row
        // (INSERT OR REPLACE on delivery_id).
        format!("{row_id_prefix}{}", item.id),
        item.text.clone(),
        DeliveryOutcome::CopiedFallback {
            reason: CopyFallbackReason::NoTextField,
        },
        target,
    )
    .with_source(DeliverySource::CloudSync {
        device: item.source.clone(),
    });
    // `createdAt` is immutable chronology; `updatedAt` is the version clock.
    record.delivered_at_ms = if item.created_at_ms > 0 {
        item.created_at_ms
    } else {
        item.updated_at_ms
    };
    record.raw_transcript = Some(item.raw_text.clone());
    record
}
