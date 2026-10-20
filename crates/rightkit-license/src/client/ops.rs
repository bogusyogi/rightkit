//! The configured client: every licence operation.
//!
//! One operation = one hold of the licence operation lock for the WHOLE workflow: acquire,
//! read, (reconcile a leftover marker), network, verify, clock compare-and-swap, record write or
//! tombstone, marker cleanup. The guard is never dropped between steps. Each record write runs on
//! a fenced worker that owns the guard until its credentials calls, including any timed-out child
//! call, have returned (see `persist` and `lock`); the guard comes back to the operation when the
//! worker finishes inside the caller's wait, and stays with the worker when it does not.
//!
//! Order inside an operation: operation lock, then (inside the credentials calls) rotation, then
//! clock. The clock is written BEFORE the record and a token is verified BEFORE either, so a
//! crash between the two writes can only fail closed (fresh anchor, old record).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rightkit_credentials::Store;
use serde_json::{Map, Value};
use zeroize::Zeroizing;

use super::clockstore::{is_lock_held, PolledClockStore};
use super::envelope::{Envelope, LeaseRecord, RecordState, RecordStatus, StoredRecord};
use super::invalidation::LicenseInvalidation;
use super::lock::{OpGuard, OpLock};
use super::order::{self, Level, LevelGuard};
use super::persist::{outcome_result, Loaded, RecordStore, Snapshot, Written};
use super::time::{parse_rfc3339, to_ms};
use super::types::{DeviceIdSource, LicenseClientConfig, SecretString};
use super::view::{
    LicenseError, LicenseMode, LicenseView, MessageCode, RefreshView, RefreshViewOutcome,
    RejectReason, RevisionState, TrialView, ViewKind, ViewStage, ViewStatus,
};
use super::wire::{Answer, HttpRefreshTransport, Reply, Transport, TrialAnswer};
use crate::{
    authenticate_token_any_device, evaluate_lifecycle, judge_refresh_response,
    refresh_and_reanchor, system_now_unix, verify_lease_envelope_fields, AntiRollbackClock,
    ClockError, ClockRefusal, ClockState, Entitlement, EntitlementStage, FileClockStoreError,
    LeaseEnvelope, LicenseKind, Lifecycle, LifecycleContext, LifecycleReport, ProcessAnchor,
    RefreshJudgement, RefreshOutcome, RefreshParams, RefreshRejection, RefreshResponse,
    StoredCredential, CLOCK_ROLLBACK_TOLERANCE_SECS, MAX_TOKEN_BYTES,
};

/// A status or authorization reader's reconcile budget. Longer work belongs to a mutation.
const READER_RECONCILE_WAIT: Duration = Duration::from_secs(3);

/// A legacy credential item to import from (and remove after the import commits).
pub struct LegacySource {
    pub store: Store,
    pub account: String,
}

pub(crate) struct Core {
    pub(crate) cfg: LicenseClientConfig,
    pub(crate) device: Arc<dyn DeviceIdSource>,
    pub(crate) op: OpLock,
    pub(crate) records: RecordStore,
    pub(crate) clock: AntiRollbackClock<PolledClockStore>,
    pub(crate) transport: Transport,
    last_view: Mutex<Option<LicenseView>>,
    /// The stored record may have changed since the live engines were last told.
    dirty: AtomicBool,
    invalidation: LicenseInvalidation,
}

/// The lock held, the item read.
pub(crate) struct Locked {
    pub guard: OpGuard,
    pub _level: LevelGuard,
    pub snap: Snapshot,
}

/// What a snapshot means right now.
pub(crate) struct Evaluation {
    pub view: LicenseView,
    /// The authenticated entitlement of the stored token, when it authenticated.
    pub entitlement: Option<Entitlement>,
    /// The lifecycle grants access (including the lease window).
    pub grants: bool,
    pub revision: Option<String>,
    pub record: Option<StoredRecord>,
}

impl Evaluation {
    fn refused(view: LicenseView) -> Self {
        Self {
            view,
            entitlement: None,
            grants: false,
            revision: None,
            record: None,
        }
    }
}

pub(crate) fn clock_error(error: ClockError<FileClockStoreError>) -> LicenseError {
    match error {
        ClockError::Missing => LicenseError::Clock(ClockRefusal::Missing),
        ClockError::ReplayedAnchor {
            last_server_iat, ..
        } => LicenseError::Clock(ClockRefusal::Rollback {
            highest_seen: last_server_iat,
        }),
        ClockError::Contended => LicenseError::Busy,
        ClockError::Storage(error) if is_lock_held(&error) => LicenseError::Busy,
        // Fixed text: the storage error is not carried.
        ClockError::Storage(_) => LicenseError::Clock(ClockRefusal::Storage("ledger".to_string())),
    }
}

pub(crate) fn wall_now() -> Result<i64, LicenseError> {
    system_now_unix()
        .map_err(|_| LicenseError::Clock(ClockRefusal::Unavailable("system clock".to_string())))
}

fn view_kind(kind: LicenseKind) -> ViewKind {
    match kind {
        LicenseKind::Trial => ViewKind::Trial,
        LicenseKind::Lifetime => ViewKind::Lifetime,
        LicenseKind::Term => ViewKind::Term,
    }
}

fn view_stage(stage: EntitlementStage) -> ViewStage {
    match stage {
        EntitlementStage::TimeBound => ViewStage::TimeBound,
        EntitlementStage::Provisional => ViewStage::Provisional,
        EntitlementStage::Final => ViewStage::Final,
    }
}

fn view_status(lifecycle: Lifecycle) -> ViewStatus {
    match lifecycle {
        Lifecycle::Trial
        | Lifecycle::Active
        | Lifecycle::Lifetime
        | Lifecycle::LifetimeProvisional => ViewStatus::Active,
        Lifecycle::Grace => ViewStatus::Grace,
        Lifecycle::Expired | Lifecycle::ExpiredTrial | Lifecycle::VerificationRequired => {
            ViewStatus::Expired
        }
        Lifecycle::Revoked => ViewStatus::Revoked,
        Lifecycle::Invalid => ViewStatus::Invalid,
        Lifecycle::Unlicensed => ViewStatus::None,
        // MigrationRequired, ClockRepairRequired, and anything added later: not established.
        _ => ViewStatus::Unverifiable,
    }
}

fn view_message(lifecycle: Lifecycle, entitlement: Option<&Entitlement>) -> MessageCode {
    match lifecycle {
        Lifecycle::Revoked => MessageCode::Revoked,
        Lifecycle::Expired | Lifecycle::ExpiredTrial => MessageCode::Expired,
        Lifecycle::MigrationRequired | Lifecycle::VerificationRequired => {
            MessageCode::MigrationRequired
        }
        Lifecycle::ClockRepairRequired => MessageCode::ClockRepairRequired,
        Lifecycle::Invalid => match entitlement {
            Some(e) if !e.has_valid_lifecycle() => MessageCode::InvalidLifecycle,
            Some(_) => MessageCode::IssuedInTheFuture,
            None => MessageCode::InvalidToken,
        },
        _ => MessageCode::None,
    }
}

/// A re-anchor that moved the trusted mark back by more than the rollback tolerance: the
/// clock-repair path. A few seconds of skew between this machine and the server is not one.
fn repaired(before: Option<&ClockState>, server_time: i64, lowered: bool) -> bool {
    lowered
        && before.is_some_and(|state| {
            state.highest_seen.saturating_sub(server_time) > CLOCK_ROLLBACK_TOLERANCE_SECS
        })
}

pub(crate) fn suffix(id: &str) -> Option<String> {
    let chars: Vec<char> = id.chars().collect();
    if chars.is_empty() {
        return None;
    }
    Some(chars[chars.len().saturating_sub(4)..].iter().collect())
}

impl Core {
    pub(crate) fn new(
        cfg: LicenseClientConfig,
        device: Arc<dyn DeviceIdSource>,
        invalidation: LicenseInvalidation,
    ) -> Result<Self, LicenseError> {
        let wall = wall_now()?;
        let transport = Transport::new(&cfg)?;
        Ok(Self {
            op: OpLock::new(cfg.license_dir.join(".lock")),
            records: RecordStore::new(cfg.store.clone(), cfg.account.clone(), cfg.persist_wait),
            clock: AntiRollbackClock::new(
                PolledClockStore::new(cfg.clock_path.clone()),
                ProcessAnchor::new(wall),
            ),
            transport,
            device,
            last_view: Mutex::new(None),
            dirty: AtomicBool::new(false),
            invalidation,
            cfg,
        })
    }

    /// Run one public operation, and when it changed (or tried to change) the stored record,
    /// tell the live engine verifiers afterwards. `f` owns the operation lock and has released
    /// it when it returns, so a subscriber may call back into the client.
    pub(crate) fn signalled<R>(&self, f: impl FnOnce() -> R) -> R {
        let result = f();
        self.flush_invalidation();
        result
    }

    pub(crate) fn flush_invalidation(&self) {
        if self.dirty.swap(false, Ordering::SeqCst) {
            self.invalidation.signal();
        }
    }

    pub(crate) fn remember(&self, view: &LicenseView) {
        if !view.unresolved && !view.busy {
            if let Ok(mut last) = self.last_view.lock() {
                *last = Some(view.clone());
            }
        }
    }

    /// The display-only view when the lock is held: the last committed one, marked busy.
    fn busy_view(&self) -> LicenseView {
        let last = self.last_view.lock().ok().and_then(|last| last.clone());
        match last {
            Some(view) => view.with_busy(),
            None => LicenseView::unverifiable(MessageCode::Busy, false).with_busy(),
        }
    }

    pub(crate) fn is_unresolved(&self, snap: &Snapshot) -> bool {
        snap.unresolved() || self.records.flagged_unresolved()
    }

    /// Take the lock, read the item, and try to resolve a leftover marker. The result can still
    /// be unresolved (a child call is running, or the store failed): callers check
    /// [`Core::is_unresolved`] and fail closed.
    pub(crate) fn lock_and_read(
        &self,
        lock_wait: Duration,
        reconcile_wait: Duration,
    ) -> Result<Locked, LicenseError> {
        let level = order::enter(Level::Op);
        let mut guard = self.op.acquire(lock_wait)?;
        let mut snap = self.records.read()?;
        if self.is_unresolved(&snap) && !snap.held {
            match self.records.reconcile(guard, reconcile_wait) {
                Written::Done(_, held) => {
                    // A leftover marker was rolled forward or discarded: the stored record may
                    // be a different one now.
                    self.dirty.store(true, Ordering::SeqCst);
                    guard = held;
                    snap = self.records.read()?;
                }
                Written::TimedOut | Written::Failed => return Err(LicenseError::Unresolved),
            }
        }
        Ok(Locked {
            guard,
            _level: level,
            snap,
        })
    }

    /// The start of every mutation: the lock for the mutation wait, the item read, a leftover
    /// marker reconciled; `Unresolved` when it cannot be.
    pub(crate) fn begin(&self) -> Result<Locked, LicenseError> {
        let locked = self.lock_and_read(self.cfg.mutation_wait, self.cfg.persist_wait)?;
        if locked.snap.held {
            return Err(LicenseError::Busy);
        }
        if self.is_unresolved(&locked.snap) {
            return Err(LicenseError::Unresolved);
        }
        Ok(locked)
    }

    /// Write `envelope` on the fenced worker; the guard comes back on success.
    pub(crate) fn commit(
        &self,
        guard: OpGuard,
        envelope: &Envelope,
    ) -> Result<OpGuard, LicenseError> {
        // Any attempt to change the stored record, whatever its outcome, means a reader's
        // cached verdict may no longer be the stored one: `signalled` tells the live engines.
        self.dirty.store(true, Ordering::SeqCst);
        match self.records.write(guard, envelope) {
            Written::Done(outcome, guard) => outcome_result(outcome).map(|()| guard),
            Written::TimedOut | Written::Failed => Err(LicenseError::Unresolved),
        }
    }

    /// The view of `envelope` as just written.
    pub(crate) fn view_after(&self, envelope: &Envelope) -> LicenseView {
        let snap = Snapshot {
            loaded: Loaded::Present(Box::new(envelope.clone())),
            marker: false,
            held: false,
        };
        let view = self.evaluate(&snap).view;
        self.remember(&view);
        view
    }

    // ---- evaluation -------------------------------------------------------------------

    pub(crate) fn evaluate(&self, snap: &Snapshot) -> Evaluation {
        let mode = LicenseMode::Configured;
        if self.is_unresolved(snap) {
            return Evaluation::refused(LicenseView::unverifiable(MessageCode::Unresolved, true));
        }
        let envelope = match &snap.loaded {
            Loaded::Absent => return Evaluation::refused(LicenseView::none(mode)),
            Loaded::Corrupt => {
                return Evaluation::refused(LicenseView {
                    status: ViewStatus::Invalid,
                    lifecycle: Lifecycle::Invalid,
                    message: MessageCode::InvalidToken,
                    ..LicenseView::none(mode)
                })
            }
            Loaded::Present(envelope) => &**envelope,
        };
        let record = match (&envelope.state, &envelope.record) {
            (RecordState::Active, Some(record)) => record,
            _ => {
                return Evaluation::refused(LicenseView {
                    revision_state: RevisionState::Tombstone,
                    ..LicenseView::none(mode)
                })
            }
        };
        let device_id = if record.device_id.is_empty() {
            match self.device.device_id() {
                Ok(id) => id,
                Err(_) => {
                    return Evaluation::refused(LicenseView::unverifiable(
                        MessageCode::VerifyFailed,
                        false,
                    ))
                }
            }
        } else {
            record.device_id.clone()
        };
        let Ok(wall) = wall_now() else {
            return Evaluation::refused(LicenseView::unverifiable(
                MessageCode::ClockUnavailable,
                false,
            ));
        };
        let credential = match (
            record.status,
            record.signed_token(),
            record.activation_token(),
        ) {
            (RecordStatus::Revoked, _, _) => StoredCredential::Revoked,
            (RecordStatus::Invalid, _, _) => StoredCredential::Unsigned,
            (RecordStatus::Active, Some(token), _) => StoredCredential::Signed {
                token,
                clock_initialized: record.clock_initialized,
            },
            (RecordStatus::Active, None, Some(_)) => StoredCredential::LegacyUnsigned,
            (RecordStatus::Active, None, None) => StoredCredential::Unsigned,
        };
        let ctx = LifecycleContext::new(&self.cfg.public_key, &self.cfg.app, &device_id)
            .with_policy(self.cfg.policy);
        let report = match evaluate_lifecycle(&ctx, &self.clock, credential, wall) {
            Ok(report) => report,
            Err(error) => {
                let (lifecycle, message) = match clock_error(error) {
                    LicenseError::Busy => {
                        return Evaluation::refused(self.busy_view());
                    }
                    LicenseError::Clock(ClockRefusal::Missing) => (
                        Lifecycle::ClockRepairRequired,
                        MessageCode::ClockRepairRequired,
                    ),
                    _ => (
                        Lifecycle::ClockRepairRequired,
                        MessageCode::ClockUnavailable,
                    ),
                };
                return Evaluation::refused(LicenseView {
                    status: ViewStatus::Unverifiable,
                    lifecycle,
                    message,
                    revision_state: RevisionState::Active,
                    ..LicenseView::none(mode)
                });
            }
        };
        self.evaluation_from(envelope, record, report)
    }

    fn evaluation_from(
        &self,
        envelope: &Envelope,
        record: &StoredRecord,
        report: LifecycleReport,
    ) -> Evaluation {
        let mut lifecycle = report.lifecycle;
        // A lease is good until its own expiry plus grace, whatever the nested token says.
        if lifecycle.grants_access() {
            if let (Some(lease), Some(now)) = (&record.lease, report.effective_now) {
                if now >= lease.expires_at.saturating_add(lease.grace_seconds.max(0)) {
                    lifecycle = Lifecycle::Expired;
                }
            }
        }
        let grants = lifecycle.grants_access();
        let entitlement = report.entitlement.clone();
        let message = if lifecycle == Lifecycle::Expired && report.lifecycle != Lifecycle::Expired {
            MessageCode::Expired
        } else {
            view_message(lifecycle, entitlement.as_ref())
        };
        let view = LicenseView {
            mode: LicenseMode::Configured,
            status: view_status(lifecycle),
            lifecycle,
            tier: entitlement.as_ref().map(|e| e.tier.clone()),
            features: entitlement
                .as_ref()
                .map(|e| e.features.clone())
                .unwrap_or_default(),
            kind: entitlement
                .as_ref()
                .and_then(|e| e.license_kind)
                .map(view_kind),
            stage: entitlement
                .as_ref()
                .and_then(|e| e.entitlement_stage)
                .map(view_stage),
            valid_until: entitlement.as_ref().map(|e| to_ms(e.exp)),
            confirmation_due_at: report.confirmation_due_at.map(to_ms),
            stale_since: record.stale_since.map(to_ms),
            license_id_suffix: entitlement
                .as_ref()
                .map(|e| e.id.clone())
                .or_else(|| record.license_id.clone())
                .as_deref()
                .and_then(suffix),
            busy: false,
            revision_state: RevisionState::Active,
            unresolved: false,
            message,
        };
        Evaluation {
            view,
            entitlement,
            grants,
            revision: envelope.revision.clone(),
            record: Some(record.clone()),
        }
    }

    // ---- status -----------------------------------------------------------------------

    pub(crate) fn status(&self) -> LicenseView {
        let view = self.status_inner();
        self.flush_invalidation();
        view
    }

    fn status_inner(&self) -> LicenseView {
        let locked = match self.lock_and_read(self.cfg.status_wait, READER_RECONCILE_WAIT) {
            Ok(locked) => locked,
            Err(LicenseError::Busy) => return self.busy_view(),
            Err(LicenseError::Unresolved) => {
                return LicenseView::unverifiable(MessageCode::Unresolved, true)
            }
            Err(_) => return LicenseView::unverifiable(MessageCode::StoreFailed, false),
        };
        let view = self.evaluate(&locked.snap).view;
        self.remember(&view);
        view
    }

    /// The authorization use: one hold of the lock (a bounded 250 ms try by default), the item
    /// read after any marker was reconciled, then `f` runs on what was read WITH THE LOCK STILL
    /// HELD. `Busy` when the lock is held, `Unresolved` while a marker or an unfinished write
    /// stands; a cached or busy view is never used.
    pub(crate) fn authorized<R>(
        &self,
        f: impl FnOnce(&Evaluation) -> R,
    ) -> Result<R, LicenseError> {
        let result = self.authorized_inner(f);
        self.flush_invalidation();
        result
    }

    fn authorized_inner<R>(&self, f: impl FnOnce(&Evaluation) -> R) -> Result<R, LicenseError> {
        let locked = self.lock_and_read(self.cfg.status_wait, READER_RECONCILE_WAIT)?;
        if self.is_unresolved(&locked.snap) {
            return Err(LicenseError::Unresolved);
        }
        let evaluation = self.evaluate(&locked.snap);
        if evaluation.view.busy {
            return Err(LicenseError::Busy);
        }
        Ok(f(&evaluation))
    }

    // ---- activate ---------------------------------------------------------------------

    pub(crate) fn activate(&self, key: SecretString) -> Result<LicenseView, LicenseError> {
        let trimmed = key.expose().trim();
        if trimmed.is_empty() {
            return Err(LicenseError::Rejected {
                reason: RejectReason::KeyNotAccepted,
            });
        }
        let device_id = self.device.device_id().map_err(|_| LicenseError::Verify)?;
        let Locked { guard, _level, .. } = self.begin()?;
        let answer = self.transport.activate(&device_id, trimmed)?;
        drop(key);
        let body = match answer {
            Answer::Body(body) => body,
            Answer::Unusable { http: 429 } => {
                return Err(LicenseError::Rejected {
                    reason: RejectReason::RateLimited,
                })
            }
            Answer::Unusable { .. } => return Err(LicenseError::Network),
        };
        if body.status != "active" {
            return Err(LicenseError::Rejected {
                reason: RejectReason::from_server(Some(&body.status), 0),
            });
        }
        let wall = wall_now()?;
        let token = body
            .signed_token
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_owned);
        let adopted = self.adopt(&device_id, token.as_deref(), None, false, wall)?;
        let Adopted::Replace {
            token, entitlement, ..
        } = adopted
        else {
            // No new token in an active answer: nothing verifiable to store.
            return Err(LicenseError::Verify);
        };
        let mut record = StoredRecord::new(device_id);
        record.activation_token = body
            .extra
            .get("activationToken")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(|t| Zeroizing::new(t.to_owned()));
        record.signed_token = Some(Zeroizing::new(token));
        record.license_id = Some(entitlement.id.clone());
        record.clock_initialized = true;
        record.activated_at = Some(wall);
        record.last_refreshed_at = Some(wall);
        let envelope = Envelope::active(record)?;
        let _held = self.commit(guard, &envelope)?;
        Ok(self.view_after(&envelope))
    }

    /// Authenticate a candidate token, judge it at its own issue time, and (for a new token)
    /// move the clock. Used by activate, lease issue and lease renew. The clock is written here,
    /// before the caller writes the record.
    fn adopt(
        &self,
        bound_id: &str,
        candidate: Option<&str>,
        cached: Option<&str>,
        clock_initialized: bool,
        wall: i64,
    ) -> Result<Adopted, LicenseError> {
        let mut params =
            RefreshParams::new(&self.cfg.public_key, &self.cfg.app, bound_id, "", wall);
        params.cached_token = cached;
        params.clock_initialized = clock_initialized;
        params.policy = self.cfg.policy;
        let response = RefreshResponse {
            status: "active".to_string(),
            signed_token: candidate.map(str::to_owned),
            message: None,
            extra: Map::new(),
        };
        let prior = self.clock.load().map_err(clock_error)?;
        match judge_refresh_response(&params, &response, prior.as_ref()) {
            RefreshJudgement::ReplaceToken {
                token, entitlement, ..
            } => {
                let reanchored = self
                    .clock
                    .reanchor(entitlement.iat, wall, clock_initialized)
                    .map_err(clock_error)?;
                let lowered = repaired(prior.as_ref(), entitlement.iat, reanchored.lowered);
                Ok(Adopted::Replace {
                    token,
                    entitlement,
                    lowered,
                })
            }
            RefreshJudgement::NoNewToken { .. } => Ok(Adopted::Keep),
            RefreshJudgement::LedgerMissing => Err(LicenseError::Clock(ClockRefusal::Missing)),
            RefreshJudgement::Rejected { rejection, .. } => Err(match rejection {
                RefreshRejection::ReplayedServerTime {
                    last_server_iat, ..
                } => LicenseError::Clock(ClockRefusal::Rollback {
                    highest_seen: last_server_iat,
                }),
                RefreshRejection::NotActive(
                    Lifecycle::Expired | Lifecycle::ExpiredTrial | Lifecycle::VerificationRequired,
                ) => LicenseError::Rejected {
                    reason: RejectReason::Expired,
                },
                _ => LicenseError::Verify,
            }),
            // A status other than active never reaches here; anything new fails closed.
            _ => Err(LicenseError::Verify),
        }
    }

    // ---- refresh ----------------------------------------------------------------------

    pub(crate) fn refresh(&self) -> Result<RefreshView, LicenseError> {
        let Locked {
            guard,
            snap,
            _level,
        } = self.begin()?;
        let record = match &snap.loaded {
            Loaded::Present(envelope) if envelope.state == RecordState::Active => {
                envelope.record.clone()
            }
            _ => None,
        };
        let Some(record) = record else {
            return Ok(self.no_credential(&snap));
        };
        if record.status != RecordStatus::Active || record.activation_token().is_none() {
            return Ok(self.no_credential(&snap));
        }
        if record.lease.is_some() {
            return self.renew_locked(guard, &snap, record);
        }
        let wall = wall_now()?;
        let activation = Zeroizing::new(record.activation_token().unwrap_or_default().to_owned());
        let mut params = RefreshParams::new(
            &self.cfg.public_key,
            &self.cfg.app,
            &record.device_id,
            &activation,
            wall,
        );
        params.cached_token = record.signed_token();
        params.clock_initialized = record.clock_initialized;
        params.stale_since = record.stale_since;
        params.policy = self.cfg.policy;
        let transport = HttpRefreshTransport {
            transport: &self.transport,
        };
        let before = self.clock.load().map_err(clock_error)?;
        let outcome =
            refresh_and_reanchor(&params, &transport, &self.clock).map_err(clock_error)?;
        drop(activation);
        let mut next = record.clone();
        let mut write = true;
        let outcome_view = match outcome {
            RefreshOutcome::Stale { stale_since, .. } => {
                write = record.stale_since != Some(stale_since);
                next.stale_since = Some(stale_since);
                RefreshViewOutcome::Stale
            }
            RefreshOutcome::Revoked { .. } => {
                next = revoked_record(&record);
                RefreshViewOutcome::Revoked
            }
            RefreshOutcome::TokenReplaced {
                token,
                entitlement,
                reanchored,
                ..
            } => {
                next.signed_token = Some(Zeroizing::new(token));
                next.stale_since = None;
                next.last_refreshed_at = Some(wall);
                next.clock_initialized = true;
                if repaired(before.as_ref(), entitlement.iat, reanchored.lowered) {
                    RefreshViewOutcome::Reanchored
                } else {
                    RefreshViewOutcome::Refreshed
                }
            }
            RefreshOutcome::TokenKept { .. } => {
                write = record.stale_since.is_some();
                next.stale_since = None;
                next.last_refreshed_at = Some(wall);
                RefreshViewOutcome::Refreshed
            }
            RefreshOutcome::TokenDropped { .. } => {
                next.signed_token = None;
                next.stale_since = None;
                next.last_refreshed_at = Some(wall);
                RefreshViewOutcome::Refreshed
            }
            RefreshOutcome::Rejected(rejection) => {
                write = false;
                match rejection {
                    RefreshRejection::ReplayedServerTime { .. } => RefreshViewOutcome::ClockRefused,
                    _ => RefreshViewOutcome::Stale,
                }
            }
        };
        if !write {
            let view = self.evaluate(&snap).view;
            self.remember(&view);
            return Ok(RefreshView {
                view,
                outcome: outcome_view,
            });
        }
        let envelope = Envelope::active(next)?;
        let _held = self.commit(guard, &envelope)?;
        Ok(RefreshView {
            view: self.view_after(&envelope),
            outcome: outcome_view,
        })
    }

    pub(crate) fn no_credential(&self, snap: &Snapshot) -> RefreshView {
        let view = self.evaluate(snap).view;
        self.remember(&view);
        RefreshView {
            view,
            outcome: RefreshViewOutcome::NoCredential,
        }
    }

    // ---- deactivate -------------------------------------------------------------------

    pub(crate) fn deactivate(&self) -> Result<LicenseView, LicenseError> {
        let Locked {
            guard,
            snap,
            _level,
        } = self.begin()?;
        let record = match &snap.loaded {
            // Nothing stored, or already a tombstone: nothing to clear.
            Loaded::Absent => {
                let view = LicenseView::none(LicenseMode::Configured);
                self.remember(&view);
                return Ok(view);
            }
            Loaded::Present(envelope) if envelope.is_tombstone() => {
                let view = self.evaluate(&snap).view;
                self.remember(&view);
                return Ok(view);
            }
            Loaded::Present(envelope) => envelope.record.clone(),
            Loaded::Corrupt => None,
        };
        // What the server call below needs, taken before the record goes.
        let release = match &record {
            Some(record) => match &record.lease {
                Some(lease) => Release::Lease(lease.lease_id.clone()),
                None => record
                    .activation_token()
                    .map(|token| Release::Activation(Zeroizing::new(token.to_owned())))
                    .unwrap_or(Release::None),
            },
            None => Release::None,
        };
        // The local clear is authoritative and goes first: a clear that fails is an error and
        // frees nothing on the server, so the two never disagree about a licence that is
        // still here.
        let tombstone = Envelope::tombstone()?;
        let _held = self.commit(guard, &tombstone)?;
        // Best effort, still under the operation lock: free the seat.
        match release {
            Release::Lease(lease_id) => {
                if let Ok(hash) = self.device.device_id_hash() {
                    let _ = self.transport.lease_remove(&hash, &lease_id);
                }
            }
            Release::Activation(token) => {
                let _ = self.transport.deactivate(&token);
            }
            Release::None => {}
        }
        let view = LicenseView {
            revision_state: RevisionState::Tombstone,
            ..LicenseView::none(LicenseMode::Configured)
        };
        self.remember(&view);
        Ok(view)
    }

    // ---- trial ------------------------------------------------------------------------

    pub(crate) fn request_trial(&self, email: &str) -> Result<TrialView, LicenseError> {
        let email = email.trim();
        if !plausible_email(email) {
            return Ok(TrialView {
                accepted: false,
                message: MessageCode::TrialRefused,
            });
        }
        let level = order::enter(Level::Op);
        let _guard = self.op.acquire(self.cfg.mutation_wait)?;
        let answer = self.transport.trial(email)?;
        drop(level);
        match answer {
            TrialAnswer::Accepted => Ok(TrialView {
                accepted: true,
                message: MessageCode::TrialRequested,
            }),
            TrialAnswer::Declined => Ok(TrialView {
                accepted: false,
                message: MessageCode::TrialRefused,
            }),
            TrialAnswer::RateLimited => Err(LicenseError::Rejected {
                reason: RejectReason::RateLimited,
            }),
        }
    }

    // ---- leases -----------------------------------------------------------------------

    pub(crate) fn lease_activate(
        &self,
        key: SecretString,
        label: Option<&str>,
    ) -> Result<LicenseView, LicenseError> {
        if key.is_blank() {
            return Err(LicenseError::Rejected {
                reason: RejectReason::KeyNotAccepted,
            });
        }
        let hash = self
            .device
            .device_id_hash()
            .map_err(|_| LicenseError::Verify)?;
        let Locked {
            guard,
            snap,
            _level,
        } = self.begin()?;
        let reply =
            self.transport
                .lease_issue(key.expose().trim(), &hash, std::env::consts::OS, label)?;
        drop(key);
        let wire = lease_reply(&reply)?;
        let prior = match &snap.loaded {
            Loaded::Present(envelope) => envelope.record.as_ref(),
            _ => None,
        };
        let wall = wall_now()?;
        let (record, _lowered) = self.lease_record(&hash, wire, prior, wall)?;
        let envelope = Envelope::active(record)?;
        let _held = self.commit(guard, &envelope)?;
        Ok(self.view_after(&envelope))
    }

    pub(crate) fn lease_renew(&self) -> Result<RefreshView, LicenseError> {
        let Locked {
            guard,
            snap,
            _level,
        } = self.begin()?;
        let record = match &snap.loaded {
            Loaded::Present(envelope) if envelope.state == RecordState::Active => {
                envelope.record.clone()
            }
            _ => None,
        };
        match record {
            Some(record) if record.lease.is_some() && record.status == RecordStatus::Active => {
                self.renew_locked(guard, &snap, record)
            }
            _ => Ok(self.no_credential(&snap)),
        }
    }

    fn renew_locked(
        &self,
        guard: OpGuard,
        snap: &Snapshot,
        record: StoredRecord,
    ) -> Result<RefreshView, LicenseError> {
        let Some(lease) = record.lease.clone() else {
            return Ok(self.no_credential(snap));
        };
        let hash = self
            .device
            .device_id_hash()
            .map_err(|_| LicenseError::Verify)?;
        let wall = wall_now()?;
        let reply = self.transport.lease_renew(&hash, &lease.lease_id);
        let (next, outcome) = match reply {
            Ok(reply) if reply.status == 410 || reply.status == 404 => {
                // The server says this device's lease is gone: access ends now.
                (revoked_record(&record), RefreshViewOutcome::Revoked)
            }
            Ok(reply) if reply.status == 200 => {
                let wire = lease_reply(&reply)?;
                let (next, lowered) = self.lease_record(&hash, wire, Some(&record), wall)?;
                (
                    next,
                    if lowered {
                        RefreshViewOutcome::Reanchored
                    } else {
                        RefreshViewOutcome::Refreshed
                    },
                )
            }
            // Offline, 5xx, anything else: the record stands, the offline episode is noted.
            _ => {
                if record.stale_since.is_some() {
                    let view = self.evaluate(snap).view;
                    self.remember(&view);
                    return Ok(RefreshView {
                        view,
                        outcome: RefreshViewOutcome::Stale,
                    });
                }
                let mut next = record.clone();
                next.stale_since = Some(wall);
                (next, RefreshViewOutcome::Stale)
            }
        };
        let envelope = Envelope::active(next)?;
        let _held = self.commit(guard, &envelope)?;
        Ok(RefreshView {
            view: self.view_after(&envelope),
            outcome,
        })
    }

    /// Verify a lease answer and build the record it makes: the envelope signature, this
    /// device's hash, generation ordering, the nested entitlement (bound to the hash) and the
    /// clock. Nothing is stored here.
    pub(crate) fn lease_record(
        &self,
        hash: &str,
        wire: LeaseWire,
        prior: Option<&StoredRecord>,
        wall: i64,
    ) -> Result<(StoredRecord, bool), LicenseError> {
        let lease = wire.lease;
        let envelope = LeaseEnvelope {
            lease_id: lease.lease_id.clone(),
            device_id_hash: lease.device_id_hash.clone().unwrap_or_default(),
            pool: lease.pool.clone().unwrap_or_default(),
            issued_at: lease.issued_at.clone().unwrap_or_default(),
            expires_at: lease.expires_at_text.clone().unwrap_or_default(),
            grace_seconds: lease.grace_seconds,
            generation: lease.generation,
            unverified_entitlement: None,
        };
        let signature = lease.signature.clone().unwrap_or_default();
        verify_lease_envelope_fields(&envelope, &signature, &self.cfg.public_key)
            .map_err(|_| LicenseError::Verify)?;
        // The lease is for this device, and not older than the one already held.
        if envelope.device_id_hash != hash {
            return Err(LicenseError::Verify);
        }
        if let Some(held) = prior.and_then(|p| p.lease.as_ref()) {
            if lease.generation < held.generation {
                return Err(LicenseError::Verify);
            }
        }
        let prior_token = prior.and_then(|p| p.signed_token());
        let adopted = self.adopt(
            hash,
            wire.entitlement.as_deref(),
            prior_token,
            prior.is_some_and(|p| p.clock_initialized),
            wall,
        )?;
        let mut record = prior
            .cloned()
            .filter(|p| p.status == RecordStatus::Active)
            .unwrap_or_else(|| StoredRecord::new(hash.to_string()));
        record.status = RecordStatus::Active;
        record.device_id = hash.to_string();
        if let Some(token) = wire.activation_token {
            record.activation_token = Some(token);
        }
        let lowered = match adopted {
            Adopted::Replace {
                token,
                entitlement,
                lowered,
            } => {
                record.signed_token = Some(Zeroizing::new(token));
                record.license_id = Some(entitlement.id);
                record.clock_initialized = true;
                lowered
            }
            Adopted::Keep => {
                if record.signed_token.is_none() {
                    return Err(LicenseError::Verify);
                }
                false
            }
        };
        if record.activation_token.is_none() {
            return Err(LicenseError::Verify);
        }
        record.lease = Some(lease);
        record.stale_since = None;
        record.activated_at.get_or_insert(wall);
        record.last_refreshed_at = Some(wall);
        Ok((record, lowered))
    }

    // ---- import -----------------------------------------------------------------------

    pub(crate) fn import_legacy_json(
        &self,
        json: SecretString,
    ) -> Result<LicenseView, LicenseError> {
        let Locked {
            guard,
            snap,
            _level,
        } = self.begin()?;
        self.import_locked(guard, &snap, json.expose())
            .map(|(view, _)| view)
    }

    pub(crate) fn import_legacy(&self, source: &LegacySource) -> Result<LicenseView, LicenseError> {
        let Locked {
            guard,
            snap,
            _level,
        } = self.begin()?;
        let text = match source.store.get(&source.account) {
            Ok(Some(secret)) if !secret.is_empty() => secret,
            Ok(_) => {
                let view = self.evaluate(&snap).view;
                self.remember(&view);
                return Ok(view);
            }
            Err(_) => return Err(LicenseError::Store),
        };
        let json = SecretString::new(text.expose_str().map_err(|_| LicenseError::Store)?);
        drop(text);
        let (view, guard) = self.import_locked(guard, &snap, json.expose())?;
        // The legacy item is removed only after the new record committed, and never when the
        // legacy item IS the licence item (an in-place upgrade of the older bare record).
        if let Some(guard) = guard {
            let same_item = source.account == self.cfg.account
                && source.store.namespace() == self.cfg.store.namespace();
            if !same_item {
                let (legacy, account) = (source.store.clone(), source.account.clone());
                let _ = self.records.fenced(guard, move || {
                    let _ = legacy.delete(&account);
                });
            }
        }
        Ok(view)
    }

    /// Import under the held lock. Returns the view and, when a record was written or the state
    /// already was migrated, the guard (for the caller's legacy delete).
    fn import_locked(
        &self,
        guard: OpGuard,
        snap: &Snapshot,
        json: &str,
    ) -> Result<(LicenseView, Option<OpGuard>), LicenseError> {
        // An envelope already stored (active or tombstone) means this install has migrated or
        // deactivated: a legacy copy must never bring a deactivated licence back.
        match &snap.loaded {
            Loaded::Present(envelope) if envelope.revision.is_some() => {
                let view = self.evaluate(snap).view;
                self.remember(&view);
                return Ok((view, Some(guard)));
            }
            Loaded::Corrupt => return Err(LicenseError::Verify),
            _ => {}
        }
        let parsed = Envelope::parse(json).map_err(|_| LicenseError::Verify)?;
        let Some(mut record) = parsed
            .record
            .filter(|_| parsed.state == RecordState::Active)
        else {
            // A legacy tombstone or empty record: no licence to import.
            return Ok((LicenseView::none(LicenseMode::Configured), Some(guard)));
        };
        if record.status != RecordStatus::Active {
            // A dead legacy record grants nothing and is not carried over.
            return Ok((LicenseView::none(LicenseMode::Configured), Some(guard)));
        }
        let Some(token) = record.signed_token().map(str::to_owned) else {
            // Nothing verifiable: an activation credential alone is not imported.
            return Ok((LicenseView::none(LicenseMode::Configured), Some(guard)));
        };
        // Authenticate only (signature, app, device binding): no time check, so an expired
        // licence imports as expired and a grace one keeps its original anchors.
        let raw = self.device.device_id().map_err(|_| LicenseError::Verify)?;
        let hash = self
            .device
            .device_id_hash()
            .map_err(|_| LicenseError::Verify)?;
        let authenticated = authenticate_token_any_device(
            &token,
            &self.cfg.public_key,
            &self.cfg.app,
            &[raw.as_str(), hash.as_str()],
        )
        .map_err(|_| LicenseError::Verify)?;
        // The record is bound to whichever id the token carries.
        record.device_id = authenticated.device_id.clone();
        if record.license_id.is_none() {
            record.license_id = Some(authenticated.id.clone());
        }
        let envelope = Envelope::active(record)?;
        let guard = self.commit(guard, &envelope)?;
        Ok((self.view_after(&envelope), Some(guard)))
    }
}

/// What to tell the server after the local clear.
enum Release {
    Lease(String),
    Activation(Zeroizing<String>),
    None,
}

/// What judging a candidate token decided.
pub(crate) enum Adopted {
    /// A new verified token; the clock is already moved to its issue time.
    Replace {
        token: String,
        entitlement: Box<Entitlement>,
        lowered: bool,
    },
    /// No new token: the held one stands.
    Keep,
}

/// A lease answer, parsed but not yet verified.
pub(crate) struct LeaseWire {
    activation_token: Option<Zeroizing<String>>,
    lease: LeaseRecord,
    entitlement: Option<String>,
}

pub(crate) fn field_str(map: &Map<String, Value>, name: &str) -> Option<String> {
    map.get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Read a lease endpoint's answer: 200 with `{activationToken?, lease}`; a status maps to a
/// rejection (the server's `error` text is not forwarded).
pub(crate) fn lease_reply(reply: &Reply) -> Result<LeaseWire, LicenseError> {
    match reply.status {
        200 => {}
        400 => {
            return Err(LicenseError::Rejected {
                reason: RejectReason::Other,
            })
        }
        404 => {
            return Err(LicenseError::Rejected {
                reason: RejectReason::KeyNotAccepted,
            })
        }
        409 => {
            return Err(LicenseError::Rejected {
                reason: RejectReason::DeviceLimit,
            })
        }
        410 => {
            return Err(LicenseError::Rejected {
                reason: RejectReason::Revoked,
            })
        }
        429 => {
            return Err(LicenseError::Rejected {
                reason: RejectReason::RateLimited,
            })
        }
        _ => return Err(LicenseError::Network),
    }
    let value: Value = serde_json::from_slice(&reply.body).map_err(|_| LicenseError::Network)?;
    let top = value.as_object().ok_or(LicenseError::Network)?;
    lease_wire_from(top)
}

/// The `{activationToken?, lease}` part of a lease answer: the body of a lease issue, a renew,
/// a link redemption and a Pro-bridge exchange all carry it. A credential longer than the
/// verifier's 16 KiB token bound is refused unread.
pub(crate) fn lease_wire_from(top: &Map<String, Value>) -> Result<LeaseWire, LicenseError> {
    let lease = top
        .get("lease")
        .and_then(Value::as_object)
        .ok_or(LicenseError::Verify)?;
    let expires_text = field_str(lease, "expiresAt").ok_or(LicenseError::Verify)?;
    let record = LeaseRecord {
        lease_id: field_str(lease, "leaseId").ok_or(LicenseError::Verify)?,
        generation: lease
            .get("generation")
            .and_then(Value::as_u64)
            .ok_or(LicenseError::Verify)?,
        expires_at: parse_rfc3339(&expires_text).ok_or(LicenseError::Verify)?,
        grace_seconds: lease
            .get("graceSeconds")
            .and_then(Value::as_i64)
            .ok_or(LicenseError::Verify)?,
        device_id_hash: Some(field_str(lease, "deviceIdHash").ok_or(LicenseError::Verify)?),
        pool: Some(field_str(lease, "pool").ok_or(LicenseError::Verify)?),
        issued_at: Some(field_str(lease, "issuedAt").ok_or(LicenseError::Verify)?),
        expires_at_text: Some(expires_text),
        signature: Some(field_str(lease, "signature").ok_or(LicenseError::Verify)?),
    };
    let activation_token = field_str(top, "activationToken").map(Zeroizing::new);
    let entitlement = field_str(lease, "entitlement");
    let too_long = |text: &str| text.len() as u64 > MAX_TOKEN_BYTES;
    let activation_too_long = activation_token
        .as_ref()
        .is_some_and(|token| too_long(token.as_str()));
    let entitlement_too_long = entitlement.as_deref().is_some_and(|token| too_long(token));
    if activation_too_long || entitlement_too_long {
        return Err(LicenseError::Verify);
    }
    Ok(LeaseWire {
        activation_token,
        lease: record,
        entitlement,
    })
}

/// The record after the server said the licence is no longer active: no token of any kind.
fn revoked_record(record: &StoredRecord) -> StoredRecord {
    let mut next = record.clone();
    next.status = RecordStatus::Revoked;
    next.signed_token = None;
    next.activation_token = None;
    next.lease = None;
    next.stale_since = None;
    next
}

pub(crate) fn plausible_email(email: &str) -> bool {
    !email.is_empty()
        && email.len() <= 254
        && email.contains('@')
        && !email.starts_with('@')
        && !email.ends_with('@')
        && !email.chars().any(|c| c.is_whitespace() || c.is_control())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_suffix_is_the_last_four_characters() {
        assert_eq!(suffix("lic_abcdef").as_deref(), Some("cdef"));
        assert_eq!(suffix("ab").as_deref(), Some("ab"));
        assert_eq!(suffix(""), None);
    }

    #[test]
    fn plausible_emails_are_filtered_before_any_network() {
        assert!(plausible_email("a@b.co"));
        for bad in ["", "ab", "@b.co", "a@", "a b@c.d", "a@b.co\n"] {
            assert!(!plausible_email(bad), "{bad:?}");
        }
    }

    #[test]
    fn clock_errors_map_to_busy_or_a_fixed_clock_refusal() {
        assert_eq!(clock_error(ClockError::Contended), LicenseError::Busy);
        assert_eq!(
            clock_error(ClockError::Storage(FileClockStoreError::Io(
                std::io::Error::from(std::io::ErrorKind::WouldBlock)
            ))),
            LicenseError::Busy
        );
        assert_eq!(
            clock_error(ClockError::Missing),
            LicenseError::Clock(ClockRefusal::Missing)
        );
        assert!(matches!(
            clock_error(ClockError::ReplayedAnchor {
                last_server_iat: 5,
                got: 4
            }),
            LicenseError::Clock(ClockRefusal::Rollback { highest_seen: 5 })
        ));
        // A storage failure carries no storage text.
        let text = match clock_error(ClockError::Storage(FileClockStoreError::Io(
            std::io::Error::other("C:\\secret\\path"),
        ))) {
            LicenseError::Clock(ClockRefusal::Storage(text)) => text,
            other => panic!("{other:?}"),
        };
        assert_eq!(text, "ledger");
    }
}
