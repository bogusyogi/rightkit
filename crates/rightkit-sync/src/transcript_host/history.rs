//! The history adapter: the engine's view of the host's history, built from bounded reads.
//!
//! History is never loaded whole. The engine scans its local store in pages in ascending order of
//! the logical id (`LocalStore::list_page`), a hash of the host's row id that the host's store
//! cannot sort by. So the adapter keeps, for the length of one pass, an index from logical id to
//! host row id (and the origin of the row), built by walking the host's keys in windows
//! (`HistorySession::key_window`; no row text). Each engine page is then read by id
//! (`rows_by_id`) for at most [`LOCAL_WINDOW_ROWS`] rows.
//!
//! Bounds: the index holds at most `max_history_rows` keys (the walk fails above that, nothing is
//! deleted), about 150 bytes per entry (estimated, not measured); row text is held for one window
//! at a time. The index is dropped at the start and the end of every pass, so it never describes
//! history older than the pass that uses it.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound;
use std::sync::{Arc, Mutex};

use super::traits::{HistoryKey, HistoryRow, HistorySession, TranscriptHost};
use crate::transcripts::{
    self as sync_core, CloudTranscript, LocalVersions, TranscriptSyncProfile,
};
use crate::{ApplyOutcome, LocalStore, SyncError, SyncResult};

/// Keys asked of the host per `key_window` call.
pub(super) const KEY_WINDOW_ROWS: usize = 2_000;
/// Rows of text read and handed to the engine per `list_page` call.
pub(super) const LOCAL_WINDOW_ROWS: usize = 500;

fn persistence<E: std::fmt::Display>(err: E) -> SyncError {
    SyncError::Persistence(err.to_string())
}

/// Walk the host's keys window by window, calling `visit` for each. Stops with an error when a
/// window is larger than asked, makes no progress, or the walk passes `max_rows` keys. Repeated
/// keys count each time, so a host that repeats rows cannot make the walk longer than the limit.
pub(super) fn scan_keys(
    history: &mut dyn HistorySession,
    max_rows: usize,
    mut visit: impl FnMut(HistoryKey),
) -> Result<(), String> {
    let mut cursor: Option<String> = None;
    let mut seen = 0usize;
    loop {
        let window = history.key_window(cursor.as_deref(), KEY_WINDOW_ROWS)?;
        if window.keys.len() > KEY_WINDOW_ROWS {
            return Err("history window larger than requested".to_string());
        }
        let empty = window.keys.is_empty();
        for key in window.keys {
            seen = seen.saturating_add(1);
            if seen > max_rows {
                return Err("local history exceeds the sync row limit".to_string());
            }
            visit(key);
        }
        match window.next {
            None => return Ok(()),
            Some(next) => {
                if empty || cursor.as_deref() == Some(next.as_str()) {
                    return Err("history window made no progress".to_string());
                }
                cursor = Some(next);
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Recorded on this device.
    Local,
    /// Received through sync from another device.
    Cloud,
    /// Received through sync from a Mac (an earlier copy of a row of this device's own).
    CloudMac,
}

impl Kind {
    fn of(synced_from: &Option<String>) -> Self {
        match synced_from {
            None => Kind::Local,
            Some(device) if sync_core::cloud_source_is_mac(device) => Kind::CloudMac,
            Some(_) => Kind::Cloud,
        }
    }

    /// Lower wins a logical-id collision: a locally recorded row beats a received one.
    fn rank(self) -> u8 {
        match self {
            Kind::Local => 0,
            Kind::Cloud | Kind::CloudMac => 1,
        }
    }
}

/// The rows that share one logical id. `delivery_id` is the winning row; an empty one marks a row
/// this pass inserted, whose host id is not known.
struct Slot {
    delivery_id: String,
    kind: Kind,
    /// Other rows with the same logical id (a legacy duplicate); empty in the normal case.
    others: Vec<(String, Kind)>,
}

struct HistoryIndex {
    slots: BTreeMap<String, Slot>,
    /// Received copies (from a Mac) of a row this device recorded: deleted by the first
    /// `apply_remote` of the pass.
    duplicates: Vec<String>,
}

impl HistoryIndex {
    fn insert(&mut self, logical: String, delivery_id: String, kind: Kind) {
        match self.slots.entry(logical) {
            Entry::Vacant(vacant) => {
                vacant.insert(Slot {
                    delivery_id,
                    kind,
                    others: Vec::new(),
                });
            }
            Entry::Occupied(mut occupied) => {
                let slot = occupied.get_mut();
                if slot.delivery_id == delivery_id
                    || slot.others.iter().any(|(other, _)| *other == delivery_id)
                {
                    return;
                }
                let better = kind.rank() < slot.kind.rank()
                    || (kind.rank() == slot.kind.rank() && delivery_id < slot.delivery_id);
                if better {
                    let old_id = std::mem::replace(&mut slot.delivery_id, delivery_id);
                    let old_kind = std::mem::replace(&mut slot.kind, kind);
                    slot.others.push((old_id, old_kind));
                } else {
                    slot.others.push((delivery_id, kind));
                }
            }
        }
    }

    fn finish(&mut self) {
        let mut duplicates = Vec::new();
        for slot in self.slots.values() {
            if slot.kind == Kind::Local {
                duplicates.extend(
                    slot.others
                        .iter()
                        .filter(|(_, kind)| *kind == Kind::CloudMac)
                        .map(|(id, _)| id.clone()),
                );
            }
        }
        self.duplicates = duplicates;
    }

    fn has_local(&self, logical: &str) -> bool {
        self.slots
            .get(logical)
            .is_some_and(|slot| slot.kind == Kind::Local)
    }

    /// Every host row id of the logical id. `None`: one of them is a row inserted in this pass
    /// whose host id is not known (the caller builds the index again).
    fn delivery_ids(&self, logical: &str) -> Option<Vec<String>> {
        let Some(slot) = self.slots.get(logical) else {
            return Some(Vec::new());
        };
        let mut ids = vec![slot.delivery_id.clone()];
        ids.extend(slot.others.iter().map(|(id, _)| id.clone()));
        if ids.iter().any(String::is_empty) {
            None
        } else {
            Some(ids)
        }
    }

    fn forget(&mut self, logical: &str) {
        self.slots.remove(logical);
    }

    fn note_inserted(&mut self, logical: &str) {
        if !self.slots.contains_key(logical) {
            self.slots.insert(
                logical.to_string(),
                Slot {
                    delivery_id: String::new(),
                    kind: Kind::Cloud,
                    others: Vec::new(),
                },
            );
        }
    }
}

pub(super) struct HistoryLocalStore {
    host: Arc<dyn TranscriptHost>,
    profile: TranscriptSyncProfile,
    local_source: String,
    max_rows: usize,
    event_sink: fn(&str, &str),
    versions: Mutex<LocalVersions>,
    /// Logical ids whose deletion is queued for the account the running pass is for, or whose
    /// owner account is not yet proven (the unverified partition), and not yet acknowledged. A
    /// pulled copy of one is not re-inserted into history. Set by the host for one engine pass,
    /// empty otherwise.
    suppressed: Arc<Mutex<BTreeSet<String>>>,
    index: Mutex<Option<HistoryIndex>>,
}

impl HistoryLocalStore {
    pub(super) fn new(
        host: Arc<dyn TranscriptHost>,
        profile: TranscriptSyncProfile,
        local_source: String,
        max_rows: usize,
        event_sink: fn(&str, &str),
        versions: LocalVersions,
        suppressed: Arc<Mutex<BTreeSet<String>>>,
    ) -> Self {
        Self {
            host,
            profile,
            local_source,
            max_rows,
            event_sink,
            versions: Mutex::new(versions),
            suppressed,
            index: Mutex::new(None),
        }
    }

    fn emit(&self, name: &str, detail: &str) {
        (self.event_sink)(name, &super::safe_detail(detail));
    }

    /// Start of a pass: nothing of an earlier pass is remembered.
    pub(super) fn begin_pass(&self) {
        self.drop_index();
    }

    /// End of a pass: drop the index and write the version book if it changed. A failed write
    /// is reported and loses nothing but a version stamp that the next scan recomputes.
    pub(super) fn end_pass(&self) {
        self.drop_index();
        if let Ok(mut versions) = self.versions.lock() {
            if let Err(error) = versions.save_if_dirty() {
                self.emit("cloud_sync_versions_save_failed", &error);
            }
        }
    }

    fn drop_index(&self) {
        if let Ok(mut index) = self.index.lock() {
            *index = None;
        }
    }

    fn build_index(&self, history: &mut dyn HistorySession) -> SyncResult<HistoryIndex> {
        let mut index = HistoryIndex {
            slots: BTreeMap::new(),
            duplicates: Vec::new(),
        };
        scan_keys(history, self.max_rows, |key| {
            let logical = self.profile.logical_cloud_id(&key.delivery_id);
            let kind = Kind::of(&key.synced_from);
            index.insert(logical, key.delivery_id, kind);
        })
        .map_err(persistence)?;
        index.finish();
        Ok(index)
    }

    /// Run `f` on the index of this pass, building it first when there is none.
    fn with_index<R>(
        &self,
        history: &mut dyn HistorySession,
        f: impl FnOnce(&mut HistoryIndex) -> R,
    ) -> SyncResult<R> {
        let mut guard = self
            .index
            .lock()
            .map_err(|_| persistence("history index lock poisoned"))?;
        if guard.is_none() {
            *guard = Some(self.build_index(history)?);
        }
        match guard.as_mut() {
            Some(index) => Ok(f(index)),
            None => Err(persistence("history index unavailable")),
        }
    }

    /// The engine's item for one row; records the content in the version book.
    fn item_for(
        &self,
        versions: &mut LocalVersions,
        id: String,
        row: HistoryRow,
        now: u64,
    ) -> CloudTranscript {
        let source = match &row.synced_from {
            Some(device) => device.clone(),
            None => self.local_source.clone(),
        };
        let raw_text = row.raw_text.clone().unwrap_or_else(|| row.text.clone());
        let hash = sync_core::content_hash(&row.text, &raw_text);
        let updated = versions.observe(&id, &hash, row.created_at_ms, now);
        CloudTranscript {
            id,
            text: row.text,
            raw_text,
            created_at_ms: row.created_at_ms,
            updated_at_ms: updated,
            source,
            tags: Vec::new(),
            folder: None,
        }
    }
}

impl LocalStore<CloudTranscript> for HistoryLocalStore {
    /// Never used: `list_page` always answers, so the engine does not fall back to a whole read.
    fn all(&self) -> SyncResult<Vec<CloudTranscript>> {
        Err(persistence("whole-history read refused"))
    }

    fn list_page(
        &self,
        after: Option<&str>,
        limit: usize,
    ) -> SyncResult<Option<Vec<CloudTranscript>>> {
        let limit = limit.max(1);
        let mut history = self.host.open_history().map_err(persistence)?;
        let now = self.host.now_ms();
        let mut cursor: Option<String> = after.map(str::to_string);
        let mut items: Vec<CloudTranscript> = Vec::with_capacity(limit.min(LOCAL_WINDOW_ROWS));
        // Set once the index is exhausted: every live id is known, so the version book can be
        // pruned.
        let mut live: Option<BTreeSet<String>> = None;
        // A row deleted since the index was built is left out and the window is filled from the
        // entries after it, so a short page always means the end (the engine reads it that way).
        while items.len() < limit && live.is_none() {
            let need = limit - items.len();
            let (picked, exhausted) = self.with_index(&mut *history, |index| {
                let lower = match cursor.as_deref() {
                    Some(last) => Bound::Excluded(last),
                    None => Bound::Unbounded,
                };
                let picked: Vec<(String, String)> = index
                    .slots
                    .range::<str, _>((lower, Bound::Unbounded))
                    .take(need)
                    .map(|(logical, slot)| (logical.clone(), slot.delivery_id.clone()))
                    .collect();
                let exhausted: Option<BTreeSet<String>> =
                    (picked.len() < need).then(|| index.slots.keys().cloned().collect());
                (picked, exhausted)
            })?;
            live = exhausted;
            if let Some((last, _)) = picked.last() {
                cursor = Some(last.clone());
            }
            let wanted: Vec<String> = picked
                .iter()
                .map(|(_, delivery)| delivery.clone())
                .filter(|delivery| !delivery.is_empty())
                .collect();
            let rows = if wanted.is_empty() {
                Vec::new()
            } else {
                history.rows_by_id(&wanted).map_err(persistence)?
            };
            let mut by_delivery: BTreeMap<String, HistoryRow> = rows
                .into_iter()
                .map(|row| (row.delivery_id.clone(), row))
                .collect();
            let mut versions = self
                .versions
                .lock()
                .map_err(|_| persistence("versions lock poisoned"))?;
            for (logical, delivery) in picked {
                if let Some(row) = by_delivery.remove(&delivery) {
                    items.push(self.item_for(&mut versions, logical, row, now));
                }
            }
        }
        if let Some(live) = live {
            let mut versions = self
                .versions
                .lock()
                .map_err(|_| persistence("versions lock poisoned"))?;
            versions.retain_ids(&live);
            versions.save_if_dirty().map_err(persistence)?;
        }
        Ok(Some(items))
    }

    fn remove(&self, id: &str) -> SyncResult<()> {
        let mut history = self.host.open_history().map_err(persistence)?;
        let mut ids = self.with_index(&mut *history, |index| index.delivery_ids(id))?;
        if ids.is_none() {
            // A row inserted earlier in this pass: read the host's keys again.
            self.drop_index();
            ids = self.with_index(&mut *history, |index| index.delivery_ids(id))?;
        }
        let ids = ids.unwrap_or_default();
        if ids.is_empty() {
            return Ok(());
        }
        if let Err(error) = history.delete(&ids) {
            self.drop_index();
            return Err(persistence(error));
        }
        if let Ok(mut guard) = self.index.lock() {
            if let Some(index) = guard.as_mut() {
                index.forget(id);
            }
        }
        self.host.history_changed();
        Ok(())
    }

    fn apply_remote(&self, items: &[CloudTranscript]) -> SyncResult<Vec<ApplyOutcome>> {
        if items.is_empty() {
            return Ok(Vec::new());
        }
        // The host's activity gate owns the history boundary: defer; the engine stashes the
        // records durably and retries next pass.
        if let Some(reason) = self.host.busy_reason() {
            self.emit(
                "cloud_sync_deferred",
                &format!("phase=apply_remote reason={reason}"),
            );
            return Ok(items
                .iter()
                .map(|_| ApplyOutcome::Deferred(reason.to_string()))
                .collect());
        }
        let mut history = self.host.open_history().map_err(persistence)?;
        // Received copies of this device's own rows are duplicates; the first call of a pass
        // removes them.
        let duplicates =
            self.with_index(&mut *history, |index| std::mem::take(&mut index.duplicates))?;
        if !duplicates.is_empty() {
            if let Err(error) = history.delete(&duplicates) {
                self.drop_index();
                return Err(persistence(error));
            }
        }
        // Foreign synced rows are view-only on desktop, so a remote copy of a logical id that a
        // locally recorded row owns never creates a second row.
        let owned_locally: Vec<bool> = self.with_index(&mut *history, |index| {
            items.iter().map(|item| index.has_local(&item.id)).collect()
        })?;
        let mut skipped = 0u32;
        let mut held_back = 0u32;
        let mut applied: Vec<CloudTranscript> = Vec::new();
        {
            let suppressed = self
                .suppressed
                .lock()
                .map_err(|_| persistence("suppressed lock poisoned"))?;
            for (item, owned) in items.iter().zip(owned_locally) {
                if owned {
                    skipped += 1;
                } else if suppressed.contains(&item.id) {
                    held_back += 1;
                } else {
                    applied.push(item.clone());
                }
            }
        }
        let inserted = applied.len() as u32;
        if let Err(error) = history.insert_synced(&applied) {
            self.drop_index();
            return Err(persistence(error));
        }
        self.host.history_changed();
        if let Ok(mut guard) = self.index.lock() {
            if let Some(index) = guard.as_mut() {
                for item in &applied {
                    index.note_inserted(&item.id);
                }
            }
        }
        {
            // Written at the end of the pass (`end_pass`): a crash before that costs one repeated
            // download of rows the next scan finds older than their remote copy.
            let mut versions = self
                .versions
                .lock()
                .map_err(|_| persistence("versions lock poisoned"))?;
            for item in &applied {
                versions.applied(
                    &item.id,
                    &sync_core::content_hash(&item.text, &item.raw_text),
                    item.updated_at_ms,
                );
            }
        }
        let (mut mac, mut iphone, mut watch, mut unknown) = (0u32, 0u32, 0u32, 0u32);
        for item in items {
            match item.source.trim().to_ascii_lowercase().as_str() {
                "mac" | "macos" | "desktop" => mac += 1,
                "iphone" | "ios" | "phone" => iphone += 1,
                "watch" | "watchos" => watch += 1,
                _ => unknown += 1,
            }
        }
        self.emit(
            "cloud_sync_pull_sources",
            &format!(
                "total={} mac={mac} iphone={iphone} watch={watch} unknown={unknown}",
                items.len()
            ),
        );
        if held_back > 0 {
            self.emit(
                "cloud_sync_pull_held_for_deletion",
                &format!("held_back={held_back}"),
            );
        }
        let removed = duplicates.len();
        if skipped > 0 || removed > 0 {
            self.emit(
                "cloud_sync_self_origin_reconciled",
                &format!("skipped={skipped} removed={removed}"),
            );
        }
        self.emit(
            "cloud_sync_history_merged",
            &format!("incoming_records={} records={inserted}", items.len()),
        );
        Ok(items.iter().map(|_| ApplyOutcome::Applied).collect())
    }
}
