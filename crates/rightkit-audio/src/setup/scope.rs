//! Host-owned setup audio admission & teardown, independent of any window/runtime framework.
//!
//! Construct with explicit initial activity (including headless hosts). Capture a [`SetupAudioTicket`](crate::setup::scope::SetupAudioTicket)
//! for legacy meter commands or consent, or [`SetupAudioMeterTicket`] with `meter_ticket` for
//! ordered meter starts, when accepting each command BEFORE dispatching blocking work. Call
//! `stop_meter_now` immediately on the same serialized command ingress, never on a blocking
//! executor. Reusing a pre-hide ticket after reveal is rejected. A newer meter ticket or stop invalidates queued meter starts without
//! revoking consent. `activate` records visibility even while teardown is draining.
//! `suspend` cancels meter & consent immediately. Dispatch `drain` on an existing blocking executor;
//! after it returns, new-generation tickets can run without calling `activate` again. A command
//! receiving `Draining` may drain off UI, then retry its SAME ticket; no implicit resume occurs.
//!
//! Renderer meter-stop calls `stop_meter_now`, preserving pending native consent.
//! Invoke hide/close synchronously too; defer only `drain`, never a second cancellation. Window hide or
//! suspend calls `suspend`; reveal calls `activate`; ordinary exit calls `close` then dispatches
//! `drain` off UI. Concurrent drains serialize, so exit waits for an earlier hide drain. No thread
//! or task is spawned by this scope, and there is one permission owner, active OR retiring.
//!
//! Host callbacks run unlocked. They may call nonblocking `close` or `suspend`, but MUST NOT call
//! blocking `drain` (or wait for another thread draining this scope): callbacks must unwind before
//! their own capture/consent can drain. Keep final ownership until an off-UI drain returns. As with
//! primitive owners, callbacks retaining this scope must use `Weak` to avoid ownership cycles.

use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};

use super::mic_meter::{MeterConfig, MeterEvent, MeterStartError, MicMeter};
use super::permissions::{MicrophonePermissionSession, PermissionHost};
use crate::capture::DeviceInfo;

/// Admission identity captured before a command is queued. Clones own no audio resources.
#[derive(Clone)]
pub struct SetupAudioTicket {
    generation: Weak<()>,
    receipt: u64,
}

/// Start admission captured with `SetupAudioScope::meter_ticket` before dispatch.
/// Newer starts & meter stops invalidate this token independently of consent.
/// Clones share one start admission. After admission, including failure, they are stale;
/// a `Draining` refusal does not consume admission.
#[derive(Clone)]
pub struct SetupAudioMeterTicket {
    scope: SetupAudioTicket,
    generation: Weak<()>,
    epoch: u64,
}

/// Explicit caller-selected activity; construction opens no device & starts no worker.
#[derive(Debug, Clone, Copy)]
pub struct SetupAudioConfig {
    pub initially_active: bool,
    pub event_sink: fn(&str, &str),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetupAudioError {
    Closed,
    Inactive,
    StaleTicket,
    /// Visibility is remembered, but a suspended predecessor still needs `drain`.
    Draining,
    /// A legacy meter start or permission request is already admitted; no call is queued.
    Busy,
    Meter(MeterStartError),
    Permission(String),
}

struct State {
    active: bool,
    closed: bool,
    generation: Arc<()>,
    meter_generation: Arc<()>,
    next_receipt: u64,
    last_meter_receipt: u64,
    needs_drain: bool,
    draining: bool,
    meter_owner: Option<std::thread::ThreadId>,
    meter_started: bool,
    #[cfg(test)]
    meter_waiters: usize,
    permission_busy: bool,
    permission: Option<Arc<MicrophonePermissionSession>>,
}

/// One setup audio lifecycle. `close` is terminal cancellation; `drain` proves release.
/// Neither visibility transitions nor permission requests allocate a teardown worker.
pub struct SetupAudioScope {
    state: Mutex<State>,
    settled: Condvar,
    meter: MicMeter,
}

impl SetupAudioScope {
    pub fn new(config: SetupAudioConfig) -> Self {
        Self {
            state: Mutex::new(State {
                active: config.initially_active,
                closed: false,
                generation: Arc::new(()),
                meter_generation: Arc::new(()),
                next_receipt: 0,
                last_meter_receipt: 0,
                needs_drain: false,
                draining: false,
                meter_owner: None,
                meter_started: false,
                #[cfg(test)]
                meter_waiters: 0,
                permission_busy: false,
                permission: None,
            }),
            settled: Condvar::new(),
            meter: MicMeter::new(config.event_sink),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Capture legacy meter/consent admission on command receipt, before dispatch.
    /// Ordered starts use `meter_ticket`; immediate stop uses `stop_meter_now`.
    /// Consent ignores receipt order. Allowed while visible teardown drains.
    pub fn ticket(&self) -> Result<SetupAudioTicket, SetupAudioError> {
        let mut state = self.state();
        if state.closed {
            return Err(SetupAudioError::Closed);
        }
        if !state.active {
            return Err(SetupAudioError::Inactive);
        }
        let receipt = Self::next_receipt(&mut state);
        Ok(SetupAudioTicket {
            generation: Arc::downgrade(&state.generation),
            receipt,
        })
    }

    /// Reserve a meter epoch on receipt of an explicit start/device-change command,
    /// before scheduling its blocking worker. Cancels an earlier queued or admitted meter
    /// start immediately, while leaving consent admission & its native owner untouched.
    /// A visible scope may reserve while teardown drains; use the SAME token after drain.
    pub fn meter_ticket(&self) -> Result<SetupAudioMeterTicket, SetupAudioError> {
        let mut state = self.state();
        if state.closed {
            return Err(SetupAudioError::Closed);
        }
        if !state.active {
            return Err(SetupAudioError::Inactive);
        }
        let receipt = Self::next_receipt(&mut state);
        Ok(self.reserve_meter(&mut state, receipt))
    }

    fn next_receipt(state: &mut State) -> u64 {
        // Never wrap receipt order: exhaustion cannot silently revive an obsolete command.
        state.next_receipt = state
            .next_receipt
            .checked_add(1)
            .expect("setup audio receipt exhausted");
        state.next_receipt
    }

    fn reserve_meter(&self, state: &mut State, receipt: u64) -> SetupAudioMeterTicket {
        state.last_meter_receipt = receipt;
        state.meter_generation = Arc::new(());
        state.meter_started = false;
        let epoch = self.meter.begin_scoped();
        self.settled.notify_all();
        SetupAudioMeterTicket {
            scope: SetupAudioTicket {
                generation: Arc::downgrade(&state.generation),
                receipt,
            },
            generation: Arc::downgrade(&state.meter_generation),
            epoch,
        }
    }

    fn validate_legacy_meter(
        state: &State,
        ticket: &SetupAudioTicket,
    ) -> Result<(), SetupAudioError> {
        Self::validate(state, ticket)?;
        if ticket.receipt <= state.last_meter_receipt {
            return Err(SetupAudioError::StaleTicket);
        }
        Ok(())
    }

    fn validate_meter(
        state: &State,
        ticket: &SetupAudioMeterTicket,
    ) -> Result<(), SetupAudioError> {
        Self::validate(state, &ticket.scope)?;
        if !Weak::ptr_eq(&ticket.generation, &Arc::downgrade(&state.meter_generation)) {
            return Err(SetupAudioError::StaleTicket);
        }
        Ok(())
    }

    fn validate(state: &State, ticket: &SetupAudioTicket) -> Result<(), SetupAudioError> {
        if state.closed {
            return Err(SetupAudioError::Closed);
        }
        if !Weak::ptr_eq(&ticket.generation, &Arc::downgrade(&state.generation)) {
            return Err(SetupAudioError::StaleTicket);
        }
        if !state.active {
            return Err(SetupAudioError::Inactive);
        }
        Ok(())
    }

    fn admit(state: &State, ticket: &SetupAudioTicket) -> Result<(), SetupAudioError> {
        Self::validate(state, ticket)?;
        if state.needs_drain || state.draining {
            return Err(SetupAudioError::Draining);
        }
        Ok(())
    }

    /// Remember explicit reveal/resume. A `Draining` result still records activity: completion
    /// of `drain` opens admission automatically for this generation, without another reveal.
    pub fn activate(&self) -> Result<(), SetupAudioError> {
        let mut state = self.state();
        if state.closed {
            return Err(SetupAudioError::Closed);
        }
        state.active = true;
        if state.needs_drain || state.draining {
            return Err(SetupAudioError::Draining);
        }
        Ok(())
    }

    /// Nonblocking hide/suspend, invoked synchronously at native lifecycle ingress.
    /// Already-admitted callbacks or device calls may unwind;
    /// a dispatched `drain` waits for them. New audio remains refused until explicit activate.
    pub fn suspend(&self) {
        self.retire(false);
    }

    /// Terminal, nonblocking & idempotent, including inside host callbacks. Dispatch `drain`
    /// off UI for ordinary exit; repeated exit drains all return only after teardown settles.
    pub fn close(&self) {
        self.retire(true);
    }

    fn retire(&self, terminal: bool) {
        let permission = {
            let mut state = self.state();
            if state.closed || (!terminal && !state.active) {
                return;
            }
            state.active = false;
            state.closed = terminal;
            state.generation = Arc::new(());
            state.needs_drain = true;
            if terminal {
                self.meter.close_scoped();
            } else {
                self.meter.stop_scoped();
            }
            self.settled.notify_all();
            state.permission.as_ref().map(Arc::clone)
        };
        // Session cancellation may release host payloads; never hold scope state here.
        if let Some(permission) = permission {
            permission.cancel();
        }
    }

    /// Legacy queued stop: capture a fresh `ticket` on command receipt. A later-received
    /// admitted meter action makes this stop stale; a later stop can cancel an earlier start
    /// even when both tickets were captured before either worker ran.
    /// Preserves permission prompt & grant watch. Prefer `stop_meter_now` on command ingress.
    pub fn stop_meter(&self, ticket: &SetupAudioTicket) -> Result<(), SetupAudioError> {
        let mut state = self.state();
        Self::validate_legacy_meter(&state, ticket)?;
        self.stop_meter_locked(&mut state, ticket.receipt);
        Ok(())
    }

    /// Immediate nonblocking meter stop, preserving consent. Call on the same serialized
    /// command ingress as `meter_ticket`, never dispatch this call to a blocking executor.
    /// Validation & cancellation form one receipt operation; no stop token can be delayed.
    pub fn stop_meter_now(&self) -> Result<(), SetupAudioError> {
        let mut state = self.state();
        if state.closed {
            return Err(SetupAudioError::Closed);
        }
        if !state.active {
            return Err(SetupAudioError::Inactive);
        }
        let receipt = Self::next_receipt(&mut state);
        self.stop_meter_locked(&mut state, receipt);
        Ok(())
    }

    fn stop_meter_locked(&self, state: &mut State, receipt: u64) {
        state.last_meter_receipt = receipt;
        state.meter_generation = Arc::new(());
        self.meter.stop_scoped();
        self.settled.notify_all();
    }

    /// Legacy blocking start with a fresh command-receipt `ticket`. Rejects an older or used
    /// receipt before hardware access; an active predecessor returns `Busy`. Consent remains
    /// independent. For ordered supersession use `meter_ticket` & `start_reserved_meter`.
    /// Caller verifies an existing OS microphone grant & explicit user action.
    pub fn start_meter(
        &self,
        ticket: &SetupAudioTicket,
        config: MeterConfig,
        resolve_device: impl FnOnce(&[DeviceInfo]) -> Result<Option<usize>, String>,
        emit: Arc<dyn Fn(MeterEvent) + Send + Sync>,
    ) -> Result<(), SetupAudioError> {
        let reserved = self.admit_legacy_meter_start(ticket)?;
        self.start_admitted_meter(&reserved, config, resolve_device, emit)
    }

    fn admit_legacy_meter_start(
        &self,
        ticket: &SetupAudioTicket,
    ) -> Result<SetupAudioMeterTicket, SetupAudioError> {
        if self.meter.is_worker_scoped() {
            return Err(SetupAudioError::Meter(MeterStartError::ReentrantStart));
        }
        let mut state = self.state();
        Self::validate_legacy_meter(&state, ticket)?;
        Self::admit(&state, ticket)?;
        if state.meter_owner.is_some() {
            return Err(SetupAudioError::Busy);
        }
        let reserved = self.reserve_meter(&mut state, ticket.receipt);
        state.meter_owner = Some(std::thread::current().id());
        state.meter_started = true;
        Ok(reserved)
    }

    /// Blocking. Caller verifies an existing OS grant & explicit user action. At most one
    /// start/resolver is admitted. The newest valid caller waits for an admitted predecessor,
    /// then `MicMeter` drains old capture before opening the new device. A newer ticket, stop
    /// or hide wakes waiters & rejects superseded work. No timeout can bound an OS/device call.
    /// Requires `meter_ticket` captured BEFORE dispatch, never a generic consent ticket.
    /// A token admits at most one start, including failures & clones. `Draining` leaves it
    /// unconsumed, so it may be retried after `drain`; no host retry is needed for overlap.
    pub fn start_reserved_meter(
        &self,
        ticket: &SetupAudioMeterTicket,
        config: MeterConfig,
        resolve_device: impl FnOnce(&[DeviceInfo]) -> Result<Option<usize>, String>,
        emit: Arc<dyn Fn(MeterEvent) + Send + Sync>,
    ) -> Result<(), SetupAudioError> {
        self.admit_meter_start(ticket)?;
        self.start_admitted_meter(ticket, config, resolve_device, emit)
    }

    fn start_admitted_meter(
        &self,
        ticket: &SetupAudioMeterTicket,
        config: MeterConfig,
        resolve_device: impl FnOnce(&[DeviceInfo]) -> Result<Option<usize>, String>,
        emit: Arc<dyn Fn(MeterEvent) + Send + Sync>,
    ) -> Result<(), SetupAudioError> {
        let operation = Operation {
            scope: self,
            permission: false,
        };
        let result = self.meter.start(ticket.epoch, config, resolve_device, emit);
        drop(operation);
        Self::validate_meter(&self.state(), ticket)?;
        result.map_err(SetupAudioError::Meter)
    }

    fn admit_meter_start(&self, ticket: &SetupAudioMeterTicket) -> Result<u64, SetupAudioError> {
        // Waiting on the capture callback would deadlock its predecessor's worker join.
        if self.meter.is_worker_scoped() {
            return Err(SetupAudioError::Meter(MeterStartError::ReentrantStart));
        }
        let current = std::thread::current().id();
        let mut state = self.state();
        loop {
            Self::validate_meter(&state, ticket)?;
            Self::admit(&state, &ticket.scope)?;
            if state.meter_started {
                return Err(SetupAudioError::StaleTicket);
            }
            match state.meter_owner {
                None => {
                    state.meter_owner = Some(current);
                    state.meter_started = true;
                    return Ok(ticket.epoch);
                }
                Some(owner) if owner == current => {
                    // A host resolver must never wait for its own outer start to return.
                    return Err(SetupAudioError::Meter(MeterStartError::ReentrantStart));
                }
                Some(_) => {
                    #[cfg(test)]
                    {
                        state.meter_waiters += 1;
                        self.settled.notify_all();
                    }
                    state = self.settled.wait(state).unwrap_or_else(|e| e.into_inner());
                    #[cfg(test)]
                    {
                        state.meter_waiters -= 1;
                    }
                }
            }
        }
    }

    /// Blocking consent on one retained owner. Concurrent requests return `Busy`, bounding
    /// admitted native requests; sequential requests reuse native grant-watch supersession.
    /// Suspend/close cancels the admitted wait, including macOS, without cancelling OS UI.
    pub fn request_permission(
        &self,
        ticket: &SetupAudioTicket,
        host: &PermissionHost,
    ) -> Result<String, SetupAudioError> {
        let permission = {
            let mut state = self.state();
            Self::admit(&state, ticket)?;
            if state.permission_busy {
                return Err(SetupAudioError::Busy);
            }
            let permission = Arc::clone(
                state
                    .permission
                    .get_or_insert_with(|| Arc::new(MicrophonePermissionSession::new())),
            );
            state.permission_busy = true;
            permission
        };
        let operation = Operation {
            scope: self,
            permission: true,
        };
        let result = permission.request(host);
        drop(operation);
        Self::validate(&self.state(), ticket)?;
        result.map_err(SetupAudioError::Permission)
    }

    /// Blocking teardown barrier, off UI & outside all host callbacks. Concurrent callers
    /// wait for the current drain; no retry/poll loop belongs in app composition. This drains
    /// only retired work; use suspend/close first. Visibility changes during drain are retained.
    /// Already-running OS/device calls & host callbacks can delay completion.
    pub fn drain(&self) {
        let permission = {
            let mut state = self.state();
            while state.draining {
                state = self.settled.wait(state).unwrap_or_else(|e| e.into_inner());
            }
            if !state.needs_drain {
                return;
            }
            state.draining = true;
            state.permission.as_ref().map(Arc::clone)
        };
        let draining = DrainOperation { scope: self };
        if let Some(permission) = &permission {
            permission.cancel();
        }
        {
            let mut state = self.state();
            while state.meter_owner.is_some() || state.permission_busy {
                state = self.settled.wait(state).unwrap_or_else(|e| e.into_inner());
            }
        }
        self.meter.drain_scoped();
        if let Some(permission) = &permission {
            permission.close();
        }
        let retired = {
            let mut state = self.state();
            let retired = state.permission.take();
            state.needs_drain = false;
            retired
        };
        // Last owner & host callback destructors must run after releasing scope state.
        drop(retired);
        drop(permission);
        drop(draining);
    }
}

impl Drop for SetupAudioScope {
    fn drop(&mut self) {
        self.close();
        // Final drop belongs off UI. Explicit drain must precede releasing final ownership;
        // blocking here could self-join when a callback releases its final weak upgrade.
        // Owned primitives still perform their normal cancellation/drain on destruction.
    }
}

struct Operation<'a> {
    scope: &'a SetupAudioScope,
    permission: bool,
}

impl Drop for Operation<'_> {
    fn drop(&mut self) {
        let mut state = self.scope.state();
        if self.permission {
            state.permission_busy = false;
        } else {
            state.meter_owner = None;
        }
        self.scope.settled.notify_all();
    }
}
// A host payload destructor may unwind during cancellation. Wake other drain callers even then;
// needs_drain remains set until successful release, allowing a later caller to finish teardown.
struct DrainOperation<'a> {
    scope: &'a SetupAudioScope,
}

impl Drop for DrainOperation<'_> {
    fn drop(&mut self) {
        let mut state = self.scope.state();
        state.draining = false;
        self.scope.settled.notify_all();
    }
}

#[cfg(test)]
mod meter_admission_tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    fn scope() -> SetupAudioScope {
        SetupAudioScope::new(SetupAudioConfig {
            initially_active: true,
            event_sink: |_, _| {},
        })
    }

    // Exercise the production start entry, without enumerating or opening hardware:
    // an invalid admission must return before the resolver/device path is reached.
    fn assert_stale_start(scope: &SetupAudioScope, ticket: &SetupAudioMeterTicket) {
        let resolved = AtomicBool::new(false);
        let result = scope.start_reserved_meter(
            ticket,
            MeterConfig {
                sample_rate: 16_000,
                block_ms: 20,
                confirm_level: 0.08,
                probe_timeout: Duration::from_secs(3),
            },
            |_| {
                resolved.store(true, Ordering::SeqCst);
                Err("unexpected resolver invocation".into())
            },
            Arc::new(|_| panic!("stale start emitted an event")),
        );
        assert_eq!(result, Err(SetupAudioError::StaleTicket));
        assert!(!resolved.load(Ordering::SeqCst));
    }

    #[test]
    fn stop_rejects_a_start_queued_before_step_exit_without_hiding_scope() {
        let scope = scope();
        let queued = scope.meter_ticket().unwrap();
        let consent = scope.ticket().unwrap();
        scope.stop_meter_now().unwrap();
        assert_stale_start(&scope, &queued);
        assert!(SetupAudioScope::validate(&scope.state(), &consent).is_ok());
        assert!(scope.ticket().is_ok());
        scope.close();
        scope.drain();
    }

    #[test]
    fn newer_device_receipt_rejects_older_queued_start_before_dispatch() {
        let scope = scope();
        let old_device = scope.meter_ticket().unwrap();
        let consent = scope.ticket().unwrap();
        let new_device = scope.meter_ticket().unwrap();
        assert_stale_start(&scope, &old_device);
        assert!(SetupAudioScope::validate_meter(&scope.state(), &new_device).is_ok());
        assert!(SetupAudioScope::validate(&scope.state(), &consent).is_ok());
        scope.close();
        scope.drain();
    }

    #[test]
    fn consent_ticket_receipt_does_not_supersede_meter_start() {
        let scope = scope();
        let meter = scope.meter_ticket().unwrap();
        let _consent = scope.ticket().unwrap();
        assert!(SetupAudioScope::validate_meter(&scope.state(), &meter).is_ok());
        scope.close();
        scope.drain();
    }

    #[test]
    fn stop_invalidates_all_clones_but_new_start_receipt_can_admit_again() {
        let scope = scope();
        let old = scope.meter_ticket().unwrap();
        let queued_clone = old.clone();
        scope.stop_meter_now().unwrap();
        let new = scope.meter_ticket().unwrap();
        assert_stale_start(&scope, &old);
        assert_stale_start(&scope, &queued_clone);
        assert!(SetupAudioScope::validate_meter(&scope.state(), &new).is_ok());
        scope.close();
        scope.drain();
    }

    #[test]
    fn earlier_legacy_stop_cannot_cancel_new_reserved_start() {
        let scope = scope();
        let delayed_stop = scope.ticket().unwrap();
        let new = scope.meter_ticket().unwrap();
        assert_eq!(
            scope.stop_meter(&delayed_stop),
            Err(SetupAudioError::StaleTicket)
        );
        assert!(SetupAudioScope::validate_meter(&scope.state(), &new).is_ok());
        scope.close();
        scope.drain();
    }

    #[test]
    fn later_legacy_stop_cancels_legacy_start_captured_before_dispatch() {
        let scope = scope();
        let start = scope.ticket().unwrap();
        let stop = scope.ticket().unwrap();
        let admitted = scope.admit_legacy_meter_start(&start).unwrap();
        scope.stop_meter(&stop).unwrap();
        assert!(matches!(
            SetupAudioScope::validate_meter(&scope.state(), &admitted),
            Err(SetupAudioError::StaleTicket)
        ));
        assert!(matches!(
            scope.admit_legacy_meter_start(&start),
            Err(SetupAudioError::StaleTicket)
        ));
        drop(Operation {
            scope: &scope,
            permission: false,
        });
        scope.close();
        scope.drain();
    }

    #[test]
    fn later_legacy_start_captured_before_admission_survives_predecessor_completion() {
        let scope = scope();
        let first = scope.ticket().unwrap();
        let later = scope.ticket().unwrap();
        let first_admitted = scope.admit_legacy_meter_start(&first).unwrap();
        drop(Operation {
            scope: &scope,
            permission: false,
        });
        let later_admitted = scope.admit_legacy_meter_start(&later).unwrap();
        assert!(SetupAudioScope::validate_meter(&scope.state(), &first_admitted).is_err());
        assert!(SetupAudioScope::validate_meter(&scope.state(), &later_admitted).is_ok());
        drop(Operation {
            scope: &scope,
            permission: false,
        });
        scope.close();
        scope.drain();
    }

    #[test]
    fn earlier_legacy_stop_cannot_cancel_later_legacy_start() {
        let scope = scope();
        let stop = scope.ticket().unwrap();
        let start = scope.ticket().unwrap();
        let admitted = scope.admit_legacy_meter_start(&start).unwrap();
        assert_eq!(scope.stop_meter(&stop), Err(SetupAudioError::StaleTicket));
        assert!(SetupAudioScope::validate_meter(&scope.state(), &admitted).is_ok());
        drop(Operation {
            scope: &scope,
            permission: false,
        });
        assert!(matches!(
            scope.admit_legacy_meter_start(&start),
            Err(SetupAudioError::StaleTicket)
        ));
        scope.close();
        scope.drain();
    }

    #[test]
    fn later_legacy_stop_cancels_reserved_start() {
        let scope = scope();
        let start = scope.meter_ticket().unwrap();
        let stop = scope.ticket().unwrap();
        scope.admit_meter_start(&start).unwrap();
        scope.stop_meter(&stop).unwrap();
        assert_stale_start(&scope, &start);
        drop(Operation {
            scope: &scope,
            permission: false,
        });
        scope.close();
        scope.drain();
    }

    #[test]
    fn legacy_start_cannot_revive_after_later_queued_stop_runs_first() {
        let scope = scope();
        let start = scope.ticket().unwrap();
        let stop = scope.ticket().unwrap();
        scope.stop_meter(&stop).unwrap();
        assert!(matches!(
            scope.admit_legacy_meter_start(&start),
            Err(SetupAudioError::StaleTicket)
        ));
        scope.close();
        scope.drain();
    }

    #[test]
    fn legacy_start_queued_before_stop_never_reaches_hardware() {
        let scope = scope();
        let queued = scope.ticket().unwrap();
        scope.stop_meter_now().unwrap();
        let result = scope.start_meter(
            &queued,
            MeterConfig {
                sample_rate: 16_000,
                block_ms: 20,
                confirm_level: 0.08,
                probe_timeout: Duration::from_secs(3),
            },
            |_| panic!("stale legacy start reached resolver"),
            Arc::new(|_| panic!("stale legacy start emitted")),
        );
        assert_eq!(result, Err(SetupAudioError::StaleTicket));
        scope.close();
        scope.drain();
    }

    #[test]
    fn meter_changes_preserve_existing_consent_owner_and_marker() {
        let scope = scope();
        let consent = scope.ticket().unwrap();
        let owner = Arc::new(MicrophonePermissionSession::new());
        {
            let mut state = scope.state();
            state.permission = Some(Arc::clone(&owner));
            state.permission_busy = true;
        }
        scope.meter_ticket().unwrap();
        scope.stop_meter_now().unwrap();
        let legacy = scope.ticket().unwrap();
        scope.stop_meter(&legacy).unwrap();
        {
            let mut state = scope.state();
            assert!(SetupAudioScope::validate(&state, &consent).is_ok());
            assert!(state.permission_busy);
            assert!(Arc::ptr_eq(state.permission.as_ref().unwrap(), &owner));
            state.permission_busy = false;
        }
        scope.close();
        scope.drain();
    }

    #[test]
    fn hide_and_reveal_never_revive_queued_meter_admission() {
        let scope = scope();
        let queued = scope.meter_ticket().unwrap();
        scope.suspend();
        assert_eq!(scope.activate(), Err(SetupAudioError::Draining));
        let new = scope.meter_ticket().unwrap();
        assert_eq!(
            scope.admit_meter_start(&new),
            Err(SetupAudioError::Draining)
        );
        scope.drain();
        assert_stale_start(&scope, &queued);
        assert_eq!(scope.admit_meter_start(&new), Ok(new.epoch));
        drop(Operation {
            scope: &scope,
            permission: false,
        });
        scope.close();
        scope.drain();
    }
}

#[cfg(test)]
mod meter_overlap_tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    fn scope() -> Arc<SetupAudioScope> {
        Arc::new(SetupAudioScope::new(SetupAudioConfig {
            initially_active: true,
            event_sink: |_, _| {},
        }))
    }

    // Observing this counter requires acquiring the scope mutex after the worker releases
    // it at Condvar::wait, so cancellation cannot race ahead of the actual wait boundary.
    fn wait_until_blocked(scope: &SetupAudioScope) {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let mut state = scope.state();
        while state.meter_waiters == 0 {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            assert!(!remaining.is_zero(), "waiter never reached admission wait");
            state = scope.settled.wait_timeout(state, remaining).unwrap().0;
        }
    }

    #[test]
    fn newest_selection_waits_for_predecessor_without_a_busy_refusal() {
        let scope = scope();
        let old = scope.meter_ticket().unwrap();
        scope.admit_meter_start(&old).unwrap();
        let predecessor = Operation {
            scope: &scope,
            permission: false,
        };
        let newest = scope.meter_ticket().unwrap();
        let expected_epoch = newest.epoch;
        let (finished, finish_received) = mpsc::channel();
        let worker_scope = Arc::clone(&scope);
        let worker = std::thread::spawn(move || {
            let result = worker_scope.admit_meter_start(&newest);
            let operation = result.as_ref().ok().map(|_| Operation {
                scope: &worker_scope,
                permission: false,
            });
            finished.send(result).unwrap();
            drop(operation);
        });
        wait_until_blocked(&scope);
        assert!(matches!(
            finish_received.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        drop(predecessor);
        assert_eq!(
            finish_received
                .recv_timeout(Duration::from_secs(2))
                .unwrap(),
            Ok(expected_epoch)
        );
        worker.join().unwrap();
        scope.close();
        scope.drain();
    }

    #[test]
    fn stop_wakes_queued_selection_while_predecessor_remains_admitted() {
        let scope = scope();
        let old = scope.meter_ticket().unwrap();
        scope.admit_meter_start(&old).unwrap();
        let predecessor = Operation {
            scope: &scope,
            permission: false,
        };
        let queued = scope.meter_ticket().unwrap();
        let (finished, finish_received) = mpsc::channel();
        let worker_scope = Arc::clone(&scope);
        let worker = std::thread::spawn(move || {
            finished
                .send(worker_scope.admit_meter_start(&queued))
                .unwrap();
        });
        wait_until_blocked(&scope);
        scope.stop_meter_now().unwrap();
        assert_eq!(
            finish_received
                .recv_timeout(Duration::from_secs(2))
                .unwrap(),
            Err(SetupAudioError::StaleTicket)
        );
        // The waiter must finish without waiting for the older resolver to return.
        drop(predecessor);
        worker.join().unwrap();
        scope.close();
        scope.drain();
    }

    #[test]
    fn newer_receipt_rejects_waiter_before_predecessor_returns() {
        let scope = scope();
        let old = scope.meter_ticket().unwrap();
        scope.admit_meter_start(&old).unwrap();
        let predecessor = Operation {
            scope: &scope,
            permission: false,
        };
        let queued = scope.meter_ticket().unwrap();
        let (finished, finish_received) = mpsc::channel();
        let worker_scope = Arc::clone(&scope);
        let worker = std::thread::spawn(move || {
            finished
                .send(worker_scope.admit_meter_start(&queued))
                .unwrap();
        });
        wait_until_blocked(&scope);
        let newest = scope.meter_ticket().unwrap();
        assert_eq!(
            finish_received
                .recv_timeout(Duration::from_secs(2))
                .unwrap(),
            Err(SetupAudioError::StaleTicket)
        );
        assert!(SetupAudioScope::validate_meter(&scope.state(), &newest).is_ok());
        assert!(scope.state().meter_owner.is_some());
        worker.join().unwrap();
        drop(predecessor);
        scope.admit_meter_start(&newest).unwrap();
        drop(Operation {
            scope: &scope,
            permission: false,
        });
        scope.close();
        scope.drain();
    }

    #[test]
    fn legacy_busy_and_draining_refusals_leave_fresh_ticket_retryable() {
        let scope = scope();
        let old = scope.meter_ticket().unwrap();
        scope.admit_meter_start(&old).unwrap();
        let predecessor = Operation {
            scope: &scope,
            permission: false,
        };
        let legacy = scope.ticket().unwrap();
        assert!(matches!(
            scope.admit_legacy_meter_start(&legacy),
            Err(SetupAudioError::Busy)
        ));
        drop(predecessor);
        let admitted = scope.admit_legacy_meter_start(&legacy).unwrap();
        drop(Operation {
            scope: &scope,
            permission: false,
        });
        assert!(SetupAudioScope::validate_meter(&scope.state(), &admitted).is_ok());
        scope.suspend();
        assert_eq!(scope.activate(), Err(SetupAudioError::Draining));
        let fresh = scope.ticket().unwrap();
        assert!(matches!(
            scope.admit_legacy_meter_start(&fresh),
            Err(SetupAudioError::Draining)
        ));
        scope.drain();
        scope.admit_legacy_meter_start(&fresh).unwrap();
        drop(Operation {
            scope: &scope,
            permission: false,
        });
        scope.close();
        scope.drain();
    }

    #[test]
    fn resolver_reentrancy_refuses_instead_of_waiting_on_itself() {
        let scope = scope();
        let old = scope.meter_ticket().unwrap();
        scope.admit_meter_start(&old).unwrap();
        let predecessor = Operation {
            scope: &scope,
            permission: false,
        };
        let newest = scope.meter_ticket().unwrap();
        assert_eq!(
            scope.admit_meter_start(&newest),
            Err(SetupAudioError::Meter(MeterStartError::ReentrantStart))
        );
        drop(predecessor);
        scope.admit_meter_start(&newest).unwrap();
        drop(Operation {
            scope: &scope,
            permission: false,
        });
        // A used token or clone cannot silently restart or report a duplicate success.
        assert_eq!(
            scope.admit_meter_start(&newest.clone()),
            Err(SetupAudioError::StaleTicket)
        );
        scope.close();
        scope.drain();
    }
}

#[cfg(test)]
mod meter_ingress_tests {
    use super::*;

    fn scope() -> SetupAudioScope {
        SetupAudioScope::new(SetupAudioConfig {
            initially_active: true,
            // Ingress must never invoke host telemetry while holding its lock.
            event_sink: |_, _| panic!("host callback from admission/stop ingress"),
        })
    }

    #[test]
    fn completed_stop_cannot_reappear_in_deferred_drain_and_cancel_later_start() {
        let scope = scope();
        let old = scope.meter_ticket().unwrap();
        let consent = scope.ticket().unwrap();
        scope.stop_meter_now().unwrap();
        let later = scope.meter_ticket().unwrap();
        // Only release work may be deferred; no unversioned stop command is queued.
        scope.drain();
        assert!(SetupAudioScope::validate_meter(&scope.state(), &old).is_err());
        assert!(SetupAudioScope::validate(&scope.state(), &consent).is_ok());
        assert!(scope.meter.scoped_epoch_is_current(later.epoch));
        scope.admit_meter_start(&later).unwrap();
        drop(Operation {
            scope: &scope,
            permission: false,
        });
        scope.close();
        scope.drain();
    }

    #[test]
    fn deferred_hide_drain_preserves_later_reveal_and_meter_receipt() {
        let scope = scope();
        let old = scope.meter_ticket().unwrap();
        scope.suspend();
        assert_eq!(scope.activate(), Err(SetupAudioError::Draining));
        let later = scope.meter_ticket().unwrap();
        // The queued old hide does not execute a second stop; it only drains.
        scope.drain();
        assert!(SetupAudioScope::validate_meter(&scope.state(), &old).is_err());
        assert!(scope.meter.scoped_epoch_is_current(later.epoch));
        scope.admit_meter_start(&later).unwrap();
        drop(Operation {
            scope: &scope,
            permission: false,
        });
        scope.close();
        scope.drain();
    }
}
