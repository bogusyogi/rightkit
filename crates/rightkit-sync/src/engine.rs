//! The sync pass. One `run_pass` mirrors the iOS `sync()` actor method and the
//! desktop `Coordinator::sync_pass`; see API.md for the shared contract.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::backoff::BackoffPolicy;
use crate::error::{SyncError, SyncResult};
use crate::ledger::{ConsentRequest, LedgerStore, RemoteVersion};
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
    /// When the signed-in account or scope differs from the one the ledger is bound to, pause
    /// instead of resetting the ledger and publishing local history into the new account. The
    /// pass reports [`PassOutcome::ConsentRequired`] and uploads, downloads and writes nothing
    /// until the app calls [`SyncEngine::grant_consent`]. Default `false` (the earlier silent
    /// reset), so existing apps keep their behaviour.
    pub require_consent_on_scope_change: bool,
    pub backoff: BackoffPolicy,
}

/// The app's answer to a [`PassOutcome::ConsentRequired`] pause.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsentChoice {
    /// Publish this device's local items into the new account (and download its content).
    Merge,
    /// Keep this device's existing local items local: they are never published to the new
    /// account. The new account's content is still downloaded; items created or received after
    /// consent sync normally. Nothing local is deleted.
    StartFresh,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            batch_size: 200,
            tombstone_batch: 50,
            ordered_new_publication: true,
            infer_local_deletions: false,
            require_consent_on_scope_change: false,
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
    Deferred {
        secs: f64,
    },
    Unavailable(String),
    Disabled,
    /// The signed-in account or scope changed and the app has not consented. Nothing was
    /// uploaded, downloaded or written. Call [`SyncEngine::grant_consent`] with `to`.
    ConsentRequired(ConsentRequest),
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

/// Cumulative engine counters and current ledger queue sizes, from [`SyncEngine::metrics`].
/// Queue sizes are the figures that grow resident memory: the ledger keeps one remote
/// version per record and is serialized in full on every durable write.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SyncMetrics {
    /// Passes run since the engine was built (including deferred ones).
    pub passes: u64,
    /// Passes that ended `Failed` or `Unavailable`.
    pub failed_passes: u64,
    /// Wall-clock duration of the most recent pass, in milliseconds.
    pub last_pass_ms: u64,
    pub downloaded_total: u64,
    pub uploaded_total: u64,
    pub tombstones_applied_total: u64,
    pub tombstones_written_total: u64,
    /// Remote versions cached in the ledger (one per known record).
    pub remote_versions: u64,
    /// Tombstones the ledger blocks re-import of, forever.
    pub tombstones: u64,
    pub pending_uploads: u64,
    pub pending_tombstones: u64,
    /// Downloaded records whose local apply was deferred.
    pub pending_apply: u64,
}

/// Runtime figures of the engine, computed on call by [`SyncEngine::status`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SyncStatus {
    /// Pass counters and the ledger queue sizes (`pending_*`, `remote_versions`).
    pub metrics: SyncMetrics,
    /// Resident memory, CPU time and I/O counters of the whole process (the engine runs in it and
    /// has no thread of its own), or `None` when the host cannot report them. See
    /// `rightkit_child_process::ProcessUsage`.
    #[cfg(feature = "process-usage")]
    pub process: Option<rightkit_child_process::ProcessUsage>,
}

#[derive(Debug, Default)]
struct Counters {
    passes: u64,
    failed_passes: u64,
    last_pass_ms: u64,
    downloaded: u64,
    uploaded: u64,
    tombstones_applied: u64,
    tombstones_written: u64,
}

const MAX_CONFLICT_ROUNDS: usize = 4;
/// Local items fetched per `LocalStore::list_page` call during a pass.
const LOCAL_PAGE_SIZE: usize = 1000;
/// Downloaded items decoded and handed to `LocalStore::apply_remote` per call.
const APPLY_CHUNK: usize = 1000;

enum Resolved {
    /// Re-saved on top of the server's version.
    Saved,
    /// The server version stands (applied locally, tombstoned, or equal).
    RemoteWon,
    Failed(String),
}

enum RemoteState {
    /// Header of the decoded record; the body is re-decoded from `record` when needed, so
    /// decoded items are never held for the whole delta.
    Live(ItemHeader),
    Tombstone,
}

struct RemoteEntry {
    id: String,
    record_name: String,
    /// updatedAt for live records, deletedAt for tombstones.
    version_ms: i64,
    state: RemoteState,
    record: RawRecord,
}

impl RemoteEntry {
    fn is_tombstone(&self) -> bool {
        matches!(self.state, RemoteState::Tombstone)
    }
    /// Total order for alias reduction: tombstone > live, then newer
    /// version, then larger record name (deterministic on every device).
    fn rank(&self) -> (bool, i64, &str) {
        (
            self.is_tombstone(),
            self.version_ms,
            self.record_name.as_str(),
        )
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
    counters: Counters,
    /// Set by `bind_account` for the pass in flight when it stopped on a consent gate.
    gate: Option<ConsentRequest>,
    local_page_size: usize,
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
    local_page_size: usize,
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
            local_page_size: LOCAL_PAGE_SIZE,
        }
    }
    /// Items requested per `LocalStore::list_page` call during a pass (default 1000, minimum 1).
    pub fn local_page_size(mut self, n: usize) -> Self {
        self.local_page_size = n.max(1);
        self
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
            self.observer
                .event("ledger_recovered", "ledger unreadable; full replay");
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
            counters: Counters::default(),
            gate: None,
            local_page_size: self.local_page_size,
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

    /// The scope change currently awaiting consent, if any. Durable: it survives a restart and
    /// is also reported by [`PassOutcome::ConsentRequired`].
    pub fn consent_request(&self) -> Option<ConsentRequest> {
        self.store.ledger().pending_consent.clone()
    }

    /// Record explicit consent to sync into the new account `to` (the `to` of the pending
    /// [`ConsentRequest`]). Resets the ledger for the new scope (the old account's cursor,
    /// versions and queued deletions never reach it) and lets the next pass run. Refused when
    /// nothing is pending, when `to` is not the pending target, or when the signed-in scope has
    /// since moved on (the next pass then raises a fresh request).
    pub fn grant_consent(&mut self, to: &str, choice: ConsentChoice) -> SyncResult<()> {
        let Some(req) = self.store.ledger().pending_consent.clone() else {
            return Err(SyncError::Transient(
                "no scope change is awaiting consent".into(),
            ));
        };
        if req.to != to {
            return Err(SyncError::Transient(
                "consent names a different account than the one pending".into(),
            ));
        }
        if let Some(current) = self.auth.account_scope() {
            if current != req.to {
                return Err(SyncError::Transient(
                    "the signed-in account changed again; consent is stale".into(),
                ));
            }
        }
        let withheld: BTreeSet<String> = match choice {
            ConsentChoice::Merge => BTreeSet::new(),
            ConsentChoice::StartFresh => {
                let mut ids = BTreeSet::new();
                self.scan_local(|i| {
                    ids.insert(self.mapper.header(&i).id);
                    Ok(())
                })?;
                ids
            }
        };
        self.hooks.on_account_switch(&req.from, &req.to);
        let scope = req.to.clone();
        self.store.mutate(|l| {
            *l = crate::ledger::Ledger::default();
            l.scope = Some(scope);
            l.withheld = withheld;
        })?;
        self.gate = None;
        self.observer.event(
            "consent_granted",
            match choice {
                ConsentChoice::Merge => "merge",
                ConsentChoice::StartFresh => "start_fresh",
            },
        );
        Ok(())
    }

    /// Visit every local item once, a page at a time through `LocalStore::list_page` (falling
    /// back to one `all()` call for stores that do not page), so only one page is resident.
    /// The visitor must not call back into the local store.
    fn scan_local(&self, mut each: impl FnMut(M::Item) -> SyncResult<()>) -> SyncResult<()> {
        let size = self.local_page_size.max(1);
        let mut cursor: Option<String> = None;
        loop {
            let (page, last) = match self.local.list_page(cursor.as_deref(), size)? {
                Some(p) => {
                    let last = p.len() < size;
                    (p, last)
                }
                None => (self.local.all()?, true),
            };
            let next = if last {
                None
            } else {
                page.last().map(|i| self.mapper.header(i).id)
            };
            if let (Some(n), Some(c)) = (next.as_ref(), cursor.as_ref()) {
                if n <= c {
                    return Err(SyncError::Persistence(
                        "LocalStore::list_page did not advance past its cursor".into(),
                    ));
                }
            }
            for item in page {
                each(item)?;
            }
            match next {
                Some(n) => cursor = Some(n),
                None => return Ok(()),
            }
        }
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

    /// Runs one sync pass and records it in [`SyncEngine::metrics`].
    pub fn run_pass(&mut self) -> PassReport {
        let started = Instant::now();
        let rep = self.run_pass_once();
        self.record_pass(&rep, started.elapsed());
        rep
    }

    fn record_pass(&mut self, rep: &PassReport, elapsed: Duration) {
        let c = &mut self.counters;
        c.passes += 1;
        if matches!(
            rep.outcome,
            PassOutcome::Failed(_) | PassOutcome::Unavailable(_)
        ) {
            c.failed_passes += 1;
        }
        c.last_pass_ms = elapsed.as_millis() as u64;
        c.downloaded += rep.downloaded as u64;
        c.uploaded += rep.uploaded as u64;
        c.tombstones_applied += rep.tombstones_applied as u64;
        c.tombstones_written += rep.tombstones_written as u64;
    }

    /// Pass counters, ledger queue sizes and process resource usage, read now. No polling thread
    /// runs for this.
    pub fn status(&self) -> SyncStatus {
        SyncStatus {
            metrics: self.metrics(),
            #[cfg(feature = "process-usage")]
            process: rightkit_child_process::process_usage().ok(),
        }
    }

    /// Cumulative pass counters and current ledger queue sizes.
    pub fn metrics(&self) -> SyncMetrics {
        let l = self.store.ledger();
        let c = &self.counters;
        SyncMetrics {
            passes: c.passes,
            failed_passes: c.failed_passes,
            last_pass_ms: c.last_pass_ms,
            downloaded_total: c.downloaded,
            uploaded_total: c.uploaded,
            tombstones_applied_total: c.tombstones_applied,
            tombstones_written_total: c.tombstones_written,
            remote_versions: l.remote_versions.len() as u64,
            tombstones: l.tombstones.len() as u64,
            pending_uploads: l.pending_uploads.len() as u64,
            pending_tombstones: l.pending_tombstones.len() as u64,
            pending_apply: l.pending_apply.len() as u64,
        }
    }

    fn run_pass_once(&mut self) -> PassReport {
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
        self.gate = None;
        let result = self.pass_inner(generation, &mut rep);
        if let Some(req) = self.gate.take() {
            if result.is_ok() {
                rep.outcome = PassOutcome::ConsentRequired(req);
                return rep;
            }
        }
        match result {
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
        let secs = self
            .config
            .backoff
            .delay_secs(failures, e.retry_hint_secs());
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

    /// A conditional save lost a race. Read the server record, decide with
    /// the conflict policy (tombstone always wins) and either re-save on top
    /// of the server's change tag, take the remote, or keep both unchanged.
    /// Bounded: a record that keeps changing under us stays pending.
    fn resolve_conflict(
        &mut self,
        header: &ItemHeader,
        item: &M::Item,
        name: &str,
        mut server: Option<RawRecord>,
        generation: u64,
        rep: &mut PassReport,
    ) -> SyncResult<Resolved> {
        for _ in 0..MAX_CONFLICT_ROUNDS {
            self.check_current(generation)?;
            let server_rec = match server.take() {
                Some(r) => r,
                None => match self.backend.fetch_record(name)? {
                    Some(r) => r,
                    None => return Ok(Resolved::Failed("conflicting record vanished".into())),
                },
            };
            self.observer.event("save_conflict", name);
            if let Some((id, deleted_at)) = read_tombstone(&server_rec) {
                self.local.remove(&id)?;
                self.store.mutate(|l| l.bury(&id, deleted_at))?;
                rep.tombstones_applied += 1;
                return Ok(Resolved::RemoteWon);
            }
            let remote_item = match self.mapper.decode(&server_rec) {
                Ok(i) => i,
                Err(e) => {
                    rep.quarantined += 1;
                    return Ok(Resolved::Failed(format!("server record undecodable: {e}")));
                }
            };
            let rh = self.mapper.header(&remote_item);
            if self
                .policy
                .upload_local_over_remote(header, item, &rh, &remote_item)
            {
                let mut rec = server_rec;
                rec.set(field::ID, FieldValue::Text(header.id.clone()));
                rec.set(field::CREATED_AT, FieldValue::Date(header.created_at_ms));
                rec.set(field::UPDATED_AT, FieldValue::Date(header.updated_at_ms));
                rec.fields.remove(field::DELETED_AT);
                self.mapper.encode(item, &mut rec);
                let outcomes = self
                    .backend
                    .save_records(&[rec], SavePolicy::IfServerUnchanged)?;
                let Some(o) = outcomes.into_iter().next() else {
                    return Ok(Resolved::Failed("no save outcome".into()));
                };
                match o.result {
                    Ok(()) => return Ok(Resolved::Saved),
                    Err(SyncError::Conflict) => server = o.server_record,
                    Err(e) => return Ok(Resolved::Failed(e.to_string())),
                }
                continue;
            }
            let version = RemoteVersion {
                record_name: name.to_string(),
                updated_at_ms: rh.updated_at_ms,
            };
            if self.policy.accept_remote_over_local(header, &rh) {
                self.check_current(generation)?;
                match self.local.apply_remote(&[remote_item])?.into_iter().next() {
                    Some(ApplyOutcome::Deferred(_)) => {
                        let id = header.id.clone();
                        self.store.mutate(|l| {
                            l.pending_apply.insert(id, server_rec);
                        })?;
                    }
                    _ => rep.downloaded += 1,
                }
            }
            let id = header.id.clone();
            self.store.mutate(|l| {
                l.pending_uploads.remove(&id);
                l.remote_versions.insert(id, version);
            })?;
            return Ok(Resolved::RemoteWon);
        }
        Ok(Resolved::Failed("conflict retries exhausted".into()))
    }

    /// Bind the ledger to the signed-in account scope; reset it when the
    /// scope changed so one account's cursor, versions and queued deletions
    /// are never applied to another.
    ///
    /// With `require_consent_on_scope_change` a switch from a bound scope is not applied: the
    /// ledger is left untouched, the request is persisted and `self.gate` is set so the pass
    /// stops before any I/O.
    fn bind_account(&mut self, generation: u64) -> SyncResult<()> {
        self.check_current(generation)?;
        let Some(scope) = self.auth.account_scope() else {
            return Ok(());
        };
        match self.store.ledger().scope.clone() {
            Some(bound) if bound == scope => {
                if self.store.ledger().pending_consent.is_some() {
                    // The account went back to the bound one: the request is moot.
                    self.store.mutate(|l| l.pending_consent = None)?;
                }
                Ok(())
            }
            Some(bound) if self.config.require_consent_on_scope_change => {
                let req = ConsentRequest {
                    from: bound,
                    to: scope,
                };
                if self.store.ledger().pending_consent.as_ref() != Some(&req) {
                    let stored = req.clone();
                    self.store.mutate(|l| l.pending_consent = Some(stored))?;
                    self.observer
                        .event("consent_required", &format!("{} -> {}", req.from, req.to));
                }
                self.gate = Some(req);
                Ok(())
            }
            None => {
                // Ledger from before scoping: keep tombstones (they only
                // suppress local copies); never import its cursor, versions,
                // aliases or queued deletions into this account.
                let dropped = {
                    let l = self.store.ledger();
                    l.change_token.is_some()
                        || !l.remote_versions.is_empty()
                        || !l.aliases.is_empty()
                        || !l.pending_tombstones.is_empty()
                };
                self.store.mutate(|l| {
                    l.scope = Some(scope);
                    l.change_token = None;
                    l.remote_versions.clear();
                    l.aliases.clear();
                    l.pending_tombstones.clear();
                    l.has_synced = false;
                })?;
                if dropped {
                    self.observer
                        .event("legacy_ledger_unscoped", "pre-scope ledger state dropped");
                }
                Ok(())
            }
            Some(bound) => {
                self.hooks.on_account_switch(&bound, &scope);
                self.store.mutate(|l| {
                    *l = crate::ledger::Ledger::default();
                    l.scope = Some(scope.clone());
                })?;
                self.observer.event("account_switched", "ledger reset");
                Ok(())
            }
        }
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
        // `availability` refreshes the account identity, which may bump the
        // generation; the pass belongs to the generation observed after it.
        let generation = self.auth.generation().max(generation);
        self.bind_account(generation)?;
        if self.gate.is_some() {
            return Ok(None);
        }
        self.backend.prepare()?;
        self.check_current(generation)?;

        // Domain announce-style records go first; failure fails the pass.
        let mut extras = self.store.ledger().extras.clone();
        let outbound = self.hooks.outbound(&mut extras);
        if !outbound.is_empty() {
            self.check_current(generation)?;
            self.write_required(&outbound)?;
        }
        self.check_current(generation)?;
        self.store.mutate(|l| l.extras = extras)?;

        // One-shot full replays (repair migrations) reset only the cursor.
        if !self.replay_flags.is_empty() {
            self.store.mutate(|l| l.change_token = None)?;
        }

        let fetched = self.fetch_all(generation)?;
        let mapper_type = self.mapper.record_type().to_string();
        let mut entries: Vec<RemoteEntry> = Vec::new();
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
                        id: h.id.clone(),
                        record_name: record.record_name.clone(),
                        version_ms: h.updated_at_ms,
                        state: RemoteState::Live(h),
                        record,
                    });
                }
                Err(why) => {
                    // A bad record never blocks the page (both platforms).
                    rep.quarantined += 1;
                    self.observer.event(
                        "record_quarantined",
                        &format!("{}: {why}", record.record_name),
                    );
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
                .or_else(|| {
                    self.store
                        .ledger()
                        .remote_versions
                        .contains_key(name)
                        .then(|| name.clone())
                });
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
        self.store.mutate(|l| {
            for e in &entries {
                l.aliases
                    .entry(e.id.clone())
                    .or_default()
                    .insert(e.record_name.clone());
            }
        })?;

        // Reduce duplicate records of one logical id.
        let mut canonical: BTreeMap<String, RemoteEntry> = BTreeMap::new();
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
            // Responses are durable on the zone BEFORE the extras that record
            // them are persisted, and a failed write aborts the pass before
            // the change token can pass the request: the delta replays and
            // the idempotent hook answers again. Never acknowledge a request
            // whose response never landed.
            if !responses.is_empty() {
                self.check_current(generation)?;
                if let Err(e) = self.write_required(&responses) {
                    self.observer.event("foreign_write_failed", &e.to_string());
                    return Err(e);
                }
            }
            self.check_current(generation)?;
            self.store.mutate(|l| l.extras = extras)?;
        }

        // Remote tombstones are authoritative: delete locally, block id.
        let tombs: Vec<(String, i64)> = canonical
            .values()
            .filter(|e| e.is_tombstone())
            .map(|e| (e.id.clone(), e.version_ms))
            .collect();
        for (id, at) in &tombs {
            self.check_current(generation)?;
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

        // ---- Local scan ----
        // One paged pass over the local store feeds the inferred deletions, the stale-queue
        // check, the upload plan and the download comparison, so at most one page of local
        // items is resident; only items that must upload are kept, and only the headers of
        // items that also have a live remote entry.
        struct Upload<I> {
            header: ItemHeader,
            item: I,
            base: Option<RawRecord>,
            is_new: bool,
        }
        let infer = self.config.infer_local_deletions;
        let mut gone_candidates: HashSet<String> = if infer {
            let l = self.store.ledger();
            l.remote_versions
                .keys()
                .filter(|id| !l.tombstones.contains_key(*id))
                .filter(|id| !l.pending_apply.contains_key(*id))
                .cloned()
                .collect()
        } else {
            HashSet::new()
        };
        let mut queued_unseen: HashSet<String> = self
            .store
            .ledger()
            .pending_uploads
            .iter()
            .cloned()
            .collect();
        let mut matched: HashMap<String, ItemHeader> = HashMap::new();
        let mut uploads: Vec<Upload<M::Item>> = Vec::new();
        let mut failed_uploads = 0usize;
        self.scan_local(|item| {
            let header = self.mapper.header(&item);
            if infer {
                gone_candidates.remove(&header.id);
            }
            queued_unseen.remove(&header.id);
            let canon = canonical.get(&header.id);
            if let Some(RemoteEntry {
                state: RemoteState::Live(_),
                ..
            }) = canon
            {
                matched
                    .entry(header.id.clone())
                    .or_insert_with(|| header.clone());
            }
            if self.store.ledger().tombstones.contains_key(&header.id) {
                rep.blocked_reuploads += 1;
                return Ok(());
            }
            if self.store.ledger().withheld.contains(&header.id) {
                return Ok(());
            }
            let queued = self.store.ledger().pending_uploads.contains(&header.id);
            match canon {
                Some(RemoteEntry {
                    state: RemoteState::Live(rh),
                    record,
                    ..
                }) => {
                    if let Ok(remote_item) = self.mapper.decode(record) {
                        if self
                            .policy
                            .upload_local_over_remote(&header, &item, rh, &remote_item)
                        {
                            uploads.push(Upload {
                                header,
                                item,
                                base: Some(record.clone()),
                                is_new: false,
                            });
                        }
                    }
                }
                Some(_) => {}
                None => {
                    if let Some(known) =
                        self.store.ledger().remote_versions.get(&header.id).cloned()
                    {
                        // Cursor delta did not include it: unchanged remotely.
                        if known.updated_at_ms < header.updated_at_ms || queued {
                            match self.backend.fetch_record(&known.record_name) {
                                Ok(base) => uploads.push(Upload {
                                    header,
                                    item,
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
                            header,
                            item,
                            base: None,
                            is_new: true,
                        });
                    }
                }
            }
            Ok(())
        })?;

        // Inferred deletions (desktop): published item vanished locally.
        if infer {
            let gone: Vec<String> = gone_candidates.into_iter().collect();
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
        // Drop queued ids whose item no longer exists locally.
        let stale: Vec<String> = queued_unseen
            .into_iter()
            .filter(|id| self.store.ledger().pending_uploads.contains(id))
            .collect();
        if !stale.is_empty() {
            self.store.mutate(|l| {
                for id in stale {
                    l.pending_uploads.remove(&id);
                }
            })?;
        }
        // Oldest origin first, ties by record name: deterministic history.
        uploads.sort_by(|a, b| {
            (
                a.header.created_at_ms,
                self.mapper.record_name(&a.header.id),
            )
                .cmp(&(
                    b.header.created_at_ms,
                    self.mapper.record_name(&b.header.id),
                ))
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
                by_name.insert(
                    rec.record_name.clone(),
                    (u.header.id.clone(), u.header.updated_at_ms),
                );
                records.push(rec);
            }
            // Conditional: an edit based on a stale change tag (or an insert of
            // a name that now exists) is refused, never overwritten.
            let outcomes = self
                .backend
                .save_records(&records, SavePolicy::IfServerUnchanged)?;
            let mut saved: Vec<(String, String, i64)> = Vec::new();
            let mut chunk_failed = false;
            for o in outcomes {
                let Some((id, version)) = by_name.get(&o.record_name) else {
                    continue;
                };
                match o.result {
                    Ok(()) => saved.push((id.clone(), o.record_name.clone(), *version)),
                    Err(SyncError::Conflict) => {
                        let upload = chunk
                            .iter()
                            .find(|u| self.mapper.record_name(&u.header.id) == o.record_name)
                            .expect("outcome names an upload of this chunk");
                        let (header, item) = (upload.header.clone(), upload.item.clone());
                        match self.resolve_conflict(
                            &header,
                            &item,
                            &o.record_name,
                            o.server_record,
                            generation,
                            rep,
                        )? {
                            Resolved::Saved => {
                                saved.push((id.clone(), o.record_name.clone(), *version))
                            }
                            Resolved::RemoteWon => {}
                            Resolved::Failed(why) => {
                                chunk_failed = true;
                                failed_uploads += 1;
                                self.observer.event(
                                    "record_save_failed",
                                    &format!("{}: {why}", o.record_name),
                                );
                            }
                        }
                    }
                    Err(e) => {
                        chunk_failed = true;
                        failed_uploads += 1;
                        self.observer
                            .event("record_save_failed", &format!("{}: {e}", o.record_name));
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
                    l.aliases
                        .entry(id.clone())
                        .or_default()
                        .insert(name.clone());
                    l.remote_versions.insert(
                        id,
                        RemoteVersion {
                            record_name: name,
                            updated_at_ms: version,
                        },
                    );
                }
            })?;
        }
        if failed_uploads > 0 {
            pending.push(format!("{failed_uploads} upload(s) pending"));
        }

        // ---- Downloads ----
        // Plan first with ids and sort keys only (the global (created, id) order is kept), then
        // decode and apply a bounded chunk at a time so item bodies are never all resident.
        enum Source {
            Remote,
            Stash(RawRecord),
        }
        struct Planned {
            created: i64,
            id: String,
            source: Source,
        }
        let stash_ids: HashSet<String> =
            self.store.ledger().pending_apply.keys().cloned().collect();
        let mut taken_stash_ids: HashSet<String> = HashSet::new();
        let mut plan: Vec<Planned> = Vec::new();
        for entry in canonical.values() {
            let RemoteState::Live(rh) = &entry.state else {
                continue;
            };
            if self.store.ledger().tombstones.contains_key(&entry.id) {
                continue;
            }
            let take = match matched.get(&entry.id) {
                None => true,
                Some(lh) => self.policy.accept_remote_over_local(lh, rh),
            };
            if take {
                if stash_ids.contains(&entry.id) {
                    taken_stash_ids.insert(entry.id.clone());
                }
                plan.push(Planned {
                    created: rh.created_at_ms,
                    id: entry.id.clone(),
                    source: Source::Remote,
                });
            }
        }
        drop(matched);
        // Retry records whose local apply was deferred earlier.
        let stashed: Vec<(String, RawRecord)> = self
            .store
            .ledger()
            .pending_apply
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        for (id, record) in stashed {
            if taken_stash_ids.contains(&id) || self.store.ledger().tombstones.contains_key(&id) {
                continue;
            }
            if let Ok(item) = self.mapper.decode(&record) {
                let h = self.mapper.header(&item);
                plan.push(Planned {
                    created: h.created_at_ms,
                    id: h.id,
                    source: Source::Stash(record),
                });
            }
        }
        plan.sort_by(|a, b| (a.created, a.id.as_str()).cmp(&(b.created, b.id.as_str())));
        let mut deferred = 0usize;
        for chunk in plan.chunks(APPLY_CHUNK) {
            self.check_current(generation)?;
            let mut items: Vec<M::Item> = Vec::with_capacity(chunk.len());
            let mut decoded_at: Vec<usize> = Vec::with_capacity(chunk.len());
            for (i, p) in chunk.iter().enumerate() {
                let decoded = match &p.source {
                    Source::Remote => canonical.get(&p.id).map(|e| self.mapper.decode(&e.record)),
                    Source::Stash(rec) => Some(self.mapper.decode(rec)),
                };
                if let Some(Ok(item)) = decoded {
                    items.push(item);
                    decoded_at.push(i);
                }
            }
            if items.is_empty() {
                continue;
            }
            let outcomes = self.local.apply_remote(&items)?;
            if outcomes.len() != items.len() {
                return Err(SyncError::Persistence(
                    "apply_remote returned wrong outcome count".into(),
                ));
            }
            drop(items);
            let mut applied_ids: Vec<String> = Vec::new();
            let mut stash: Vec<(String, RawRecord)> = Vec::new();
            for (i, out) in decoded_at.into_iter().zip(outcomes) {
                let p = &chunk[i];
                match out {
                    ApplyOutcome::Applied => {
                        rep.downloaded += 1;
                        applied_ids.push(p.id.clone());
                    }
                    ApplyOutcome::Deferred(why) => {
                        deferred += 1;
                        self.observer
                            .event("apply_deferred", &format!("{}: {why}", p.id));
                        let record = match &p.source {
                            Source::Remote => canonical.get(&p.id).map(|e| e.record.clone()),
                            Source::Stash(rec) => Some(rec.clone()),
                        };
                        if let Some(record) = record {
                            stash.push((p.id.clone(), record));
                        }
                    }
                }
            }
            let touches_ledger = !stash.is_empty()
                || applied_ids
                    .iter()
                    .any(|id| self.store.ledger().pending_apply.contains_key(id));
            if touches_ledger {
                self.store.mutate(|l| {
                    for id in applied_ids {
                        l.pending_apply.remove(&id);
                    }
                    for (id, rec) in stash {
                        l.pending_apply.insert(id, rec);
                    }
                })?;
            }
        }
        drop(plan);
        if deferred > 0 {
            pending.push(format!("{deferred} download(s) deferred"));
        }

        // Remote versions + advance the token: only now, after everything
        // above applied (or was durably queued).
        // Final guard: a pass whose account/enable generation changed during
        // I/O must not advance the cursor or mark the ledger synced.
        self.check_current(generation)?;
        let flags = std::mem::take(&mut self.replay_flags);
        let token = fetched.token;
        self.store.mutate(|l| {
            for e in canonical.values() {
                // A local upload this pass may already hold a newer version.
                let keep = l
                    .remote_versions
                    .get(&e.id)
                    .is_some_and(|k| k.updated_at_ms > e.version_ms);
                if !keep {
                    l.remote_versions.insert(
                        e.id.clone(),
                        RemoteVersion {
                            record_name: e.record_name.clone(),
                            updated_at_ms: e.version_ms,
                        },
                    );
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
            self.observer
                .event("blocked_reupload", &rep.blocked_reuploads.to_string());
        }
        Ok(if pending.is_empty() {
            None
        } else {
            Some(pending.join("; "))
        })
    }

    /// Fetch every page; reset the token once on expiry and re-prepare the
    /// zone once if it vanished.
    fn fetch_all(&mut self, generation: u64) -> SyncResult<Fetched> {
        let mut reset_token = false;
        let mut recreated = false;
        'restart: loop {
            let mut token = self.store.ledger().change_token.clone();
            let mut out = Fetched {
                records: Vec::new(),
                deleted: Vec::new(),
                token: None,
            };
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
                        self.observer
                            .event("token_reset", "change token expired; full replay");
                        self.store.mutate(|l| l.change_token = None)?;
                        continue 'restart;
                    }
                    Err(SyncError::ZoneMissing) if !recreated => {
                        recreated = true;
                        self.observer
                            .event("zone_recreated", "zone missing; re-preparing");
                        // Every remote record is gone: what the ledger believes is
                        // published no longer exists, so local items must
                        // re-publish (donor: acknowledgements are invalidated).
                        self.store.mutate(|l| {
                            l.change_token = None;
                            l.remote_versions.clear();
                            l.aliases.clear();
                        })?;
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
        canonical: &BTreeMap<String, RemoteEntry>,
        generation: u64,
        rep: &mut PassReport,
    ) -> SyncResult<bool> {
        let ids: Vec<String> = self
            .store
            .ledger()
            .pending_tombstones
            .iter()
            .cloned()
            .collect();
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
            let mut names: BTreeSet<String> = self
                .store
                .ledger()
                .aliases
                .get(id)
                .cloned()
                .unwrap_or_default();
            let known =
                self.store.ledger().remote_versions.contains_key(id) || canonical.contains_key(id);
            if names.is_empty() && !known {
                never_remote.push(id.clone());
                continue;
            }
            names.insert(self.mapper.record_name(id));
            let at = self
                .store
                .ledger()
                .tombstones
                .get(id)
                .copied()
                .unwrap_or_else(|| self.clock.now_ms());
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
                writes.push(Write {
                    id: id.clone(),
                    record,
                });
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
        let size = self
            .config
            .tombstone_batch
            .min(self.backend.max_batch())
            .max(1);
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
                    self.observer
                        .event("tombstone_save_failed", &w.record.record_name);
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
