//! The sync pass. One `run_pass` mirrors the iOS `sync()` actor method and the
//! desktop `Coordinator::sync_pass`; see API.md for the shared contract.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;

use crate::backoff::BackoffPolicy;
use crate::error::{SyncError, SyncResult};
use crate::ledger::{LedgerStore, RemoteVersion};
use crate::record::{field, read_tombstone, write_tombstone, FieldValue, RawRecord};
use crate::storage::LedgerStorage;
use crate::traits::*;

#[derive(Debug, Clone)]
pub struct SyncConfig {
    /// Upload chunk size for edits to records that already exist remotely.
    pub batch_size: usize,
    /// Tombstone write chunk size.
    pub tombstone_batch: usize,
    /// Publish never-seen items one record per save, oldest first, and stop
    /// new publication for the pass after the first failure (preserves
    /// history order: CloudKit gives no ordering inside one operation).
    pub ordered_new_publication: bool,
    /// Treat a known-published item that vanished from the local store as a
    /// local deletion (desktop). Apps with explicit `note_deleted` calls
    /// (iOS) leave this off.
    pub infer_local_deletions: bool,
    pub backoff: BackoffPolicy,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            batch_size: 200,
            tombstone_batch: 50,
            ordered_new_publication: true,
            infer_local_deletions: false,
            backoff: BackoffPolicy::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum PassOutcome {
    /// Everything applied; token advanced; no pending work.
    Synced,
    /// Token advanced but some uploads/tombstones/applies remain queued.
    Pending(String),
    /// Backoff window still open; nothing was attempted.
    Deferred { secs: f64 },
    Unavailable(String),
    Disabled,
    /// Auth generation changed or sync was switched off mid-pass.
    Cancelled,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct PassReport {
    pub outcome: PassOutcome,
    pub uploaded: usize,
    pub downloaded: usize,
    pub tombstones_applied: usize,
    pub tombstones_written: usize,
    pub quarantined: usize,
    pub blocked_reuploads: usize,
    /// Seconds until the next pass should run, when one is scheduled.
    pub retry_after_secs: Option<f64>,
}

impl PassReport {
    fn new() -> Self {
        Self {
            outcome: PassOutcome::Synced,
            uploaded: 0,
            downloaded: 0,
            tombstones_applied: 0,
            tombstones_written: 0,
            quarantined: 0,
            blocked_reuploads: 0,
            retry_after_secs: None,
        }
    }
}

enum RemoteState<T> {
    Live(T),
    Tombstone,
}

struct RemoteEntry<T> {
    id: String,
    record_name: String,
    /// updatedAt for live records, deletedAt for tombstones.
    version_ms: i64,
    state: RemoteState<T>,
    record: RawRecord,
}

impl<T> RemoteEntry<T> {
    fn is_tombstone(&self) -> bool {
        matches!(self.state, RemoteState::Tombstone)
    }
    /// Total order for alias reduction: tombstone > live, then newer
    /// version, then larger record name (deterministic on every device).
    fn rank(&self) -> (bool, i64, &str) {
        (self.is_tombstone(), self.version_ms, self.record_name.as_str())
    }
}

pub struct SyncEngine<M: RecordMapper> {
    mapper: Arc<M>,
    backend: Arc<dyn Backend>,
    auth: Arc<dyn AuthProvider>,
    local: Arc<dyn LocalStore<M::Item>>,
    policy: Box<dyn ConflictPolicy<M::Item>>,
    clock: Arc<dyn Clock>,
    observer: Arc<dyn SyncObserver>,
    hooks: Arc<dyn PassHooks>,
    config: SyncConfig,
    store: LedgerStore,
    replay_flags: Vec<String>,
}

pub struct SyncEngineBuilder<M: RecordMapper> {
    mapper: Arc<M>,
    backend: Arc<dyn Backend>,
    local: Arc<dyn LocalStore<M::Item>>,
    storage: Box<dyn LedgerStorage>,
    auth: Arc<dyn AuthProvider>,
    policy: Box<dyn ConflictPolicy<M::Item>>,
    clock: Arc<dyn Clock>,
    observer: Arc<dyn SyncObserver>,
    hooks: Arc<dyn PassHooks>,
    config: SyncConfig,
}

impl<M: RecordMapper + 'static> SyncEngineBuilder<M> {
    pub fn new(
        mapper: Arc<M>,
        backend: Arc<dyn Backend>,
        local: Arc<dyn LocalStore<M::Item>>,
        storage: Box<dyn LedgerStorage>,
    ) -> Self
    where
        M::Item: 'static,
    {
        Self {
            mapper,
            backend,
            local,
            storage,
            auth: Arc::new(AlwaysAvailable),
            policy: Box::new(LastWriterWins),
            clock: Arc::new(SystemClock),
            observer: Arc::new(NullObserver),
            hooks: Arc::new(NoHooks),
            config: SyncConfig::default(),
        }
    }
    pub fn auth(mut self, v: Arc<dyn AuthProvider>) -> Self {
        self.auth = v;
        self
    }
    pub fn policy(mut self, v: Box<dyn ConflictPolicy<M::Item>>) -> Self {
        self.policy = v;
        self
    }
    pub fn clock(mut self, v: Arc<dyn Clock>) -> Self {
        self.clock = v;
        self
    }
    pub fn observer(mut self, v: Arc<dyn SyncObserver>) -> Self {
        self.observer = v;
        self
    }
    pub fn hooks(mut self, v: Arc<dyn PassHooks>) -> Self {
        self.hooks = v;
        self
    }
    pub fn config(mut self, v: SyncConfig) -> Self {
        self.config = v;
        self
    }
    pub fn build(self) -> SyncResult<SyncEngine<M>> {
        let store = LedgerStore::open(self.storage)?;
        if store.recovered_from_corruption {
            self.observer.event("ledger_recovered", "ledger unreadable; full replay");
        }
        Ok(SyncEngine {
            mapper: self.mapper,
            backend: self.backend,
            auth: self.auth,
            local: self.local,
            policy: self.policy,
            clock: self.clock,
            observer: self.observer,
            hooks: self.hooks,
            config: self.config,
            store,
            replay_flags: Vec::new(),
        })
    }
}

struct Fetched {
    records: Vec<RawRecord>,
    deleted: Vec<String>,
    token: Option<Vec<u8>>,
}

impl<M: RecordMapper + 'static> SyncEngine<M>
where
    M::Item: 'static,
{
    pub fn ledger(&self) -> &crate::ledger::Ledger {
        self.store.ledger()
    }

    /// Offline queue: record that `id` changed locally. Durable immediately.
    pub fn note_changed(&mut self, id: &str) -> SyncResult<()> {
        let id = id.to_string();
        self.store.mutate(|l| {
            if !l.tombstones.contains_key(&id) {
                l.pending_uploads.insert(id);
            }
        })
    }

    /// Local deletion: block the id and queue a tombstone write. Durable.
    pub fn note_deleted(&mut self, id: &str) -> SyncResult<()> {
        let now = self.clock.now_ms();
        let id = id.to_string();
        self.store.mutate(|l| {
            l.bury(&id, now);
            l.pending_tombstones.insert(id);
        })
    }

    /// Replay the whole zone once for `flag` (one-shot repair migrations).
    pub fn request_full_replay_once(&mut self, flag: &str) {
        if !self.store.ledger().flags.contains(flag) {
            self.replay_flags.push(flag.to_string());
        }
    }

    pub fn run_pass(&mut self) -> PassReport {
        let mut rep = PassReport::new();
        if !self.auth.enabled() {
            rep.outcome = PassOutcome::Disabled;
            return rep;
        }
        let now = self.clock.now_ms();
        if let Some(until) = self.store.ledger().retry_after_ms {
            if until > now {
                let secs = (until - now) as f64 / 1000.0;
                rep.outcome = PassOutcome::Deferred { secs };
                rep.retry_after_secs = Some(secs);
                return rep;
            }
        }
        let generation = self.auth.generation();
        match self.pass_inner(generation, &mut rep) {
            Ok(pending) => {
                let result = self.store.mutate(|l| {
                    if pending.is_none() {
                        l.consecutive_failures = 0;
                        l.retry_after_ms = None;
                    }
                });
                match (result, pending) {
                    (Err(e), _) => self.fail(e, &mut rep),
                    (Ok(()), None) => rep.outcome = PassOutcome::Synced,
                    (Ok(()), Some(why)) => {
                        let secs = self.config.backoff.pending_floor_secs;
                        let until = self.clock.now_ms() + (secs * 1000.0) as i64;
                        let _ = self.store.mutate(|l| l.retry_after_ms = Some(until));
                        rep.retry_after_secs = Some(secs);
                        rep.outcome = PassOutcome::Pending(why);
                    }
                }
            }
            Err(e) => self.fail(e, &mut rep),
        }
        rep
    }

    fn fail(&mut self, e: SyncError, rep: &mut PassReport) {
        if e == SyncError::Cancelled {
            rep.outcome = PassOutcome::Cancelled;
            return;
        }
        self.observer.event("pass_failed", &e.to_string());
        let failures = self.store.ledger().consecutive_failures.saturating_add(1);
        let secs = self.config.backoff.delay_secs(failures, e.retry_hint_secs());
        let until = self.clock.now_ms() + (secs * 1000.0) as i64;
        let _ = self.store.mutate(|l| {
            l.consecutive_failures = failures;
            l.retry_after_ms = Some(until);
        });
        rep.retry_after_secs = Some(secs);
        rep.outcome = match e {
            SyncError::Unavailable(d) => PassOutcome::Unavailable(d),
            other => PassOutcome::Failed(other.to_string()),
        };
    }

    fn check_current(&self, generation: u64) -> SyncResult<()> {
        if self.auth.enabled() && self.auth.generation() == generation {
            Ok(())
        } else {
            Err(SyncError::Cancelled)
        }
    }

    /// Returns `Some(reason)` when the pass finished with queued work left.
    fn pass_inner(&mut self, generation: u64, rep: &mut PassReport) -> SyncResult<Option<String>> {
        self.auth.availability()?;
        self.backend.prepare()?;
        self.check_current(generation)?;

        // Domain announce-style records go first; failure fails the pass.
        let mut extras = self.store.ledger().extras.clone();
        let outbound = self.hooks.outbound(&mut extras);
        self.store.mutate(|l| l.extras = extras)?;
        if !outbound.is_empty() {
            self.write_required(&outbound)?;
        }

        // One-shot full replays (repair migrations) reset only the cursor.
        if !self.replay_flags.is_empty() {
            self.store.mutate(|l| l.change_token = None)?;
        }

        let fetched = self.fetch_all(generation)?;
        let mapper_type = self.mapper.record_type().to_string();
        let mut entries: Vec<RemoteEntry<M::Item>> = Vec::new();
        let mut foreign: Vec<RawRecord> = Vec::new();
        for record in fetched.records {
            if record.record_type != mapper_type {
                foreign.push(record);
                continue;
            }
            if let Some((id, deleted_at)) = read_tombstone(&record) {
                entries.push(RemoteEntry {
                    id,
                    record_name: record.record_name.clone(),
                    version_ms: deleted_at,
                    state: RemoteState::Tombstone,
                    record,
                });
                continue;
            }
            match self.mapper.decode(&record) {
                Ok(item) => {
                    let h = self.mapper.header(&item);
                    entries.push(RemoteEntry {
                        id: h.id,
                        record_name: record.record_name.clone(),
                        version_ms: h.updated_at_ms,
                        state: RemoteState::Live(item),
                        record,
                    });
                }
                Err(why) => {
                    // A bad record never blocks the page (both platforms).
                    rep.quarantined += 1;
                    self.observer.event("record_quarantined", &format!("{}: {why}", record.record_name));
                }
            }
        }
        // Server-side hard deletions of records we know behave as tombstones.
        for name in &fetched.deleted {
            let known = self
                .store
                .ledger()
                .aliases
                .iter()
                .find(|(_, names)| names.contains(name))
                .map(|(id, _)| id.clone())
                .or_else(|| self.store.ledger().remote_versions.contains_key(name).then(|| name.clone()));
            if let Some(id) = known {
                if !entries.iter().any(|e| e.id == id && e.is_tombstone()) {
                    let mut record = RawRecord::new(&mapper_type, name);
                    let now = self.clock.now_ms();
                    write_tombstone(&mut record, &id, now, &[]);
                    entries.push(RemoteEntry {
                        id,
                        record_name: name.clone(),
                        version_ms: now,
                        state: RemoteState::Tombstone,
                        record,
                    });
                }
            }
        }
        if rep.quarantined > 0 {
            let n = rep.quarantined as u64;
            self.store.mutate(|l| l.quarantined_total += n)?;
        }

        // Aliases: every record name ever seen for a logical id.
        {
            let seen: Vec<(String, String)> =
                entries.iter().map(|e| (e.id.clone(), e.record_name.clone())).collect();
            self.store.mutate(|l| {
                for (id, name) in seen {
                    l.aliases.entry(id).or_default().insert(name);
                }
            })?;
        }

        // Reduce duplicate records of one logical id.
        let mut canonical: BTreeMap<String, RemoteEntry<M::Item>> = BTreeMap::new();
        for entry in entries {
            match canonical.get(&entry.id) {
                Some(cur) if cur.rank() >= entry.rank() => {}
                _ => {
                    canonical.insert(entry.id.clone(), entry);
                }
            }
        }

        // Domain-owned record types (queue requests etc.).
        if !foreign.is_empty() {
            let mut extras = self.store.ledger().extras.clone();
            let responses = self.hooks.on_foreign_records(&foreign, &mut extras);
            self.store.mutate(|l| l.extras = extras)?;
            if !responses.is_empty() {
                if let Err(e) = self.write_required(&responses) {
                    self.observer.event("foreign_write_failed", &e.to_string());
                }
            }
        }

        // Remote tombstones are authoritative: delete locally, block id.
        let tombs: Vec<(String, i64)> = canonical
            .values()
            .filter(|e| e.is_tombstone())
            .map(|e| (e.id.clone(), e.version_ms))
            .collect();
        for (id, at) in &tombs {
            self.local.remove(id)?;
            let (id, at) = (id.clone(), *at);
            self.store.mutate(|l| {
                l.bury(&id, at);
                l.pending_tombstones.remove(&id);
            })?;
        }
        rep.tombstones_applied = tombs.len();
        self.check_current(generation)?;

        let mut pending: Vec<String> = Vec::new();

        // Our own queued deletions.
        match self.flush_tombstones(&canonical, generation, rep) {
            Ok(true) => {}
            Ok(false) => pending.push("tombstones pending".into()),
            Err(e) => return Err(e),
        }

        // Inferred deletions (desktop): published item vanished locally.
        let local_items = self.local.all()?;
        let mut local_ids: HashSet<String> = HashSet::new();
        let mut locals: Vec<(ItemHeader, M::Item)> = Vec::new();
        for item in local_items {
            let h = self.mapper.header(&item);
            local_ids.insert(h.id.clone());
            locals.push((h, item));
        }
        if self.config.infer_local_deletions {
            let gone: Vec<String> = self
                .store
                .ledger()
                .remote_versions
                .keys()
                .filter(|id| !local_ids.contains(*id))
                .filter(|id| !self.store.ledger().tombstones.contains_key(*id))
                .filter(|id| !self.store.ledger().pending_apply.contains_key(*id))
                .cloned()
                .collect();
            if !gone.is_empty() {
                let now = self.clock.now_ms();
                self.store.mutate(|l| {
                    for id in gone {
                        l.bury(&id, now);
                        l.pending_tombstones.insert(id);
                    }
                })?;
                if !self.flush_tombstones(&canonical, generation, rep)? {
                    pending.push("tombstones pending".into());
                }
            }
        }

        // ---- Uploads ----
        let blocked: BTreeSet<String> = self.store.ledger().tombstones.keys().cloned().collect();
        // Drop queued ids whose item no longer exists locally.
        let stale: Vec<String> = self
            .store
            .ledger()
            .pending_uploads
            .iter()
            .filter(|id| !local_ids.contains(*id))
            .cloned()
            .collect();
        if !stale.is_empty() {
            self.store.mutate(|l| {
                for id in stale {
                    l.pending_uploads.remove(&id);
                }
            })?;
        }

        struct Upload<I> {
            header: ItemHeader,
            item: I,
            base: Option<RawRecord>,
            is_new: bool,
        }
        let mut uploads: Vec<Upload<M::Item>> = Vec::new();
        let mut failed_uploads = 0usize;
        for (header, item) in &locals {
            if blocked.contains(&header.id) {
                rep.blocked_reuploads += 1;
                continue;
            }
            let queued = self.store.ledger().pending_uploads.contains(&header.id);
            match canonical.get(&header.id) {
                Some(RemoteEntry { state: RemoteState::Live(remote_item), record, .. }) => {
                    let rh = self.mapper.header(remote_item);
                    if self.policy.upload_local_over_remote(header, item, &rh, remote_item) {
                        uploads.push(Upload {
                            header: header.clone(),
                            item: item.clone(),
                            base: Some(record.clone()),
                            is_new: false,
                        });
                    }
                }
                Some(_) => {}
                None => {
                    if let Some(known) = self.store.ledger().remote_versions.get(&header.id).cloned() {
                        // Cursor delta did not include it: unchanged remotely.
                        if known.updated_at_ms < header.updated_at_ms || queued {
                            match self.backend.fetch_record(&known.record_name) {
                                Ok(base) => uploads.push(Upload {
                                    header: header.clone(),
                                    item: item.clone(),
                                    base,
                                    is_new: false,
                                }),
                                Err(e) => {
                                    failed_uploads += 1;
                                    self.observer.event("record_fetch_failed", &e.to_string());
                                }
                            }
                        }
                    } else {
                        uploads.push(Upload {
                            header: header.clone(),
                            item: item.clone(),
                            base: None,
                            is_new: true,
                        });
                    }
                }
            }
        }
        // Oldest origin first, ties by record name: deterministic history.
        uploads.sort_by(|a, b| {
            (a.header.created_at_ms, self.mapper.record_name(&a.header.id))
                .cmp(&(b.header.created_at_ms, self.mapper.record_name(&b.header.id)))
        });
        let (new_ones, existing): (Vec<_>, Vec<_>) = uploads.into_iter().partition(|u| u.is_new);
        let size = self.config.batch_size.min(self.backend.max_batch()).max(1);
        let mut chunks: Vec<(bool, Vec<Upload<M::Item>>)> = Vec::new();
        if self.config.ordered_new_publication {
            chunks.extend(new_ones.into_iter().map(|u| (true, vec![u])));
            let mut it = existing.into_iter().peekable();
            while it.peek().is_some() {
                chunks.push((false, it.by_ref().take(size).collect()));
            }
        } else {
            let mut it = new_ones.into_iter().chain(existing).peekable();
            while it.peek().is_some() {
                chunks.push((false, it.by_ref().take(size).collect()));
            }
        }

        let mut blocked_new = false;
        for (ordered_new, chunk) in chunks {
            if ordered_new && blocked_new {
                continue;
            }
            self.check_current(generation)?;
            let mut records = Vec::with_capacity(chunk.len());
            let mut by_name: BTreeMap<String, (String, i64)> = BTreeMap::new();
            for u in &chunk {
                let name = self.mapper.record_name(&u.header.id);
                let mut rec = u
                    .base
                    .clone()
                    .unwrap_or_else(|| RawRecord::new(self.mapper.record_type(), &name));
                rec.set(field::ID, FieldValue::Text(u.header.id.clone()));
                rec.set(field::CREATED_AT, FieldValue::Date(u.header.created_at_ms));
                rec.set(field::UPDATED_AT, FieldValue::Date(u.header.updated_at_ms));
                rec.fields.remove(field::DELETED_AT);
                self.mapper.encode(&u.item, &mut rec);
                by_name.insert(rec.record_name.clone(), (u.header.id.clone(), u.header.updated_at_ms));
                records.push(rec);
            }
            let outcomes = self.backend.save_records(&records, SavePolicy::Overwrite)?;
            let mut saved: Vec<(String, String, i64)> = Vec::new();
            let mut chunk_failed = false;
            for o in outcomes {
                let Some((id, version)) = by_name.get(&o.record_name) else { continue };
                match o.result {
                    Ok(()) => saved.push((id.clone(), o.record_name.clone(), *version)),
                    Err(e) => {
                        chunk_failed = true;
                        failed_uploads += 1;
                        self.observer.event("record_save_failed", &format!("{}: {e}", o.record_name));
                    }
                }
            }
            if ordered_new && chunk_failed {
                blocked_new = true;
            }
            // Persist progress per chunk: a crash resumes from here.
            rep.uploaded += saved.len();
            self.store.mutate(|l| {
                for (id, name, version) in saved {
                    l.pending_uploads.remove(&id);
                    l.aliases.entry(id.clone()).or_default().insert(name.clone());
                    l.remote_versions
                        .insert(id, RemoteVersion { record_name: name, updated_at_ms: version });
                }
            })?;
        }
        if failed_uploads > 0 {
            pending.push(format!("{failed_uploads} upload(s) pending"));
        }

        // ---- Downloads ----
        let mut downloads: Vec<(ItemHeader, M::Item, RawRecord)> = Vec::new();
        let mut seen_ids: HashSet<String> = HashSet::new();
        for entry in canonical.values() {
            let RemoteState::Live(item) = &entry.state else { continue };
            if self.store.ledger().tombstones.contains_key(&entry.id) {
                continue;
            }
            let rh = self.mapper.header(item);
            let take = match locals.iter().find(|(h, _)| h.id == entry.id) {
                None => true,
                Some((lh, _)) => self.policy.accept_remote_over_local(lh, &rh),
            };
            if take {
                seen_ids.insert(entry.id.clone());
                downloads.push((rh, item.clone(), entry.record.clone()));
            }
        }
        // Retry records whose local apply was deferred earlier.
        let stashed: Vec<(String, RawRecord)> = self
            .store
            .ledger()
            .pending_apply
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        for (id, record) in stashed {
            if seen_ids.contains(&id) || self.store.ledger().tombstones.contains_key(&id) {
                continue;
            }
            if let Ok(item) = self.mapper.decode(&record) {
                downloads.push((self.mapper.header(&item), item, record));
            }
        }
        downloads.sort_by(|a, b| {
            (a.0.created_at_ms, a.0.id.as_str()).cmp(&(b.0.created_at_ms, b.0.id.as_str()))
        });
        if !downloads.is_empty() {
            let items: Vec<M::Item> = downloads.iter().map(|d| d.1.clone()).collect();
            let outcomes = self.local.apply_remote(&items)?;
            if outcomes.len() != downloads.len() {
                return Err(SyncError::Persistence("apply_remote returned wrong outcome count".into()));
            }
            let mut deferred = 0usize;
            let mut applied_ids: Vec<String> = Vec::new();
            let mut stash: Vec<(String, RawRecord)> = Vec::new();
            for ((h, _, record), out) in downloads.into_iter().zip(outcomes) {
                match out {
                    ApplyOutcome::Applied => {
                        rep.downloaded += 1;
                        applied_ids.push(h.id);
                    }
                    ApplyOutcome::Deferred(why) => {
                        deferred += 1;
                        self.observer.event("apply_deferred", &format!("{}: {why}", h.id));
                        stash.push((h.id, record));
                    }
                }
            }
            self.store.mutate(|l| {
                for id in applied_ids {
                    l.pending_apply.remove(&id);
                }
                for (id, rec) in stash {
                    l.pending_apply.insert(id, rec);
                }
            })?;
            if deferred > 0 {
                pending.push(format!("{deferred} download(s) deferred"));
            }
        }

        // Remote versions + advance the token: only now, after everything
        // above applied (or was durably queued).
        let versions: Vec<(String, RemoteVersion)> = canonical
            .values()
            .map(|e| {
                (
                    e.id.clone(),
                    RemoteVersion { record_name: e.record_name.clone(), updated_at_ms: e.version_ms },
                )
            })
            .collect();
        let flags = std::mem::take(&mut self.replay_flags);
        let token = fetched.token;
        self.store.mutate(|l| {
            for (id, v) in versions {
                // A local upload this pass may already hold a newer version.
                let keep = l.remote_versions.get(&id).is_some_and(|k| k.updated_at_ms > v.updated_at_ms);
                if !keep {
                    l.remote_versions.insert(id, v);
                }
            }
            if token.is_some() {
                l.change_token = token;
            }
            l.has_synced = true;
            for f in flags {
                l.flags.insert(f);
            }
        })?;
        if rep.blocked_reuploads > 0 {
            self.observer.event("blocked_reupload", &rep.blocked_reuploads.to_string());
        }
        Ok(if pending.is_empty() { None } else { Some(pending.join("; ")) })
    }

    /// Fetch every page; reset the token once on expiry and re-prepare the
    /// zone once if it vanished.
    fn fetch_all(&mut self, generation: u64) -> SyncResult<Fetched> {
        let mut reset_token = false;
        let mut recreated = false;
        'restart: loop {
            let mut token = self.store.ledger().change_token.clone();
            let mut out = Fetched { records: Vec::new(), deleted: Vec::new(), token: None };
            loop {
                self.check_current(generation)?;
                match self.backend.fetch_changes(token.as_deref()) {
                    Ok(page) => {
                        out.records.extend(page.records);
                        out.deleted.extend(page.deleted_record_names);
                        if page.new_token.is_some() {
                            token = page.new_token.clone();
                            out.token = page.new_token;
                        }
                        if !page.more_coming {
                            return Ok(out);
                        }
                    }
                    Err(SyncError::TokenExpired) if !reset_token && token.is_some() => {
                        reset_token = true;
                        self.observer.event("token_reset", "change token expired; full replay");
                        self.store.mutate(|l| l.change_token = None)?;
                        continue 'restart;
                    }
                    Err(SyncError::ZoneMissing) if !recreated => {
                        recreated = true;
                        self.observer.event("zone_recreated", "zone missing; re-preparing");
                        self.store.mutate(|l| l.change_token = None)?;
                        self.backend.prepare()?;
                        continue 'restart;
                    }
                    Err(e) => return Err(e),
                }
            }
        }
    }

    /// Write queued tombstones to every alias record. Returns true when the
    /// queue drained.
    fn flush_tombstones(
        &mut self,
        canonical: &BTreeMap<String, RemoteEntry<M::Item>>,
        generation: u64,
        rep: &mut PassReport,
    ) -> SyncResult<bool> {
        let ids: Vec<String> = self.store.ledger().pending_tombstones.iter().cloned().collect();
        if ids.is_empty() {
            return Ok(true);
        }
        struct Write {
            id: String,
            record: RawRecord,
        }
        let mut writes: Vec<Write> = Vec::new();
        let mut never_remote: Vec<String> = Vec::new();
        for id in &ids {
            let mut names: BTreeSet<String> =
                self.store.ledger().aliases.get(id).cloned().unwrap_or_default();
            let known = self.store.ledger().remote_versions.contains_key(id) || canonical.contains_key(id);
            if names.is_empty() && !known {
                never_remote.push(id.clone());
                continue;
            }
            names.insert(self.mapper.record_name(id));
            let at = self.store.ledger().tombstones.get(id).copied().unwrap_or_else(|| self.clock.now_ms());
            for name in names {
                let mut record = match canonical.get(id).filter(|e| e.record_name == name) {
                    Some(e) => e.record.clone(),
                    None => self
                        .backend
                        .fetch_record(&name)
                        .ok()
                        .flatten()
                        .unwrap_or_else(|| RawRecord::new(self.mapper.record_type(), &name)),
                };
                write_tombstone(&mut record, id, at, self.mapper.tombstone_keep_fields());
                writes.push(Write { id: id.clone(), record });
            }
        }
        if !never_remote.is_empty() {
            // Created and deleted before ever reaching the zone: nothing to write.
            self.store.mutate(|l| {
                for id in never_remote {
                    l.pending_tombstones.remove(&id);
                }
            })?;
        }
        let mut failed: BTreeSet<String> = BTreeSet::new();
        let size = self.config.tombstone_batch.min(self.backend.max_batch()).max(1);
        for chunk in writes.chunks(size) {
            self.check_current(generation)?;
            let records: Vec<RawRecord> = chunk.iter().map(|w| w.record.clone()).collect();
            let outcomes = self.backend.save_records(&records, SavePolicy::Overwrite)?;
            for w in chunk {
                let ok = outcomes
                    .iter()
                    .find(|o| o.record_name == w.record.record_name)
                    .is_some_and(|o| o.result.is_ok());
                if !ok {
                    failed.insert(w.id.clone());
                    self.observer.event("tombstone_save_failed", &w.record.record_name);
                }
            }
        }
        let acked: Vec<String> = writes
            .iter()
            .map(|w| w.id.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter(|id| !failed.contains(id))
            .collect();
        rep.tombstones_written += acked.len();
        self.store.mutate(|l| {
            for id in &acked {
                l.pending_tombstones.remove(id);
            }
        })?;
        Ok(failed.is_empty())
    }

    /// Save records whose failure must fail the pass.
    fn write_required(&self, records: &[RawRecord]) -> SyncResult<()> {
        for chunk in records.chunks(self.backend.max_batch().max(1)) {
            for o in self.backend.save_records(chunk, SavePolicy::Overwrite)? {
                o.result?;
            }
        }
        Ok(())
    }
}
