//! Event-driven sync coordinator: wake coalescing, bounded retry with an engine-supplied floor,
//! parking until a real wake, and the consent epoch. It owns wake and coalescing only; the pass
//! itself (pull, merge, push, tombstones, backoff) is the engine's, reached through
//! [`TranscriptSync::pass`].
//!
//! One coordinator task serializes passes. Two things can outlive a caller: the coordinator task
//! (stopped by [`Scheduler::stop`], which makes it return at its next wake) and one retry timer
//! (it checks the stop flag when it fires and exits).

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use super::pending::PendingWork;
use super::sync::{PassTurn, TranscriptSync};
use super::traits::Spawner;
use crate::transcripts as sync_core;
use crate::PassOutcome;

pub(crate) const PUSH: u8 = 1;
pub(crate) const PULL: u8 = 2;

/// No wait the engine or the host asks for is longer than this.
const MAX_WAIT_SECS: f64 = 86_400.0;

/// The scheduler's timings. There is no default: the host chooses its values. Out-of-range values
/// are clamped on construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SchedulerPolicy {
    retry_interval: Duration,
    pending_retry_interval: Duration,
    max_attempts: u8,
}

impl SchedulerPolicy {
    /// `retry_secs`: delay before retrying a failed pass when the engine gave none, clamped to
    /// 5..=3600. `pending_retry_secs`: how long work that exhausted its retries stays parked
    /// before one more attempt, clamped to 5..=86400. `max_attempts`: retries of one failure
    /// streak before parking, clamped to 1..=10.
    pub fn new(retry_secs: u64, pending_retry_secs: u64, max_attempts: u8) -> Self {
        Self {
            retry_interval: Duration::from_secs(retry_secs.clamp(5, 3_600)),
            pending_retry_interval: Duration::from_secs(pending_retry_secs.clamp(5, 86_400)),
            max_attempts: max_attempts.clamp(1, 10),
        }
    }
}

pub(crate) struct Scheduler {
    work: PendingWork,
    notify: tokio::sync::Notify,
    enabled: AtomicBool,
    started: AtomicBool,
    stopped: AtomicBool,
    timer_armed: AtomicBool,
    consent_generation: AtomicU64,
    /// Work that exhausted bounded retries stays parked until a real wake
    /// (native event, local edit, explicit reconcile) or the single retry timer.
    parked_work: AtomicU8,
    policy: SchedulerPolicy,
    spawn: Spawner,
}

/// The instance a native callback is routed to. A native observer has one process-wide callback
/// slot, so only the most recently started instance receives events; an instance that was
/// dropped cannot be reached (the `Weak` no longer upgrades).
static NATIVE_TARGET: Mutex<Option<Weak<TranscriptSync>>> = Mutex::new(None);

/// The callback handed to [`super::NativeObserver::install`]. May run on any native thread.
pub(crate) extern "C" fn native_event(kind: i32) {
    let target = match NATIVE_TARGET.lock() {
        Ok(guard) => guard.as_ref().and_then(Weak::upgrade),
        Err(_) => None,
    };
    let Some(core) = target else {
        return;
    };
    match kind {
        1 => core.notify_remote_change(),
        2 => core.notify_reconcile(),
        _ => core.emit("cloud_sync_native_event_ignored", &format!("kind={kind}")),
    }
}

fn clamp_wait(secs: Option<f64>, fallback: Duration) -> Duration {
    match secs {
        Some(s) if s.is_finite() && s > 0.0 => Duration::from_secs_f64(s.min(MAX_WAIT_SECS)),
        _ => fallback.min(Duration::from_secs_f64(MAX_WAIT_SECS)),
    }
}

impl Scheduler {
    pub(crate) fn new(policy: SchedulerPolicy, spawn: Spawner) -> Self {
        Self {
            work: PendingWork::default(),
            notify: tokio::sync::Notify::new(),
            enabled: AtomicBool::new(false),
            started: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            timer_armed: AtomicBool::new(false),
            consent_generation: AtomicU64::new(0),
            parked_work: AtomicU8::new(0),
            policy,
            spawn,
        }
    }

    pub(crate) fn request(&self, bits: u8) {
        // Set work before clearing its parked marker: either ordering leaves
        // a wake durable when a request races retry exhaustion.
        self.work.request(bits);
        self.parked_work.fetch_and(!bits, Ordering::AcqRel);
        self.notify.notify_one();
    }

    pub(crate) fn wake(&self) {
        self.notify.notify_one();
    }

    pub(crate) fn disable(&self) {
        self.enabled.store(false, Ordering::Release);
        self.consent_generation.fetch_add(1, Ordering::AcqRel);
        self.work.clear();
        self.parked_work.store(0, Ordering::Release);
        self.notify.notify_waiters();
    }

    pub(crate) fn enable(&self) {
        self.enabled.store(true, Ordering::Release);
        self.consent_generation.fetch_add(1, Ordering::AcqRel);
        self.request(PUSH | PULL);
    }

    /// Stop the coordinator task and the retry timer. Idempotent. Does not wait for a pass that
    /// is already running; that pass is cancelled by the transport's generation (the caller
    /// disables the transport) and its result is dropped as stale.
    pub(crate) fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
        self.enabled.store(false, Ordering::Release);
        self.consent_generation.fetch_add(1, Ordering::AcqRel);
        self.work.clear();
        self.parked_work.store(0, Ordering::Release);
        self.notify.notify_waiters();
        self.notify.notify_one();
    }

    pub(crate) fn consent_generation(&self) -> u64 {
        self.consent_generation.load(Ordering::Acquire)
    }

    fn park(self: &Arc<Self>, bits: u8) {
        self.parked_work.fetch_or(bits, Ordering::AcqRel);
        // One timer at a time. It keeps durable pending work alive while the app stays open,
        // but only for directions still known failed: a successful external wake clears the
        // marker, so the timer finds nothing to do.
        if self.stopped.load(Ordering::Acquire) || self.timer_armed.swap(true, Ordering::AcqRel) {
            return;
        }
        let scheduler = Arc::clone(self);
        let delay = self.policy.pending_retry_interval;
        (self.spawn)(Box::pin(async move {
            tokio::time::sleep(delay).await;
            scheduler.timer_armed.store(false, Ordering::Release);
            let parked = scheduler.parked_work.swap(0, Ordering::AcqRel);
            if parked != 0
                && scheduler.enabled.load(Ordering::Acquire)
                && !scheduler.stopped.load(Ordering::Acquire)
            {
                scheduler.work.request(parked);
                scheduler.notify.notify_one();
            }
        }));
    }

    pub(crate) fn start(self: &Arc<Self>, core: &Arc<TranscriptSync>) {
        if self.started.swap(true, Ordering::AcqRel) {
            self.request(PUSH | PULL);
            return;
        }
        self.enabled.store(core.sync_enabled(), Ordering::Release);
        // The native bridge owns subscription, account and network observation in the main
        // process. A transport hands it out only when this session may use it.
        if let Some(native) = core.native_observer() {
            if let Ok(mut target) = NATIVE_TARGET.lock() {
                *target = Some(Arc::downgrade(core));
            }
            native.install(native_event);
            native.set_enabled(self.enabled.load(Ordering::Acquire));
        }
        self.request(PUSH | PULL); // startup reconcile
        let scheduler = Arc::clone(self);
        let core = Arc::clone(core);
        (self.spawn)(Box::pin(async move {
            scheduler.run(core).await;
        }));
    }

    async fn run(self: Arc<Self>, core: Arc<TranscriptSync>) {
        let mut retry_until: Option<tokio::time::Instant> = None;
        let mut attempts = 0u8;
        let mut seen_generation = self.work.generation();
        loop {
            let bits = loop {
                if self.stopped.load(Ordering::Acquire) {
                    return;
                }
                if !self.enabled.load(Ordering::Acquire) {
                    self.work.clear();
                    retry_until = None;
                }
                let notified = self.notify.notified();
                let bits = self.work.take();
                if bits != 0 && self.enabled.load(Ordering::Acquire) {
                    break bits;
                }
                notified.await;
            };
            let generation = self.work.generation();
            if generation != seen_generation {
                attempts = 0;
                seen_generation = generation;
            }
            let now = tokio::time::Instant::now();
            if retry_until.is_some_and(|deadline| deadline > now) {
                self.work.requeue(bits);
                self.wait_for_retry_until(retry_until).await;
                continue;
            }
            retry_until = None;

            // Admission. Every pass this loop runs is automatic (scheduled, retry, remote-event
            // wake, local-change wake, status wake). While the host's shared monotonic
            // `not_before` is in the future none is admitted: the work stays queued and this loop
            // re-arms for the remaining time. A wake only re-checks admission, so a remote
            // change cannot end a pause early. The instant is the host's; a pass starting never
            // clears it, and explicit user actions (`TranscriptSync::reset_feed_watch` paths)
            // clear it and wake this loop. The pass checks it again under the sync lock
            // (`pass_automatic`), because a pass that ran while this turn waited for the lock may
            // have set it after this check.
            if let Some(remaining) = core.admission_wait() {
                self.work.requeue(bits);
                let _ = tokio::time::timeout(remaining, self.notify.notified()).await;
                continue;
            }

            // The host's activity gate owns the history boundary. The host wakes us when it
            // ends, so deferring cannot become polling.
            if core.busy_reason().is_some() {
                self.work.requeue(bits);
                self.notify.notified().await;
                continue;
            }

            match core.pass_automatic().await {
                // Admission closed while this turn waited for the sync lock (a pass that ran in
                // between set `not_before`). The pass re-checked under the lock and did no
                // engine work: the work stays queued and this loop waits the remaining time, the
                // same as the check above. A wake only re-checks admission.
                Ok(PassTurn::NotBefore(remaining)) => {
                    self.work.requeue(bits);
                    let _ = tokio::time::timeout(remaining, self.notify.notified()).await;
                }
                Ok(PassTurn::Done(None)) => {
                    if self.enabled.load(Ordering::Acquire) && core.busy_reason().is_some() {
                        self.work.requeue(bits);
                    }
                }
                Ok(PassTurn::Done(Some(report))) => {
                    if report.downloaded > 0 {
                        core.notify_downloaded(
                            u32::try_from(report.downloaded).unwrap_or(u32::MAX),
                        );
                    }
                    let wait = report
                        .retry_after_secs
                        .filter(|s| s.is_finite() && *s > 0.0);
                    // A pass that could not read the feed to its end (halt) or hit the pass budget
                    // has set the host's `not_before`; the admission check above holds the next
                    // automatic pass back. Nothing is taken or cleared here.
                    match &report.outcome {
                        PassOutcome::Synced | PassOutcome::Disabled => {
                            attempts = 0;
                            self.parked_work.fetch_and(!(PUSH | PULL), Ordering::AcqRel);
                        }
                        PassOutcome::Pending(_)
                        | PassOutcome::Deferred { .. }
                        | PassOutcome::Cancelled => {
                            // Token advanced (or backoff open) with queued work: come back after
                            // the engine's floor; the host's `not_before` still gates admission.
                            self.work.requeue(bits);
                            let delay =
                                wait.map(|secs| clamp_wait(Some(secs), self.policy.retry_interval));
                            if let Some(delay) = delay {
                                let now = tokio::time::Instant::now();
                                retry_until = Some(now.checked_add(delay).unwrap_or(now));
                            }
                        }
                        // The consent pause is not enabled by this host (the host's own account
                        // gate runs first); park like Unavailable rather than loop.
                        PassOutcome::ConsentRequired(_) => {
                            self.park(PUSH | PULL);
                        }
                        PassOutcome::Unavailable(detail) => {
                            // Signed out, restricted, entitlement missing: wait for an
                            // external wake (account or reachability event).
                            self.park(PUSH | PULL);
                            core.emit(
                                "cloud_sync_unavailable",
                                &sync_core::error_is_terminal(detail).to_string(),
                            );
                        }
                        PassOutcome::Failed(detail) => {
                            attempts = self.retry_or_park(
                                &core,
                                bits,
                                attempts,
                                detail,
                                wait,
                                &mut retry_until,
                            );
                        }
                    }
                }
                Err(err) => {
                    attempts = self.retry_or_park(
                        &core,
                        bits,
                        attempts,
                        &err,
                        Some(self.policy.retry_interval.as_secs_f64()),
                        &mut retry_until,
                    );
                }
            }
        }
    }

    /// Bounded retry: up to `max_attempts` attempts at the engine's delay (floored by the
    /// permanent-schema window), then park until a real wake.
    fn retry_or_park(
        self: &Arc<Self>,
        core: &TranscriptSync,
        bits: u8,
        attempts: u8,
        detail: &str,
        wait_secs: Option<f64>,
        retry_until: &mut Option<tokio::time::Instant>,
    ) -> u8 {
        core.emit("cloud_sync_pass_failed", detail);
        if !self.enabled.load(Ordering::Acquire) {
            return attempts;
        }
        if sync_core::error_is_terminal(detail) {
            self.park(bits | PUSH | PULL);
            return attempts;
        }
        if attempts < self.policy.max_attempts {
            self.work.requeue(bits);
            let mut delay = clamp_wait(wait_secs, self.policy.retry_interval);
            if sync_core::is_production_schema_error(detail) {
                delay = delay.max(sync_core::PERMANENT_SCHEMA_FAILURE_BACKOFF);
            }
            let now = tokio::time::Instant::now();
            *retry_until = Some(now.checked_add(delay).unwrap_or(now));
            attempts.saturating_add(1)
        } else {
            self.park(PUSH | PULL);
            core.emit("cloud_sync_pass_retry_exhausted", detail);
            attempts
        }
    }

    async fn wait_for_retry_until(&self, deadline: Option<tokio::time::Instant>) {
        let Some(deadline) = deadline else {
            self.notify.notified().await;
            return;
        };
        if !self.enabled.load(Ordering::Acquire) {
            return;
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return;
        }
        let _ = tokio::time::timeout(remaining, self.notify.notified()).await;
    }
}
