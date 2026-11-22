//! Host-owned setup audio admission & teardown, independent of any window/runtime framework.
//!
//! Construct with explicit initial activity (including headless hosts). Capture a [`SetupAudioTicket`](crate::setup::scope::SetupAudioTicket)
//! when accepting each user command, BEFORE dispatching blocking work; reusing a pre-hide ticket
//! after reveal is rejected. `activate` records visibility even while teardown is draining.
//! `suspend` cancels meter & consent immediately. Dispatch `drain` on an existing blocking executor;
//! after it returns, new-generation tickets can run without calling `activate` again. A command
//! receiving `Draining` may drain off UI, then retry its SAME ticket; no implicit resume occurs.
//!
//! Renderer meter-stop calls only `stop_meter`, preserving pending native consent. Window hide or
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
    /// One operation of this kind is already admitted; no additional call is queued.
    Busy,
    Meter(MeterStartError),
    Permission(String),
}

struct State {
    active: bool,
    closed: bool,
    generation: Arc<()>,
    needs_drain: bool,
    draining: bool,
    meter_busy: bool,
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
                needs_drain: false,
                draining: false,
                meter_busy: false,
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

    /// Capture on command receipt, before dispatch. Allowed while visible teardown drains.
    pub fn ticket(&self) -> Result<SetupAudioTicket, SetupAudioError> {
        let state = self.state();
        if state.closed {
            return Err(SetupAudioError::Closed);
        }
        if !state.active {
            return Err(SetupAudioError::Inactive);
        }
        Ok(SetupAudioTicket {
            generation: Arc::downgrade(&state.generation),
        })
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

    /// Nonblocking hide/suspend. Already-admitted callbacks or device calls may unwind;
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
            state.permission.as_ref().map(Arc::clone)
        };
        // Session cancellation may release host payloads; never hold scope state here.
        if let Some(permission) = permission {
            permission.cancel();
        }
    }

    /// Renderer-only stop, intentionally preserving permission prompt & grant watch.
    pub fn stop_meter(&self, ticket: &SetupAudioTicket) -> Result<(), SetupAudioError> {
        let state = self.state();
        Self::validate(&state, ticket)?;
        self.meter.stop_scoped();
        Ok(())
    }

    /// Blocking. Caller verifies an existing OS grant & explicit user action. At most one
    /// start/resolver is admitted; device replacement drains its predecessor in `MicMeter`.
    pub fn start_meter(
        &self,
        ticket: &SetupAudioTicket,
        config: MeterConfig,
        resolve_device: impl FnOnce(&[DeviceInfo]) -> Result<Option<usize>, String>,
        emit: Arc<dyn Fn(MeterEvent) + Send + Sync>,
    ) -> Result<(), SetupAudioError> {
        let epoch = {
            let mut state = self.state();
            Self::admit(&state, ticket)?;
            if state.meter_busy {
                return Err(SetupAudioError::Busy);
            }
            state.meter_busy = true;
            self.meter.begin_scoped()
        };
        let operation = Operation {
            scope: self,
            permission: false,
        };
        let result = self.meter.start(epoch, config, resolve_device, emit);
        drop(operation);
        Self::validate(&self.state(), ticket)?;
        result.map_err(SetupAudioError::Meter)
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
            while state.meter_busy || state.permission_busy {
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
            state.meter_busy = false;
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
