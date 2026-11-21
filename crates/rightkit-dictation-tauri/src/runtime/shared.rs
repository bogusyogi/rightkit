// Shared imports live on the module root (runtime.rs); `use super::*` keeps
// this file on the same namespace the include!-based layout provided.
#[allow(unused_imports)]
use super::*;

/// `Arc<RwLock<AppState>>` — the lock-free half of the runtime state machine.
///
/// `EngineRuntime.state` is a `SharedState`, and the SAME `Arc` is cloned into
/// `SidecarHandle::state`, which `open_sidecar` publishes in
/// `ENGINE_SIDECAR`. Single-writer semantics are preserved: every mutation
/// still runs under the global `Mutex<RuntimeApp>`; the `RwLock` only lets
/// *readers* (`get_app_state`, the transcribe watchdog, future status
/// surfaces) snapshot the state without queuing behind a held lock (e.g.
/// prewarm's config/vocab sync).
///
/// Poison policy: a panic while a writer holds the write lock would poison
/// the `RwLock`; both `get`/`set` recover via `into_inner` — `AppState` is a
/// plain data enum, so a mid-write panic cannot leave it structurally
/// inconsistent (each `set` is one atomic store of a complete value).
#[derive(Clone)]
pub struct SharedState {
    inner: Arc<RwLock<AppState>>,
    /// Bumped on every transition INTO `Error`, under the write lock. A
    /// terminal-error expiry timer is bound to the generation it observed, so
    /// an expiry from an older Error can never reset a newer interaction.
    error_generation: Arc<std::sync::atomic::AtomicU64>,
}

impl SharedState {
    pub fn new(state: AppState) -> Self {
        Self {
            inner: Arc::new(RwLock::new(state)),
            error_generation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }

    /// Generation of the most recent transition into `Error`.
    pub fn error_generation(&self) -> u64 {
        self.error_generation
            .load(std::sync::atomic::Ordering::Acquire)
    }

    /// Terminal-error recovery decision: the state must still be `Error`
    /// and, when the caller carries a generation, it must be the CURRENT
    /// error's. `None` is the legacy unbound caller (state check only).
    pub fn terminal_error_recovery_permits(&self, expected: Option<u64>) -> bool {
        let guard = self
            .inner
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        matches!(*guard, AppState::Error { .. })
            && expected.is_none_or(|g| g == self.error_generation())
    }

    pub fn get(&self) -> AppState {
        self.inner
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn set(&self, state: AppState) {
        let mut guard = self
            .inner
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if matches!(state, AppState::Error { .. }) {
            self.error_generation
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        }
        *guard = state;
    }

    /// Emission-authority decision: `candidate` may emit only while it is
    /// still the authoritative value — session-bound, because `AppState`
    /// equality carries the session id, so a superseded `Recording` or
    /// terminal snapshot can never paint over the state that replaced it.
    /// Called inside the serialized emission critical section in
    /// `dictation_lifecycle` — pairing this read with the emit is what makes
    /// terminal-write-wins atomic.
    pub fn emission_permits(&self, candidate: &AppState) -> bool {
        self.get() == *candidate
    }
}

/// The sidecar half of the running runtime, registered once by
/// `RuntimeApp::open_sidecar`. `None` until the runtime has been opened — every
/// lock-free facade falls back to the locked path then.
static ENGINE_SIDECAR: OnceLock<SidecarHandle> = OnceLock::new();

pub fn register_engine_sidecar(handle: &SidecarHandle) {
    // The supervisor tells a recording engine from an idle one through the shell's active-session
    // mirror: no key-delivery path retires a generation while it holds a session.
    handle
        .supervisor
        .register_active_session_probe(Arc::clone(&handle.active_session));
    // APP itself is OnceLock-initialized, so this fires exactly once; a second
    // set would mean a second runtime was constructed, which must not happen.
    if ENGINE_SIDECAR.set(handle.clone()).is_err() {
        crate::telemetry::log_message(
            "engine_sidecar_register_duplicate",
            "ENGINE_SIDECAR already initialized; keeping the first handle",
        );
    }
}

pub fn engine_sidecar() -> Option<&'static SidecarHandle> {
    ENGINE_SIDECAR.get()
}

/// Start attempts: one id per shortcut press, strictly increasing for the life of the process.
/// The id travels on the press's warm-up, its `RecordingStarted`, and its session's stop and
/// cancel frames, so the engine can tell which press every frame belongs to and fence a warm-up
/// that arrives late, out of order, or after its press ended.
static START_ATTEMPT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// The attempt of the newest session. A stop or cancel for any older session carries no id, so it
/// can never revoke a newer press's warm-up.
static SESSION_START_ATTEMPT: StdMutex<Option<(String, Option<u64>)>> = StdMutex::new(None);

/// Allocate the next start attempt for a shortcut press. The caller owns the returned id and
/// threads it explicitly through its own start (`prepare_start`, the start frame) and its stop and
/// cancel; no process-wide slot holds it, so concurrent presses can never take each other's id.
pub fn begin_start_attempt() -> u64 {
    START_ATTEMPT.fetch_add(1, Ordering::AcqRel) + 1
}

/// Record that `session_id` was started by `attempt`.
pub(crate) fn bind_session_capture_route(session_id: &str, attempt: Option<u64>) {
    if let Ok(mut slot) = SESSION_START_ATTEMPT.lock() {
        *slot = Some((session_id.to_string(), attempt));
    }
}

/// The attempt that started `session_id`; `None` for any other session.
pub(crate) fn session_start_attempt(session_id: &str) -> Option<u64> {
    let slot = SESSION_START_ATTEMPT.lock().ok()?;
    match slot.as_ref() {
        Some((id, attempt)) if id == session_id => *attempt,
        _ => None,
    }
}

/// The one start in flight, if any. A shortcut press claims it before it does anything else (see
/// [`StartReservation`]); a cancel or stop that finds no session yet marks it cancelled
/// ([`cancel_pending_start`]).
static PENDING_START: StdMutex<Option<Arc<PendingStart>>> = StdMutex::new(None);

const START_PENDING: u8 = 0;
const START_CANCELLED: u8 = 1;
const START_COMMITTED: u8 = 2;
/// A stop (not a cancel) reached the press before its start committed: the start goes on, and
/// performs the stop itself as soon as `Recording` is committed.
const START_STOP_REQUESTED: u8 = 3;

/// The stop a user asked for while the press's start was still pending. It is distinct from a
/// cancel: the start completes (adopting the warm-up audio), commits `Recording`, and then runs
/// exactly this stop, so a short press-and-release is transcribed and delivered instead of
/// discarded. Only a cancel discards.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeferredStop {
    /// `stop_dictation_inner` (shortcut, tray, app).
    Stop,
    /// `stop_dictation_from_pill_inner`.
    PillStop { send_enter: bool },
    /// `stop_dictation_and_send_inner`.
    StopAndSend,
}

/// The identity of one press's start: its attempt id and whether it was cancelled before it
/// committed. The phase moves once, by compare-exchange, out of `Pending`: either the start
/// commits `Recording` (`commit`) or a cancel reaches it first (`cancel`); the loser sees the
/// other's result, so a cancel can never be lost between the start's last check and its commit,
/// and a start can never commit after it was cancelled. A stop moves `Pending` to `StopRequested`
/// (`defer_stop`): the start still commits (from either phase) and then takes the stop to run
/// (`take_deferred_stop`); a later cancel still wins over a stop that was not yet performed.
pub struct PendingStart {
    attempt: u64,
    exact_capture: bool,
    phase: std::sync::atomic::AtomicU8,
    /// The stop to run after the commit. Written only under this lock together with the phase
    /// move, and read by the start under it after its commit, so the two cannot miss each other.
    deferred: StdMutex<Option<DeferredStop>>,
    /// The session this press's start committed (`Recording`), bound once at the commit. A
    /// deferred stop runs only against this exact session (`dictation_control::run_deferred_stop`),
    /// so it can never stop a different recording than the press it belongs to.
    session: StdMutex<Option<String>>,
    /// The release of this press that the engine could not confirm (see `seal_capture_for_stop`)
    /// and whether this press has begun capture. One lock, so a release and the first capture
    /// can never both miss each other: either the release is recorded first and the start refuses
    /// to record (`begin_capture`), or capture began first and the release is applied as the seal
    /// cutoff before the deferred stop (`seal_capture_at_recorded_release`).
    release: StdMutex<ReleaseTrace>,
}

/// The pending start records the key-up mark when the key is released.
#[derive(Clone)]
pub struct OriginalReleaseMark {
    at: Instant,
    edge: Option<rightkit_dictation_core::engine::CaptureReleaseEdge>,
}

impl OriginalReleaseMark {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    pub fn edge_from_audio(
        edge: rightkit_audio::capture::CaptureEdge,
    ) -> rightkit_dictation_core::engine::CaptureReleaseEdge {
        use rightkit_audio::capture::CaptureClock;
        rightkit_dictation_core::engine::CaptureReleaseEdge {
            version: edge.version,
            clock: match edge.clock {
                CaptureClock::WasapiQpc100nsV1 => "WasapiQpc100nsV1",
                CaptureClock::CoreAudioHostNsV1 => "CoreAudioHostNsV1",
            }
            .into(),
            seconds: edge.seconds,
            nanos: edge.nanos,
        }
    }

    /// Explicit wake-word/developer legacy action without a physical release marker.
    pub fn legacy_receipt() -> Self {
        Self {
            at: Instant::now(),
            edge: None,
        }
    }

    /// An expected keyboard edge is missing. Keep exact mode & force rejection.
    pub fn missing() -> Self {
        Self {
            at: Instant::now(),
            edge: Some(rightkit_dictation_core::engine::CaptureReleaseEdge {
                version: 0,
                clock: "MissingNativeRelease".into(),
                seconds: 0,
                nanos: 0,
            }),
        }
    }

    /// Supply the marker sampled by the native key hook before its asynchronous queue.
    pub fn from_edge(edge: rightkit_dictation_core::engine::CaptureReleaseEdge) -> Self {
        Self {
            at: Instant::now(),
            edge: Some(edge),
        }
    }

    /// Choose route before sampling. Supplied `*_at` marks are never changed.
    pub fn for_current_action() -> Self {
        if Self::current_capture_is_legacy() {
            Self::legacy_receipt()
        } else {
            Self::now()
        }
    }

    /// True only for a positively bound legacy start, never a missing exact epoch.
    pub fn current_capture_is_legacy() -> bool {
        let legacy = pending_start_token()
            .map(|token| token.capture_attempt().is_none())
            .unwrap_or_else(|| {
                let active = active_session_lock_free();
                SESSION_START_ATTEMPT.lock().ok().is_some_and(|slot| {
                    slot.as_ref().is_some_and(|(id, attempt)| {
                        Some(id) == active.as_ref() && attempt.is_none()
                    })
                })
            });
        legacy
    }

    pub fn now() -> Self {
        let at = Instant::now();
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        let edge = Some(
            match rightkit_audio::capture::capture_edge_now()
                .map_err(|error| error.to_string())
                .and_then(|edge| serde_json::to_value(edge).map_err(|error| error.to_string()))
                .and_then(|value| serde_json::from_value(value).map_err(|error| error.to_string()))
            {
                Ok(edge) => edge,
                // Invalid marker intentionally requires receiver rejection, never legacy fallback.
                Err(error) => {
                    crate::telemetry::log_message("capture_release_clock_failed", &error);
                    rightkit_dictation_core::engine::CaptureReleaseEdge {
                        version: 0,
                        clock: "Unavailable".into(),
                        seconds: 0,
                        nanos: 0,
                    }
                }
            },
        );
        // Explicit compatibility opt-out on platforms with no common OS capture clock.
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        let edge = None;
        Self { at, edge }
    }
}

#[derive(Default)]
struct ReleaseTrace {
    /// The first instant the user released (or otherwise asked to stop) this press while no engine
    /// confirmed the seal: the engine was down or retiring, or did not answer in time.
    ///
    /// Exact markers retain native clock/version/seconds/nanos unchanged across IPC & retry.
    /// Legacy peers alone use release_age_ms; the engine owns the stream-bound cut handle.
    unsealed_release: Option<OriginalReleaseMark>,
    /// Capture of this press has begun: its warm-up reached a running engine, or its start is
    /// about to be sent to the engine.
    capture_begun: bool,
}

impl PendingStart {
    /// This press's start attempt (the id on its warm-up, start frame, stop and cancel frames).
    pub fn capture_attempt(&self) -> Option<u64> {
        self.exact_capture.then_some(self.attempt)
    }

    pub fn attempt(&self) -> u64 {
        self.attempt
    }

    /// A cancel reached this press before its start committed. The start checks this
    /// before it sends to the backend, while it waits for live capture, and before it commits
    /// `Recording`. A pending stop does not count: the start continues for it.
    pub fn is_cancelled(&self) -> bool {
        self.phase.load(Ordering::Acquire) == START_CANCELLED
    }

    /// The start commits `Recording`. `false` when a cancel won first: the caller must abort
    /// instead. After `true`, a cancel finds no pending start and takes the ordinary path against
    /// the committed session. A pending stop does not block the commit.
    pub fn commit(&self) -> bool {
        [START_PENDING, START_STOP_REQUESTED]
            .into_iter()
            .any(|from| {
                self.phase
                    .compare_exchange(from, START_COMMITTED, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            })
    }

    /// Record the session this press committed. Called by the start right after a successful
    /// `commit`, before anything can take the deferred stop.
    pub fn bind_session(&self, session_id: String) {
        *self.session.lock().unwrap_or_else(|e| e.into_inner()) = Some(session_id);
    }

    /// The session this press committed, once known (`None` before the commit or when the start
    /// produced no engine session).
    pub fn committed_session(&self) -> Option<String> {
        self.session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// The stop a user asked for while this start was pending, taken once after a successful
    /// `commit`. The start runs it as an ordinary stop against the committed session.
    pub fn take_deferred_stop(&self) -> Option<DeferredStop> {
        self.deferred
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }

    /// Record the release of this press that no engine confirmed. The earliest instant wins.
    fn note_unsealed_release(&self, at: OriginalReleaseMark) {
        let mut trace = self.release.lock().unwrap_or_else(|e| e.into_inner());
        if trace.unsealed_release.is_none() {
            trace.unsealed_release = Some(at);
        }
    }

    /// Whether capture of this press has begun (see [`PendingStart::begin_capture`]).
    pub fn capture_begun(&self) -> bool {
        self.release
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .capture_begun
    }

    /// The unconfirmed release instant of this press, if any.
    pub fn unsealed_release(&self) -> Option<OriginalReleaseMark> {
        self.release
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .unsealed_release
            .clone()
    }

    /// This press is about to begin capture (warm-up to a running engine, or the start frame).
    /// `false`, with the press cancelled, when the user already released it while no capture
    /// existed: every sample a capture started now would hold was spoken after the release, so the
    /// press ends as a cancelled, empty one (no recording state, no audio, no deferred stop to
    /// replay). `true` once capture has begun (idempotent); a release arriving after that is
    /// applied as the seal cutoff instead.
    pub fn begin_capture(&self) -> bool {
        {
            let mut trace = self.release.lock().unwrap_or_else(|e| e.into_inner());
            if trace.capture_begun {
                return true;
            }
            if trace.unsealed_release.is_none() {
                trace.capture_begun = true;
                return true;
            }
        }
        self.end_released_before_capture();
        false
    }

    /// Non-mutating gate for the places that run before any capture (the top of the start, the
    /// optimistic pill): `true`, with the press cancelled, when it was released before capture
    /// began.
    pub fn refuse_if_released_before_capture(&self) -> bool {
        let released = {
            let trace = self.release.lock().unwrap_or_else(|e| e.into_inner());
            !trace.capture_begun && trace.unsealed_release.is_some()
        };
        if released {
            self.end_released_before_capture();
        }
        released
    }

    fn end_released_before_capture(&self) {
        // A cancel (it also clears a pending stop) so the start unwinds through its ordinary
        // cancelled path. A cancel that already won leaves this a no-op.
        let _ = self.cancel();
        crate::telemetry::log_message("start_released_before_capture", "phase=before_capture");
    }

    fn defer_stop(&self, stop: DeferredStop) -> bool {
        let mut slot = self.deferred.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            match self.phase.load(Ordering::Acquire) {
                START_PENDING => {
                    if self
                        .phase
                        .compare_exchange(
                            START_PENDING,
                            START_STOP_REQUESTED,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        *slot = Some(stop);
                        return true;
                    }
                }
                // A second stop on the same press: the latest wins (a stop-and-send after a plain
                // stop sends the Enter).
                START_STOP_REQUESTED => {
                    *slot = Some(stop);
                    return true;
                }
                _ => return false,
            }
        }
    }

    fn cancel(&self) -> bool {
        let won = [START_PENDING, START_STOP_REQUESTED]
            .into_iter()
            .any(|from| {
                self.phase
                    .compare_exchange(from, START_CANCELLED, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            });
        if won {
            // A cancel discards the press, including a stop that was waiting for the start.
            *self.deferred.lock().unwrap_or_else(|e| e.into_inner()) = None;
        }
        won
    }
}

/// The reservation of the whole start of one press. Only one press at a time may be starting:
/// `claim` fails while another press's start call has not fully ended, and a press that fails it
/// must return at once (it never observes the target, shows the pill, takes the backend gate,
/// reaches the engine, or revokes anything). Released when the guard drops, that is when the
/// reserving press's start call returns, success or failure. Allocates the press's attempt id and
/// its [`PendingStart`].
pub struct StartReservation {
    token: Arc<PendingStart>,
    /// Holds the engine for this start (`EngineSupervisor::hold_capture`): no key-delivery path
    /// retires the engine generation while the start is reserved. Type-erased on purpose (the
    /// guard type is not re-exported); released when the reservation drops.
    _engine_hold: Option<Box<dyn std::any::Any + Send>>,
}

impl StartReservation {
    pub fn claim() -> Option<Self> {
        Self::claim_with_route(true)
    }

    pub fn claim_legacy() -> Option<Self> {
        Self::claim_with_route(false)
    }

    fn claim_with_route(exact_capture: bool) -> Option<Self> {
        let mut slot = PENDING_START.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_some() {
            return None;
        }
        let token = Arc::new(PendingStart {
            attempt: begin_start_attempt(),
            exact_capture,
            phase: std::sync::atomic::AtomicU8::new(START_PENDING),
            deferred: StdMutex::new(None),
            session: StdMutex::new(None),
            release: StdMutex::new(ReleaseTrace::default()),
        });
        *slot = Some(Arc::clone(&token));
        // Never waits here: this runs with PENDING_START held, often on the hotkey dispatch
        // thread. While an engine retirement is shutting down, the hold is taken later by
        // `ensure_engine_hold` on the start's own thread.
        let hold = engine_sidecar().and_then(|sidecar| {
            sidecar
                .supervisor
                .try_hold_capture()
                .map(|hold| Box::new(hold) as Box<dyn std::any::Any + Send>)
        });
        Some(Self {
            token,
            _engine_hold: hold,
        })
    }

    /// Take the engine hold if the claim could not (a retirement was in flight). Blocks only for
    /// that shutdown; called at the top of the start, off the dispatch thread, holding no slot.
    pub fn ensure_engine_hold(&mut self) {
        if self._engine_hold.is_some() {
            return;
        }
        self._engine_hold = engine_sidecar().map(|sidecar| {
            Box::new(sidecar.supervisor.hold_capture()) as Box<dyn std::any::Any + Send>
        });
    }

    pub fn token(&self) -> Arc<PendingStart> {
        Arc::clone(&self.token)
    }
}

impl Drop for StartReservation {
    fn drop(&mut self) {
        let mut slot = PENDING_START.lock().unwrap_or_else(|e| e.into_inner());
        if slot
            .as_ref()
            .is_some_and(|held| Arc::ptr_eq(held, &self.token))
        {
            *slot = None;
        }
    }
}

/// A STOP (plain, from the pill, or stop-and-send) arrived: if a start is still pending (no
/// session committed yet), record the stop as that press's intent and return `true`. The stop is
/// not a cancel: nothing is revoked or fenced, the start keeps its warm-up and completes, commits
/// `Recording`, and then runs the stop itself (`PendingStart::take_deferred_stop`), so a short
/// press-and-release is finalised and delivered. The caller returns the current state at once.
/// `false` when no start is pending or it already committed: the caller then takes the ordinary
/// stop path against the session.
pub fn defer_stop_to_pending_start(stop: DeferredStop) -> bool {
    let token = {
        let slot = PENDING_START.lock().unwrap_or_else(|e| e.into_inner());
        match slot.as_ref() {
            Some(token) => Arc::clone(token),
            None => return false,
        }
    };
    if !token.defer_stop(stop) {
        return false;
    }
    crate::telemetry::log_message("start_stop_deferred", "phase=before_commit");
    true
}

/// A CANCEL arrived (only a cancel discards; a stop goes through `defer_stop_to_pending_start`): if a start is still pending (no session committed yet, whether it is
/// still observing the target, waiting for the backend gate, or waiting for live capture), mark it
/// cancelled and revoke its warm-up now, then return `true`. It never waits for the start: the
/// start notices at its next check and unwinds cleanly. `false` when no start is pending or it
/// already committed; the caller then takes the ordinary stop/cancel path.
pub fn cancel_pending_start() -> bool {
    let token = {
        let slot = PENDING_START.lock().unwrap_or_else(|e| e.into_inner());
        match slot.as_ref() {
            Some(token) => Arc::clone(token),
            None => return false,
        }
    };
    if !token.cancel() {
        return false;
    }
    crate::telemetry::log_message("start_cancelled_pending", "phase=before_commit");
    revoke_capture_warmup(token.attempt());
    true
}

/// Shortcut pressed (start attempt `attempt`, from the press's [`StartReservation`]): tell a
/// running engine to resume its idle microphone stream now, so the audio spoken while the shell
/// observes the target and allocates the session is kept. It touches neither the runtime lock nor
/// the transition gate, does not wait for the engine and is never waited on: the start does not
/// block on it. The engine adopts the audio if this warm-up reached its worker before the start of
/// the same attempt; if the start got there first it cold starts, and the late warm-up is fenced
/// by attempt id (ignored, resumes nothing). The worker pauses the stream again a fixed time after
/// it resumed when no start follows.
///
/// The frame goes out through the narrowly typed `EngineSupervisor::send_capture_control`, which
/// skips the provider-key gate wait and config work and only reaches an engine that is already
/// running (it never spawns one). Best effort and content-free: no engine, a stopped engine or a
/// rejected frame (an older engine that does not know the request) leaves the start exactly as it
/// was. It carries no session and no target, so the hotkey-time target proof in `RecordingStarted`
/// is unchanged.
///
/// Capture begins here, so the press's [`PendingStart::begin_capture`] is taken first, and only for
/// a running engine (no engine, no capture): a press already released while no capture existed is
/// refused and sends nothing.
pub fn send_capture_warmup(token: &PendingStart) {
    let running = engine_sidecar().is_some_and(|sidecar| sidecar.supervisor.is_running());
    if token.capture_attempt().is_none() || !running || !token.begin_capture() {
        return;
    }
    send_warmup_frame(token.attempt(), false);
}

/// The press `attempt` ended without a recording (the start failed, or a cancel or stop reached it
/// first): close the attempt so the engine pauses the warm stream now instead of at its bound,
/// refuses any warm-up of it still in flight, and refuses a start of it that is still in flight.
/// Nothing waits on it.
pub fn revoke_capture_warmup(attempt: u64) {
    send_warmup_frame(attempt, true);
}

/// How long a stop waits for the engine's acknowledgement of the capture seal before it goes on
/// without it (the engine's stop then seals the recording itself, at the moment it is processed).
const CAPTURE_SEAL_WAIT: Duration = Duration::from_millis(300);

/// The user released the shortcut (or otherwise asked to stop) the press whose recording is
/// pending or active: SEAL that recording now, synchronously, before the key-gated stop is built.
/// One `capture_seal` request bound to the press's attempt (and its session once the start
/// committed) goes through `EngineSupervisor::seal_capture` (no key gate, no config work, never
/// spawns an engine) and this call returns when the engine's reply says the worker applied it, on
/// the same ordered path the stop uses: no more audio appended, no more streaming partials, the
/// cut at the release instant; or after `CAPTURE_SEAL_WAIT`. The stop that follows therefore
/// finalizes a buffer that no longer grows, whatever the start, the gate or a key delivery still
/// have to do. The reply is `sealed` (active recording), `held` (its start has not reached the
/// worker yet: the start applies the release itself), `noop` (the press already ended) or
/// `unavailable`; a missing or late reply is logged and the stop proceeds (the worker's stop seals
/// itself first). Best effort and content-free. The attempt is the pending press's (a start in
/// flight, or committed with its reservation still held) or else the newest session's.
///
/// The release instant is taken first and travels in the frame (`release_age_ms`), so an engine
/// that applies the seal late still cuts at the release. When NO engine confirms the seal (none
/// running or one retiring, no reply in time, or an `unavailable` ack) the instant is kept on the
/// pending press (`PendingStart::note_unsealed_release`): a start that has not begun capture then
/// refuses to record at all (`PendingStart::begin_capture`), and one that has applies the instant
/// as the cutoff before its deferred stop (`seal_capture_at_recorded_release`).
pub fn seal_capture_for_stop() -> bool {
    seal_capture_for_stop_at(OriginalReleaseMark::for_current_action())
}

/// Seal at the caller's original native release marker; asynchronous callers use this seam.
pub fn seal_capture_for_stop_at(released: OriginalReleaseMark) -> bool {
    let token = pending_start_token();
    let target = match &token {
        Some(token) => Some((token.capture_attempt(), token.committed_session())),
        None => SESSION_START_ATTEMPT
            .lock()
            .ok()
            .and_then(|slot| slot.clone())
            .map(|(session, attempt)| (attempt, Some(session))),
    };
    let Some(target) = target else {
        return true;
    };
    let Some(attempt) = target.0 else {
        if released.edge.is_some() {
            abort_unsealed_capture(&token, target.1.as_deref());
            return false;
        }
        return true;
    };
    let status = send_capture_seal((attempt, target.1.clone()), released.clone());
    // `held` means the engine has no recording of this press yet and keeps the release for the
    // start frame. That is only a confirmation when the start frame is on its way (capture
    // begun); with no capture begun there is no audio of this press at all, so the press is
    // treated like an unconfirmed release and its start refuses to record.
    let confirmed = match status.as_deref() {
        Some("sealed") | Some("noop") => true,
        Some("held") => token.as_ref().is_none_or(|token| token.capture_begun()),
        _ => false,
    };
    if !confirmed {
        if let Some(token) = &token {
            token.note_unsealed_release(released.clone());
        }
    }
    // Pending attempts keep their original edge for replay after commit. Committed sessions
    // with unconfirmed exact seals are cancelled before any successful stop can be sent.
    if !confirmed
        && token
            .as_ref()
            .is_none_or(|token| token.committed_session().is_some())
    {
        abort_unsealed_capture(&token, target.1.as_deref());
        return false;
    }
    true
}

fn abort_unsealed_capture(token: &Option<Arc<PendingStart>>, session: Option<&str>) {
    if let Some(token) = token {
        let _ = token.cancel();
        revoke_capture_warmup(token.attempt());
    }
    if let Some(session) = session {
        let prepared = crate::runtime_lock::runtime_lock()
            .ok()
            .and_then(|runtime| runtime.prepare_possibly_started_cancel(session));
        if let Some(prepared) = prepared {
            prepared.send_possibly_started();
        }
    }
}

/// The pending press's original release could not reach the engine when it happened (see
/// [`seal_capture_for_stop`]) but capture had begun: seal the committed session NOW at that
/// original instant (`release_age_ms` = the time since it), BEFORE the press's deferred stop is
/// dispatched. The worker's seal is idempotent, so a seal that was applied meanwhile wins and this
/// is a no-op ack. An unconfirmed exact replay cancels this session & refuses finalization.
pub fn seal_capture_at_recorded_release(token: &PendingStart) -> bool {
    let Some(released) = token.unsealed_release() else {
        return true;
    };
    let status = send_capture_seal((token.attempt(), token.committed_session()), released);
    if !matches!(
        status.as_deref(),
        Some("sealed") | Some("held") | Some("noop")
    ) {
        crate::telemetry::log_message(
            "capture_seal_reapply_unconfirmed",
            "phase=before_deferred_stop",
        );
        abort_unsealed_capture(&None, token.committed_session().as_deref());
        return false;
    }
    true
}

/// The pending start's token, if a start is in flight (or committed with its reservation held).
fn pending_start_token() -> Option<Arc<PendingStart>> {
    PENDING_START
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(Arc::clone)
}

/// Send one `capture_seal` for `(attempt, session)` cut at `released`, and wait (bounded) for the
/// engine's ack status (`sealed`, `held`, `noop`, `unavailable`, ...); `None` when no engine
/// answered (no sidecar, not running, no reply in time).
fn send_capture_seal(
    target: (u64, Option<String>),
    released: OriginalReleaseMark,
) -> Option<String> {
    let (attempt, session) = target;
    let sidecar = engine_sidecar()?;
    let supervisor = Arc::clone(&sidecar.supervisor);
    if !supervisor.is_running() {
        return None;
    }
    let request_id = format!("capture_seal:{}", supervisor.next_request_id());
    let mut frame = engine_supervisor::build_request(
        &request_id,
        EngineSchemaName::CaptureSeal,
        session,
        Some(EnginePayload::CaptureSeal {
            start_attempt_id: attempt,
            release_age_ms: Some(
                u64::try_from(released.at.elapsed().as_millis()).unwrap_or(u64::MAX),
            ),
            release_edge: released.edge,
        }),
    );
    frame.start_attempt_id = Some(attempt);
    let exact = matches!(
        &frame.payload,
        Some(EnginePayload::CaptureSeal {
            release_edge: Some(_),
            ..
        })
    );
    let began = Instant::now();
    match supervisor.seal_capture(&frame, CAPTURE_SEAL_WAIT) {
        Ok(reply) => {
            let mut status = match reply.payload {
                Some(EnginePayload::EngineAck { status, .. }) => status,
                _ => "no_ack".to_string(),
            };
            if exact {
                status = match status.as_str() {
                    "sealed_exact" => "sealed",
                    "held_exact" => "held",
                    "noop_exact" => "noop",
                    _ => "unconfirmed_exact",
                }
                .to_string();
            }
            crate::telemetry::log_message(
                "capture_seal",
                &format!("status={status} elapsed_ms={}", began.elapsed().as_millis()),
            );
            Some(status)
        }
        Err(message) => {
            crate::telemetry::log_message("capture_seal_unconfirmed", &message);
            None
        }
    }
}

/// The session the runtime holds right now, read from the lock-free mirror (`None` before the
/// runtime opened or with no session). A release callback binds this before it hands the stop to
/// another thread.
pub fn active_session_lock_free() -> Option<String> {
    engine_sidecar().and_then(|sidecar| {
        sidecar
            .active_session
            .lock()
            .ok()
            .and_then(|session| session.clone())
    })
}

fn send_warmup_frame(attempt: u64, revoke: bool) {
    let Some(sidecar) = engine_sidecar() else {
        return;
    };
    let supervisor = Arc::clone(&sidecar.supervisor);
    if !supervisor.is_running() {
        return;
    }
    let label = if revoke { "warmup_revoke" } else { "warmup" };
    let request_id = format!("{label}:{}", supervisor.next_request_id());
    let mut frame = engine_supervisor::build_request(
        &request_id,
        EngineSchemaName::CaptureWarmup,
        None,
        Some(EnginePayload::CaptureWarmup {
            start_attempt_id: attempt,
            revoke,
        }),
    );
    frame.start_attempt_id = Some(attempt);
    // Off the caller's thread: the send waits for the engine's accepted reply, and neither the
    // start's shell work nor a cancel may queue behind it. A failed spawn resumed nothing and is
    // harmless.
    let spawned = std::thread::Builder::new()
        .name("capture-warmup".to_string())
        .spawn(move || {
            if let Err(message) = supervisor.send_capture_control(&frame) {
                crate::telemetry::log_message("capture_warmup_send_failed", &message);
            }
        });
    if let Err(error) = spawned {
        crate::telemetry::log_message("capture_warmup_thread_failed", &error.to_string());
    }
}

/// Lock-free `RuntimeApp::app_state()` for the sidecar runtime. `None` when
/// there is no published sidecar handle (the runtime has not
/// been opened yet) — callers fall back to `runtime_lock()?.app_state()`.
pub fn app_state_lock_free() -> Option<AppState> {
    engine_sidecar().map(|sidecar| sidecar.state.get())
}

/// `true` while `state` is still the authoritative runtime state. With no
/// authority (pre-init) no racing writer exists and
/// emission proceeds, preserving pre-guard behavior. The caller must hold the
/// emission critical section — pairing this check with the emit is what makes
/// terminal-write-wins atomic. A bare read without that serialization is the
/// non-atomic reread the 2026-10-01 review rejected.
pub fn state_is_current(state: &AppState) -> bool {
    engine_sidecar().is_none_or(|sidecar| sidecar.state.emission_permits(state))
}

/// Lock-free `RuntimeApp::engine_info()` for the sidecar runtime
/// (audit §2.1 — the Hub footer's `get_engine_info` must never queue behind
/// prewarm's model load or its under-lock config/vocab sync).
///
/// Semantics vs the old `EngineRuntime::engine_info`: identical fields
/// (`mode="sidecar"`, no cold-load timing, mirrored engine version). The old
/// version also *drove* readiness via `ensure_engine_ready` once onboarding
/// completed — including restarting a dead sidecar, which can block for the
/// full model load. That side effect is preserved asynchronously: a dead
/// sidecar gets a detached `ensure_started` kick while this query returns the
/// (stale-version) snapshot immediately. `None` before
/// `APP` init — callers fall back to the locked path.
pub fn engine_info_lock_free() -> Option<EngineInfoResult> {
    let sidecar = engine_sidecar()?;
    if crate::settings::onboarding_complete() && !sidecar.supervisor.is_running() {
        let sidecar = sidecar.clone();
        let _ = std::thread::Builder::new()
            .name("engine-info-restart".to_string())
            .spawn(move || {
                if let Err(err) = sidecar.ensure_started() {
                    crate::telemetry::log_message("engine_info_restart_error", &err);
                }
            });
    }
    let engine_version = sidecar
        .engine_version
        .lock()
        .ok()
        .and_then(|slot| slot.clone());
    Some(EngineInfoResult {
        mode: "sidecar".to_string(),
        cold_load_s: None,
        engine_version,
    })
}

/// Lock-free sibling of `RuntimeApp::sidecar_is_transcribing_session` — the
/// watchdog's "is the engine still decoding this session" probe. Bounded by
/// `SIDECAR_STATE_PROBE_TIMEOUT` on the supervisor's own channel, never by
/// the runtime lock. `None` = no sidecar runtime → caller falls back.
pub fn sidecar_is_transcribing_session_lock_free(session_id: &str) -> Option<Result<bool, String>> {
    engine_sidecar().map(|sidecar| sidecar.is_transcribing_session(session_id))
}

/// Lock-free watchdog revocation (#149). `None` = no sidecar runtime.
pub fn sidecar_revoke_session_lock_free(
    session_id: &str,
) -> Option<engine_supervisor::RevocationOutcome> {
    engine_sidecar().map(|sidecar| sidecar.revoke_session_acked(session_id))
}

/// Whether the audio-length-scaled final-transcript wait (#159) for this
/// session has run out. `Some(false)` while armed and in budget; `None` when
/// no wait is armed (or no sidecar runtime) so the caller uses its fixed cap.
pub fn sidecar_final_wait_expired_lock_free(session_id: &str) -> Option<bool> {
    engine_sidecar().and_then(|sidecar| {
        sidecar
            .supervisor
            .final_wait_armed(session_id)
            .then(|| sidecar.supervisor.final_wait_expired(session_id))
    })
}

/// Staged prewarm, half 1 (audit §2.1): process spawn + ready-wait + the
/// `EngineInfo` handshake — the parts that can take the full model-load time —
/// WITHOUT the global runtime lock. `None` before `APP` init.
///
/// Half 2 is `RuntimeApp::sync_sidecar_runtime_snapshot_if_running` under a
/// short `runtime_lock()` — it only pushes the cached config/vocab diff.
pub fn prewarm_handshake_lock_free() -> Option<Result<(), String>> {
    engine_sidecar().map(|sidecar| sidecar.warm_handshake())
}

/// Generation of the current Error for the pill's expiry timer (`None` before `APP` init).
pub fn terminal_error_generation_lock_free() -> Option<u64> {
    engine_sidecar().map(|sidecar| sidecar.state.error_generation())
}
