//! Requests to the supervised Windows UI Automation worker.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use super::pins::{PendingReleases, GLOBAL_LEDGER};
use super::protocol::{
    Config, FocusedElementIdentity, NativeTarget, Request, RestoreOutcome, StopTarget, TextProof,
    NEXT_HANDLE_ID,
};
use super::supervisor::{
    add_clamped, deadline_after, mint_handle_id, remaining, Abandonment, Admit, Begin, EffectGate,
    Exit, Refusal, SkipReason, Supervisor,
};
use super::worker::worker_loop;

/// Whether the handle can take work now. Public calls keep their `Option` /
/// `bool` shapes (a refused call is `None` / `false`, the same fail-closed
/// value as a timeout); this is the distinct signal for hosts that want to
/// tell "provider hung, workers exhausted" from an ordinary miss.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Availability {
    Ready,
    /// The abandoned-worker cap is reached: calls fail fast until a blocked
    /// provider call returns and its thread exits.
    Exhausted,
    /// `shutdown` ran, or the handle-id space was spent when the handle was created.
    Closed,
    /// All 2^24 - 1 worker generations of this handle have been used: no worker
    /// is running and every call is refused for good (no wrap-around).
    GenerationsSpent,
    /// The last worker could not create its UIA instance; calls fail fast until
    /// the backoff elapses, then the next call spawns a new worker.
    InitBackoff,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Health {
    pub availability: Availability,
    /// A worker exists, possibly still starting.
    pub worker_running: bool,
    /// A worker exists AND has finished initialising. A worker stuck in
    /// initialisation is `worker_running` but not `worker_ready`.
    pub worker_ready: bool,
    pub abandoned_live: usize,
    pub abandoned_total: u64,
    /// COM references the whole PROCESS holds leaked on purpose (never
    /// released; see `Config::max_forgotten_pins`). Process-wide, not per handle.
    pub leaked_com_refs: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShutdownReport {
    /// The worker left within the budget (or none was running).
    pub clean: bool,
    /// Abandoned workers still alive when `shutdown` returned.
    pub abandoned_live: usize,
}

struct State {
    sup: Supervisor,
    tx: Option<mpsc::SyncSender<Request>>,
    abandon_flag: Option<Arc<AtomicBool>>,
    /// Releases that found the worker's queue full; the worker drains them as
    /// soon as it is back in its loop.
    pending: PendingReleases,
}

/// Shared between the handle and its worker threads.
pub(super) struct Inner {
    pub(super) config: Arc<Config>,
    /// This handle's id (carried by every pin token it mints); 0 when the id
    /// space was spent and the handle was created closed.
    handle: u64,
    timeouts_reported: AtomicBool,
    state: Mutex<State>,
    changed: Condvar,
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Inner {
    fn emit(&self, event: &str, detail: &str) {
        if let Some(sink) = self.config.event_sink {
            sink(event, detail);
        }
    }

    /// Called with the state lock held, at every abandonment: the pins the
    /// generation owned are forgotten now (whether or not its thread ever
    /// returns), so they are counted now.
    fn account_abandonment(ab: &Abandonment) {
        GLOBAL_LEDGER.count_abandoned(ab.pins);
    }

    fn report_abandonment(&self, ab: Abandonment) {
        self.emit(
            "uia_worker_abandoned",
            &format!(
                "generation={} phase={} live={} total={} overdue_ms={} pins_leaked={}",
                ab.generation,
                if ab.during_init { "init" } else { "request" },
                ab.live,
                ab.total,
                ab.overdue_by.as_millis(),
                ab.pins
            ),
        );
    }

    /// Worker side: may this generation start a unit of work (request) with
    /// this deadline? Every unit has one; the supervisor records it so a hang in
    /// any of them is abandoned. A closed handle refuses it.
    pub(super) fn begin(&self, generation: u64, deadline: Instant) -> Begin {
        lock(&self.state)
            .sup
            .begin(generation, deadline, Instant::now())
    }

    /// Worker side: may this generation start a release window (release, prune,
    /// exit drain) with this deadline? Unlike `begin` it is also granted on a
    /// closed handle while the generation is still the running one.
    pub(super) fn begin_release(&self, generation: u64, deadline: Instant) -> Begin {
        lock(&self.state)
            .sup
            .begin_release(generation, deadline, Instant::now())
    }

    /// Worker side: a capture produced a pin; false when the generation is no
    /// longer live (see `Supervisor::pin_added`).
    pub(super) fn pin_added(&self, generation: u64) -> bool {
        lock(&self.state).sup.pin_added(generation)
    }

    /// Worker side: `count` pins left this generation's ownership.
    pub(super) fn pins_released(&self, generation: u64, count: usize) {
        lock(&self.state).sup.pins_released(generation, count);
    }

    /// Worker side: may this generation `Release` a COM reference (running,
    /// closed or not; false once abandoned or replaced)?
    pub(super) fn may_release(&self, generation: u64) -> bool {
        lock(&self.state).sup.may_release(generation)
    }

    /// Worker side: the worker could not create its UIA instance and is about to
    /// leave. Starts the respawn backoff and reports it.
    pub(super) fn note_init_failure(&self, generation: u64) {
        let noted = lock(&self.state)
            .sup
            .note_init_failure(generation, Instant::now());
        if let Some((attempts, delay)) = noted {
            self.emit(
                "uia_worker_init_failed",
                &format!(
                    "reason=uia_instance_unavailable attempts={attempts} retry_in_ms={}",
                    delay.as_millis()
                ),
            );
        }
    }

    /// Worker side: is this still the live generation (not abandoned,
    /// replaced or shut down)?
    pub(super) fn is_live(&self, generation: u64) -> bool {
        lock(&self.state).sup.is_live(generation)
    }

    /// Worker side, immediately before an externally visible side effect:
    /// liveness, deadline and caller handshake, decided under the lock every
    /// abandonment takes (see `Supervisor::permit_effect`).
    pub(super) fn permit_effect(
        &self,
        generation: u64,
        deadline: Instant,
        gate: &EffectGate,
    ) -> Result<(), SkipReason> {
        lock(&self.state)
            .sup
            .permit_effect(generation, deadline, Instant::now(), gate)
    }

    pub(super) fn handle_id(&self) -> u64 {
        self.handle
    }

    /// Worker side: releases that could not be queued, for this generation.
    pub(super) fn take_pending_releases(&self, generation: u64) -> Vec<u64> {
        lock(&self.state).pending.take_for(self.handle, generation)
    }

    /// Worker side: initialisation finished. False means this worker was
    /// abandoned while it was starting and must leave without serving.
    pub(super) fn worker_ready(&self, generation: u64) -> bool {
        lock(&self.state).sup.worker_ready_for(generation)
    }

    pub(super) fn finish(&self, generation: u64) {
        lock(&self.state).sup.finish(generation);
    }

    pub(super) fn worker_exited(&self, generation: u64) {
        let exit = {
            let mut state = lock(&self.state);
            let exit = state.sup.worker_exited(generation);
            if exit == Exit::Clean {
                state.tx = None;
                state.abandon_flag = None;
            }
            exit
        };
        self.changed.notify_all();
        if let Exit::Recovered { live } = exit {
            self.emit("uia_worker_recovered", &format!("live={live}"));
        }
    }

    /// Bound out-of-process provider calls made through THIS worker's own UIA
    /// instance (crate-side `use_private_instance`) with IUIAutomation2
    /// connection/transaction timeouts. Abandonment stays the hard bound; this
    /// makes a blocked worker release itself sooner so the cap recovers. Every
    /// worker sets them on its own instance (so no process-wide `Once`/`static`
    /// state exists and a replacement never waits behind another generation).
    /// The event is reported the first time, and again whenever a worker could
    /// not apply them. Runs on a worker that is `starting`.
    pub(super) fn apply_provider_timeouts(&self) {
        let config = &self.config;
        if config.provider_connection_timeout.is_zero()
            && config.provider_transaction_timeout.is_zero()
        {
            return;
        }
        let ms = |d: Duration| u32::try_from(d.as_millis()).unwrap_or(u32::MAX);
        let applied = crate::win::uia::set_provider_timeouts(
            ms(config.provider_connection_timeout),
            ms(config.provider_transaction_timeout),
        );
        let first = !self.timeouts_reported.swap(true, Ordering::AcqRel);
        if first || !applied {
            self.emit(
                "uia_provider_timeouts",
                if applied { "applied" } else { "unavailable" },
            );
        }
    }
}

/// Handle to the persistent MTA UIA worker. A worker thread starts on first
/// use. If a provider call outlives its deadline the worker is abandoned and a
/// fresh one serves later calls (see `Config::max_abandoned_workers`). Create one handle per host.
pub struct Uia {
    inner: Arc<Inner>,
}

impl Uia {
    /// `config` is clamped by `Config::validated` (no `Duration` can overflow an
    /// `Instant`, no count of zero). If the 16-bit handle-id space is spent the
    /// handle is created closed: every call fails closed.
    pub fn new(config: Config) -> Self {
        let config = config.validated();
        // A new handle may be configured with a different leak cap: let the
        // process-wide ledger report a suspension again if it applies.
        GLOBAL_LEDGER.rearm_suspension_report();
        let handle = mint_handle_id(&NEXT_HANDLE_ID);
        let mut sup = Supervisor::new(
            handle.unwrap_or(0),
            config.max_abandoned_workers,
            config.abandon_grace,
            config.worker_init_timeout,
        );
        if handle.is_none() {
            sup.close();
        }
        Self {
            inner: Arc::new(Inner {
                config: Arc::new(config),
                handle: handle.unwrap_or(0),
                timeouts_reported: AtomicBool::new(false),
                state: Mutex::new(State {
                    sup,
                    tx: None,
                    abandon_flag: None,
                    pending: PendingReleases::default(),
                }),
                changed: Condvar::new(),
            }),
        }
    }

    pub fn config(&self) -> &Config {
        &self.inner.config
    }

    /// Default deadline of one read-only request (`Config::request_timeout`).
    pub fn request_timeout(&self) -> Duration {
        self.inner.config.request_timeout
    }

    pub fn health(&self) -> Health {
        let state = lock(&self.inner.state);
        let availability = if state.sup.is_closed() {
            Availability::Closed
        } else if state.sup.generations_spent() {
            Availability::GenerationsSpent
        } else if state.sup.is_exhausted() {
            Availability::Exhausted
        } else if state.sup.in_init_backoff(Instant::now()) {
            Availability::InitBackoff
        } else {
            Availability::Ready
        };
        Health {
            availability,
            worker_running: state.sup.worker_running(),
            worker_ready: state.sup.worker_ready(),
            abandoned_live: state.sup.abandoned_live(),
            abandoned_total: state.sup.abandoned_total(),
            leaked_com_refs: GLOBAL_LEDGER.leaked(),
        }
    }

    /// Stop the worker within `budget` and refuse further work. Closing the
    /// channel tells a responsive worker to finish its request, release the pins
    /// it retains inside a deadline-bound release window (`Config::release_timeout`;
    /// `begin_release` admits it on a closed handle) and leave. A worker that has
    /// not left when the budget ends is abandoned (counted, its pins counted as
    /// leaked, reported), never joined, so this returns within `budget` (at most
    /// 60 s). Call it from the app's exit path. `Drop` uses `Uia::drop_budget`.
    pub fn shutdown(&self, budget: Duration) -> ShutdownReport {
        let inner = &self.inner;
        let mut state = lock(&inner.state);
        state.sup.close();
        // Closing the channel is the clean-close signal (the abandon flag is NOT
        // set here: only a forced abandonment below sets it, so a responsive
        // worker releases its pins instead of leaking them).
        state.tx = None;
        // A huge budget is clamped (to 60 s), never read as "no budget".
        let deadline = add_clamped(Instant::now(), budget);
        while state.sup.worker_running() {
            let left = deadline
                .map(|d| remaining(d, Instant::now()))
                .unwrap_or_default();
            if left.is_zero() {
                break;
            }
            state = inner
                .changed
                .wait_timeout(state, left)
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .0;
        }
        let clean = !state.sup.worker_running();
        let abandonment = state.sup.abandon_current();
        if let Some(ab) = &abandonment {
            Inner::account_abandonment(ab);
            if let Some(flag) = state.abandon_flag.take() {
                flag.store(true, Ordering::Release);
            }
        }
        let abandoned_live = state.sup.abandoned_live();
        drop(state);
        if let Some(ab) = abandonment {
            inner.report_abandonment(ab);
        }
        inner.emit(
            "uia_worker_shutdown",
            &format!("clean={clean} live={abandoned_live}"),
        );
        ShutdownReport {
            clean,
            abandoned_live,
        }
    }

    /// Pick (or start) the worker for one request. Abandons a worker whose
    /// in-flight request (or initialisation) is past its deadline plus grace
    /// first, so a hung provider is replaced instead of every later call
    /// queueing behind it. `pin` is the token of a retained-pin request: it is
    /// admitted only to the worker generation that minted it, under the same
    /// lock as the abandonment decision, and never causes a spawn.
    fn acquire(&self, pin: Option<u64>) -> Option<mpsc::SyncSender<Request>> {
        let inner = &self.inner;
        let mut abandonment = None;
        let mut refusal = None;
        let acquired = {
            let mut state = lock(&inner.state);
            if let Some(ab) = state.sup.reap_overrun(Instant::now()) {
                Inner::account_abandonment(&ab);
                abandonment = Some(ab);
                state.tx = None;
                if let Some(flag) = state.abandon_flag.take() {
                    flag.store(true, Ordering::Release);
                }
            }
            match state.sup.admit(Instant::now(), pin) {
                Admit::Use(_) => state.tx.clone(),
                Admit::Spawn(generation) => {
                    let (tx, rx) = mpsc::sync_channel(1);
                    let flag = Arc::new(AtomicBool::new(false));
                    let worker_inner = Arc::clone(inner);
                    let worker_flag = Arc::clone(&flag);
                    let spawned = std::thread::Builder::new()
                        .name("windows-uia-mta".to_string())
                        .spawn(move || worker_loop(rx, worker_inner, generation, worker_flag));
                    match spawned {
                        Ok(_) => {
                            state.tx = Some(tx.clone());
                            state.abandon_flag = Some(flag);
                            Some(tx)
                        }
                        Err(_) => {
                            state.sup.spawn_failed(generation);
                            None
                        }
                    }
                }
                Admit::Refuse(reason) => {
                    refusal = Some(reason);
                    None
                }
            }
        };
        if let Some(ab) = abandonment {
            inner.report_abandonment(ab);
        }
        if let Some(Refusal::Exhausted { first: true }) = refusal {
            let health = self.health();
            inner.emit(
                "uia_worker_exhausted",
                &format!(
                    "live={} total={}",
                    health.abandoned_live, health.abandoned_total
                ),
            );
        }
        acquired
    }

    /// A caller's deadline passed. Abandon the worker only when its in-flight
    /// request is itself overdue (past grace); a request that merely waited in
    /// the queue behind healthy work leaves the worker alone.
    fn note_timeout(&self) {
        let abandonment = {
            let mut state = lock(&self.inner.state);
            let ab = state.sup.reap_overrun(Instant::now());
            if let Some(ab) = &ab {
                Inner::account_abandonment(ab);
                state.tx = None;
                if let Some(flag) = state.abandon_flag.take() {
                    flag.store(true, Ordering::Release);
                }
            }
            ab
        };
        if let Some(ab) = abandonment {
            self.inner.report_abandonment(ab);
        }
    }

    fn request<T>(
        &self,
        timeout: Duration,
        build: impl FnOnce(Instant, mpsc::SyncSender<T>) -> Request,
    ) -> Option<T> {
        self.request_pinned(None, timeout, build)
    }

    /// `pin` is the retained-pin token the request names, if any; a token from
    /// another worker generation returns `None` here without being sent.
    fn request_pinned<T>(
        &self,
        pin: Option<u64>,
        timeout: Duration,
        build: impl FnOnce(Instant, mpsc::SyncSender<T>) -> Request,
    ) -> Option<T> {
        let deadline = deadline_after(Instant::now(), timeout)?;
        let (reply, receive) = mpsc::sync_channel(1);
        let request = build(deadline, reply);
        // A refused, full or closed queue is the same fail-closed `None` as a
        // timeout; the caller never learns a proof it did not get.
        let sender = self.acquire(pin)?;
        sender.try_send(request).ok()?;
        match receive.recv_timeout(remaining(deadline, Instant::now())) {
            Ok(value) => Some(value),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.note_timeout();
                None
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => None,
        }
    }

    pub fn focused_text_input(&self) -> Option<bool> {
        self.request(self.inner.config.request_timeout, |deadline, reply| {
            Request::Editable { deadline, reply }
        })
        .flatten()
    }

    pub fn focused_field_is_password(&self) -> Option<bool> {
        self.request(self.inner.config.request_timeout, |deadline, reply| {
            Request::Password { deadline, reply }
        })
        .flatten()
    }

    pub fn focused_element_identity(
        &self,
        target: &NativeTarget,
    ) -> Option<FocusedElementIdentity> {
        let process_id = target.process_id? as i32;
        let window_handle = target.window_handle?;
        self.request(self.inner.config.request_timeout, |deadline, reply| {
            Request::Identity {
                deadline,
                process_id,
                control_handle: target.focused_control_handle.unwrap_or_default(),
                window_handle,
                reply,
            }
        })
        .flatten()
    }

    pub fn focused_text_with_identity(
        &self,
        target: &NativeTarget,
        expected: &FocusedElementIdentity,
        timeout: Duration,
    ) -> Option<TextProof> {
        self.request(timeout, |deadline, reply| Request::ReadText {
            deadline,
            target: *target,
            expected: expected.clone(),
            reply,
        })
        .flatten()
    }

    pub fn stop_target_text<T>(
        &self,
        stop: &StopTarget<T>,
        timeout: Duration,
    ) -> Option<TextProof> {
        self.request_pinned(Some(stop.element_token), timeout, |deadline, reply| {
            Request::ReadStopText {
                deadline,
                stop: stop.stop_ref(),
                reply,
            }
        })
        .flatten()
    }

    /// Prove the focused editor on the worker and retain its element. The
    /// host's own snapshot rides along unchanged in `StopTarget::target`;
    /// `apply_editable` is called once on success with the proven
    /// editable state so the host can record it on its snapshot.
    pub fn capture_stop_target<T>(
        &self,
        native: NativeTarget,
        target: T,
        apply_editable: impl FnOnce(&mut T, Option<bool>),
    ) -> Option<StopTarget<T>> {
        let captured = self
            .request(self.inner.config.request_timeout, |deadline, reply| {
                Request::CaptureStopTarget {
                    deadline,
                    target: native,
                    reply,
                }
            })
            .flatten()?;
        let mut target = target;
        apply_editable(&mut target, captured.focused_text_input);
        Some(StopTarget {
            target,
            window_handle: captured.window_handle,
            process_creation_time: captured.process_creation_time,
            identity: captured.identity,
            focused_text_input: captured.focused_text_input,
            is_password: captured.is_password,
            element_token: captured.element_token,
        })
    }

    pub fn validate_stop_target<T>(&self, stop: &StopTarget<T>) -> bool {
        self.request_pinned(
            Some(stop.element_token),
            self.inner.config.request_timeout,
            |deadline, reply| Request::ValidateStopTarget {
                deadline,
                stop: stop.stop_ref(),
                reply,
            },
        )
        .unwrap_or(false)
    }

    pub fn stop_target_is_gone<T>(&self, stop: &StopTarget<T>) -> Option<bool> {
        self.request_pinned(
            Some(stop.element_token),
            self.inner.config.request_timeout,
            |deadline, reply| Request::StopTargetIsGone {
                deadline,
                stop: stop.stop_ref(),
                reply,
            },
        )
        .flatten()
    }

    /// Restore focus, reporting whether the effect was issued. `NotIssued`
    /// means nothing changed; `IssuedUnconfirmed` means the Focus call was made
    /// (or may still be running on an abandoned worker) without a verified
    /// restore, so the user's focus may have moved. Decided by a one-shot
    /// handshake with the worker (`EffectGate`), so a caller that stops waiting
    /// can tell the two apart without racing the worker.
    pub fn restore_stop_target_outcome<T>(&self, stop: &StopTarget<T>) -> RestoreOutcome {
        let gate = EffectGate::new();
        let reply = self.request_pinned(
            Some(stop.element_token),
            self.inner.config.request_timeout,
            |deadline, reply| Request::RestoreStopTarget {
                deadline,
                stop: stop.stop_ref(),
                gate: gate.clone(),
                reply,
            },
        );
        match reply {
            Some(outcome) => outcome,
            // No answer (refused, never queued, expired, timed out, worker
            // gone): cancel the effect. If the worker had not claimed it, it
            // never will; if it had, the Focus call was issued.
            None if gate.cancel() => RestoreOutcome::NotIssued,
            None => {
                self.inner.emit(
                    "uia_effect_issued_unconfirmed",
                    "kind=focus reason=no_reply",
                );
                RestoreOutcome::IssuedUnconfirmed
            }
        }
    }

    /// UIA counterpart of macOS web-area binding: true only when the retained,
    /// non-secure, editable stop element sits under a Document ancestor whose
    /// native window is the control named by the hotkey proof.
    pub fn stop_target_names_web_container<T>(
        &self,
        stop: &StopTarget<T>,
        hotkey_control: isize,
        window_handle: isize,
    ) -> bool {
        self.request_pinned(
            Some(stop.element_token),
            self.inner.config.request_timeout,
            |deadline, reply| Request::WebContainerNamed {
                deadline,
                stop: stop.stop_ref(),
                hotkey_control,
                window_handle,
                reply,
            },
        )
        .unwrap_or(false)
    }

    /// Drop a retained element. Never starts a worker and never blocks: a pin
    /// held by an abandoned or exited worker is already gone with it. If the
    /// worker's queue is full the token is parked (bounded) and the worker drops
    /// the pin as soon as it is back in its loop, not at the TTL. The drop runs
    /// in a supervised window (`Config::release_timeout`), so a COM `Release`
    /// that hangs in the provider is abandoned like any other hung request.
    /// A token from another handle or generation is a no-op.
    pub fn release_stop_target(&self, token: u64) {
        if token == 0 {
            return;
        }
        let Some(deadline) = deadline_after(Instant::now(), self.inner.config.release_budget())
        else {
            return;
        };
        // The send is non-blocking, so it is made under the lock the worker
        // takes to drain `pending`: either the worker sees the parked token, or
        // the queue was not full.
        let mut state = lock(&self.inner.state);
        if !state.sup.pin_is_current(token) {
            return;
        }
        let Some(sender) = state.tx.clone() else {
            return;
        };
        if let Err(mpsc::TrySendError::Full(_)) =
            sender.try_send(Request::ReleaseStopTarget { deadline, token })
        {
            let cap = self.inner.config.max_retained_stop_targets;
            state.pending.push(token, cap);
        }
    }
}

impl Uia {
    /// How long `Drop` waits for the worker to leave: the longest a responsive
    /// worker can still be busy (one request, then one release window, each with
    /// its abandon grace). Returns at once when the worker is idle; blocks for up
    /// to this long (60 s at most) only while a provider is actually hanging.
    fn drop_budget(&self) -> Duration {
        let config = &self.inner.config;
        config
            .request_timeout
            .saturating_add(config.abandon_grace)
            .saturating_add(config.release_budget())
            .saturating_add(config.abandon_grace)
    }
}

impl Drop for Uia {
    fn drop(&mut self) {
        self.shutdown(self.drop_budget());
    }
}
