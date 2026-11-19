//! Streaming insertion: type the engine's stable words into the focused field while recording,
//! then reconcile with the final transcript.
//!
//! The engine only publishes committed partials while streaming is on, and each partial is the
//! full committed text so far (append-only). This side types the new suffix.
//!
//! Every write (typed chunk, reconcile backspaces, cancel-erase, Enter) is built first
//! (`gated_writes::prepare_*`, CPU only) and then goes through the central evidence gate
//! (`evidence_gate::authorize_with_input`) immediately before it is sent: a FRESH read of the
//! focused control compared with the control streaming bound at the first chunk, a FRESH
//! secure-field read, a deadline, and LAST a sample of the user's physical modifier keys. Only
//! (same control, confirmed not secure, in time, modifiers sampled up) writes; unknown, error,
//! timeout, a changed control, a held modifier and an unreadable modifier state refuse. On macOS the
//! foreground application is also observed from the OS (bundle identifier, never a window title):
//! classified against the host's `AppTargetPolicy` and bound at the first chunk, then observed
//! again inside every authorization after the control read and before the modifier sample; a
//! terminal, an unclassified application, an unknown or error observation and a changed identity
//! refuse and stop streaming. Nothing
//! runs between the authorization and the effect-only send: the batch already exists, and the
//! permit's expiry is checked once more at redemption (`gated_writes`). A refused chunk stops
//! streaming for the session (it never types again). A refused erase leaves the typed text in
//! place, and `take_for_final` says so with an explicit [`ReconcileOutcome`]; the host must not
//! deliver the full transcript on top of text that is still in the field.
//!
//! Host inputs: [`StreamingHost`], passed to every call. The host supplies the secure-field probe,
//! the fresh focused-control probe, the probe budget and the event sink; the keystroke
//! primitives are reached only through `gated_writes`.

mod reconcile;

pub use reconcile::reconcile_point;

use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::evidence_gate::{
    authorize_with_input, control_match, AppIdentity, AppRefusal, AppTargetPolicy, ControlId,
    ControlMatch, IdentityLevel, Refusal, WriteKind, WritePermit,
};
use crate::gated_writes::{self, SendOutcome};

/// Per-write cost bounds of streaming insertion.
#[derive(Clone, Copy, Debug)]
pub struct StreamingConfig {
    /// Budget for one gate decision: the secure read, the control read, the modifier sample and
    /// the write that follows. A slower probe refuses (stops streaming).
    pub probe_budget_ms: u64,
    /// Shortest time between two gated chunk attempts. A skipped attempt types nothing and is
    /// not a refusal: the next partial carries the same delta again. This is the batching rule
    /// that keeps probe cost bounded however fast partials arrive.
    pub min_write_interval_ms: u64,
}

/// What the host supplies to streaming insertion.
#[derive(Clone, Copy)]
pub struct StreamingHost {
    /// Security state of the field focused in `destination` (macOS: the bound process id;
    /// Windows: the bound top-level window handle): `Some(false)` only when positively known
    /// non-secure. `Some(true)` and `None` both refuse.
    pub destination_security: fn(destination: isize) -> Option<bool>,
    /// A fresh read of the focused control, at least as strong as `want` where the platform can
    /// (`paste_delivery::focused_control_id` is the implementation). `None` when unreadable.
    pub focused_control: fn(want: IdentityLevel) -> Option<ControlId>,
    /// Event sink `(event, detail)`; carries outcome codes and counts only, never typed text.
    pub event_sink: fn(&str, &str),
    pub config: StreamingConfig,
    /// Windows only. The host's terminal list (executable names, matched case-insensitively; the
    /// same list as `DeliveryConfig::wezterm_gui_process_names`). Streaming identifies a terminal
    /// by the process of the fresh foreground window and NEVER types into one, under any
    /// terminal-pane policy: a streamed chunk has no bound pane that is re-confirmed per write.
    /// An unreadable process refuses too. Not consulted on macOS (see `app_policy`).
    pub terminal_process_names: &'static [&'static str],
    /// macOS only (the host still sets it; the same value as
    /// `DeliveryConfig::macos_app_policy`). The foreground application is observed from the OS at
    /// the first chunk and again inside every authorization. Streaming writes only into an
    /// application the policy lists for streaming: a terminal, an application on neither list, an
    /// unknown or error observation and a changed identity all refuse and stop streaming for the
    /// session (`evidence_app_terminal`, `evidence_app_unclassified`,
    /// `evidence_app_identity_unknown`, `evidence_app_identity_error`,
    /// `evidence_app_identity_changed`). Not consulted on Windows.
    pub app_policy: &'static AppTargetPolicy,
}

/// What `take_for_final` did to the field, as a closed set. The host decides what to deliver from
/// this and from nothing else: only [`ReconcileOutcome::NothingWritten`] and
/// [`ReconcileOutcome::Reconciled`] allow an automatic delivery.
///
/// Deliberately not `Debug`: `Reconciled` carries transcript text, so a `{:?}` would put content
/// in a log. Log [`ReconcileOutcome::tag`].
pub enum ReconcileOutcome {
    /// The streamed text now equals a prefix of the final transcript. `remainder` is the rest
    /// (possibly empty) and is what the host delivers through the normal gated delivery. When it
    /// is empty, [`press_enter`] may submit.
    Reconciled { remainder: String },
    /// This session wrote nothing into any field (no stream, a stream of another session, or no
    /// chunk was ever typed, and no send failed). The host delivers the whole transcript through
    /// the normal gated delivery, once.
    NothingWritten,
    /// Streaming stopped and text it typed is still in the field. `remaining_chars` is exactly
    /// the number of streamed characters left there (the erase batches that did go through are
    /// subtracted). `cause` is a stable tag (a gate refusal code, `destination_moved`,
    /// `batch_not_preparable`, `permit_expired`). The host must not deliver the transcript
    /// automatically and must not press Return: it would duplicate or append to text already
    /// typed. The full transcript stays with the host for manual recovery.
    StoppedWritesRemain {
        remaining_chars: usize,
        cause: &'static str,
    },
    /// Streaming stopped and how much of its text is in the field cannot be told: a send
    /// reported failure after the batch was handed to the OS (it may have been applied in part),
    /// the binding is missing for typed text, or the streaming state is unavailable. Same host
    /// rule as [`ReconcileOutcome::StoppedWritesRemain`].
    StoppedCountUncertain { cause: &'static str },
}

impl ReconcileOutcome {
    /// A stable, content-free tag for logs and events.
    pub fn tag(&self) -> &'static str {
        match self {
            Self::Reconciled { .. } => "reconciled",
            Self::NothingWritten => "nothing_written",
            Self::StoppedWritesRemain { .. } => "stopped_writes_remain",
            Self::StoppedCountUncertain { .. } => "stopped_count_uncertain",
        }
    }
}

/// What streaming binds at the first chunk: the focused control and, on macOS, the foreground
/// application observed from the OS at that moment (`None` on Windows, where a terminal is
/// identified by the process-name list instead).
#[derive(Clone)]
struct Binding {
    control: ControlId,
    app: Option<AppIdentity>,
}

struct Stream {
    session_id: String,
    /// Committed engine text already typed.
    typed: String,
    /// The control and application bound at the first keystroke.
    bound: Option<Binding>,
    /// A gate refused, a keystroke failed, or the partial was not append-only.
    stopped: bool,
    /// A typed batch was handed to the OS and reported failure: part of it may be in the field,
    /// so `typed` is no longer a count of what is there.
    uncertain: bool,
    last_attempt: Option<Instant>,
}

struct Slots {
    stream: Mutex<Option<Stream>>,
    /// The binding the last reconcile typed into, for the Enter that follows it.
    enter: Mutex<Option<Binding>>,
}

static SLOTS: Slots = Slots {
    stream: Mutex::new(None),
    enter: Mutex::new(None),
};

/// Everything the streaming logic does to the outside world, so the logic is testable with a
/// fake and the real implementation is one place. `Batch` is a built, unsent input batch.
trait StreamIo {
    type Batch;
    fn fresh_control(&self, want: IdentityLevel) -> Option<ControlId>;
    /// Windows: is the window of `control` a terminal (host process-name list)? `Some(true)`
    /// terminal, `Some(false)` not, `None` unknown (refuses). Streaming never writes into a
    /// terminal. Consulted only while no application identity is bound (`Binding::app` is
    /// `None`, i.e. Windows). No default: an implementation states its answer.
    fn terminal_state(&self, control: &ControlId) -> Option<bool>;
    /// Capture (first chunk): observe the foreground application now from the OS and classify it
    /// for streaming. `Ok(Some(identity))` to bind on macOS; `Err` refuses (terminal,
    /// unclassified, unknown, error); `Ok(None)` only where terminals are identified by
    /// `terminal_state` (Windows). No default.
    fn capture_app(&self) -> Result<Option<AppIdentity>, AppRefusal>;
    /// Re-observe the foreground application and compare it with `bound`. On macOS a missing
    /// binding refuses; `Ok(())` for no binding is Windows only. No default.
    fn recheck_app(&self, bound: Option<&AppIdentity>) -> Result<(), AppRefusal>;
    fn secure(&self, control: &ControlId) -> Option<bool>;
    /// The modifier sample: `Some(true)` all physical modifiers up, `Some(false)` one held, `None`
    /// unreadable (refuses).
    fn input_quiet(&self) -> Option<bool>;
    /// Wait (bounded) for the stop hotkey's physical modifiers to lift before Return is
    /// authorized; `false` when a key is still down at the cap. No default: an implementation
    /// states whether it has anything to wait for.
    fn settle_modifiers(&self) -> bool;
    /// Build the batches. CPU only; called BEFORE any evidence is sampled. `None` means it cannot
    /// be built, so nothing is authorized or sent.
    fn prepare_type(&self, text: &str) -> Option<Self::Batch>;
    fn prepare_backspaces(&self, count: usize) -> Option<Self::Batch>;
    fn prepare_enter(&self) -> Option<Self::Batch>;
    /// Redeem the permit and send a built batch; nothing else.
    fn type_batch(&self, permit: WritePermit, batch: &Self::Batch) -> SendOutcome;
    fn backspace_batch(&self, permit: WritePermit, batch: &Self::Batch) -> SendOutcome;
    fn enter_batch(&self, permit: WritePermit, batch: &Self::Batch) -> bool;
    fn emit(&self, event: &str, detail: &str);
    fn config(&self) -> StreamingConfig;
}

struct OsIo<'a> {
    host: &'a StreamingHost,
}

impl StreamIo for OsIo<'_> {
    type Batch = gated_writes::Prepared;

    fn fresh_control(&self, want: IdentityLevel) -> Option<ControlId> {
        (self.host.focused_control)(want)
    }
    fn terminal_state(&self, control: &ControlId) -> Option<bool> {
        #[cfg(target_os = "windows")]
        {
            crate::paste_delivery::window_terminal_state(
                self.host.terminal_process_names,
                control.window,
            )
        }
        #[cfg(not(target_os = "windows"))]
        {
            // Never consulted off Windows: macOS binds an application identity and the app
            // policy decides; any other platform has no focused control to gate on. Unknown, so
            // that no path can read this as "not a terminal".
            let _ = control;
            None
        }
    }
    fn capture_app(&self) -> Result<Option<AppIdentity>, AppRefusal> {
        #[cfg(target_os = "windows")]
        {
            Ok(None)
        }
        #[cfg(not(target_os = "windows"))]
        {
            crate::evidence_gate::classify_observation(
                self.host.app_policy,
                crate::paste_delivery::foreground_app_observation(),
                crate::evidence_gate::AppUse::Streaming,
            )
            .map(Some)
        }
    }
    fn recheck_app(&self, bound: Option<&AppIdentity>) -> Result<(), AppRefusal> {
        #[cfg(target_os = "windows")]
        {
            let _ = bound;
            Ok(())
        }
        #[cfg(not(target_os = "windows"))]
        {
            match bound {
                Some(bound) => crate::evidence_gate::recheck_observation(
                    self.host.app_policy,
                    bound,
                    crate::paste_delivery::foreground_app_observation(),
                    crate::evidence_gate::AppUse::Streaming,
                ),
                None => Err(AppRefusal::IdentityUnknown),
            }
        }
    }
    fn secure(&self, control: &ControlId) -> Option<bool> {
        #[cfg(target_os = "macos")]
        let destination = control.process_id as isize;
        #[cfg(not(target_os = "macos"))]
        let destination = control.window;
        (self.host.destination_security)(destination)
    }
    fn input_quiet(&self) -> Option<bool> {
        gated_writes::modifiers_quiet()
    }
    fn settle_modifiers(&self) -> bool {
        // The cap is the host's per-write probe budget: the wait comes first, before the gate.
        #[cfg(target_os = "windows")]
        {
            !gated_writes::settle_modifiers(self.host.config.probe_budget_ms).still_held
        }
        #[cfg(not(target_os = "windows"))]
        {
            // No modifier wait exists off Windows. On macOS the gate's own sample
            // (`modifiers_quiet`, the HID modifier state) refuses a held key or an unobservable
            // state (no Input Monitoring permission, Secure Input) before anything is sent; on
            // other platforms it is `None` and refuses.
            true
        }
    }
    fn prepare_type(&self, text: &str) -> Option<Self::Batch> {
        gated_writes::prepare_type_text(text)
    }
    fn prepare_backspaces(&self, count: usize) -> Option<Self::Batch> {
        gated_writes::prepare_backspaces(count)
    }
    fn prepare_enter(&self) -> Option<Self::Batch> {
        gated_writes::prepare_enter()
    }
    fn type_batch(&self, permit: WritePermit, batch: &Self::Batch) -> SendOutcome {
        gated_writes::type_prepared(permit, batch)
    }
    fn backspace_batch(&self, permit: WritePermit, batch: &Self::Batch) -> SendOutcome {
        gated_writes::backspaces_prepared(permit, batch)
    }
    fn enter_batch(&self, permit: WritePermit, batch: &Self::Batch) -> bool {
        gated_writes::enter_prepared(permit, batch)
    }
    fn emit(&self, event: &str, detail: &str) {
        (self.host.event_sink)(event, detail);
    }
    fn config(&self) -> StreamingConfig {
        self.host.config
    }
}

/// A fresh comparison with the binding. Windows (no application identity bound): the freshly read
/// control's window is matched against the host's terminal list, and a terminal (or an unreadable
/// process) is never `Same`. macOS: the control is compared, and then the foreground application
/// is observed from the OS again and must pass the host's streaming policy and be the bound
/// identity; that observation is the last sample before the modifier keys. Streaming has no bound
/// pane, so there is no terminal it may write into.
fn fresh_state<I: StreamIo>(io: &I, bound: &Binding) -> ControlMatch {
    let fresh = io.fresh_control(bound.control.level);
    if bound.app.is_none() {
        if let Some(fresh) = fresh.as_ref() {
            match io.terminal_state(fresh) {
                Some(false) => {}
                Some(true) => return ControlMatch::TerminalUnverified,
                None => return ControlMatch::Unknown,
            }
        }
    }
    let matched = control_match(&bound.control, fresh.as_ref());
    if matched != ControlMatch::Same {
        return matched;
    }
    match io.recheck_app(bound.app.as_ref()) {
        Ok(()) => ControlMatch::Same,
        Err(refusal) => ControlMatch::App(refusal),
    }
}

/// The gate for one streaming write against the binding: secure read, control and application
/// read, then the modifier sample last. The caller has already built the batch; the next thing it
/// does with the permit is the effect-only send.
fn gate<I: StreamIo>(io: &I, kind: WriteKind, bound: &Binding) -> Result<WritePermit, Refusal> {
    authorize_with_input(
        kind,
        Duration::from_millis(io.config().probe_budget_ms),
        || io.secure(&bound.control),
        || fresh_state(io, bound),
        || io.input_quiet(),
    )
}

/// Why an erase stopped before everything was removed.
#[derive(Clone, Copy)]
enum EraseStop {
    /// Nothing of the failed batch was sent (a gate refusal, an unbuildable batch, an expired
    /// permit): the count of characters erased so far is exact.
    Certain(&'static str),
    /// A send reported failure after the batch was handed to the OS: it may have removed some of
    /// its characters, so the count is unknown.
    Uncertain(&'static str),
}

impl EraseStop {
    fn code(self) -> &'static str {
        match self {
            EraseStop::Certain(code) | EraseStop::Uncertain(code) => code,
        }
    }
}

/// Backspaces in batches of at most `gated_writes::MAX_UNITS_PER_BATCH`. Each batch is built
/// first, then passes its own gate (one authorization per prepared batch), then is sent. Returns
/// the characters erased by batches that were sent and why it stopped early, if it did. Erasing
/// is from the caret backwards, so a stop part-way leaves a shorter streamed text in place and
/// nothing else, unless the stop was `Uncertain`.
fn erase_in_batches<I: StreamIo>(
    io: &I,
    kind: WriteKind,
    bound: &Binding,
    count: usize,
) -> (usize, Result<(), EraseStop>) {
    let mut erased = 0usize;
    while erased < count {
        let batch_len = (count - erased).min(gated_writes::MAX_UNITS_PER_BATCH);
        let Some(batch) = io.prepare_backspaces(batch_len) else {
            return (erased, Err(EraseStop::Certain("batch_not_preparable")));
        };
        match gate(io, kind, bound) {
            Ok(permit) => match io.backspace_batch(permit, &batch) {
                SendOutcome::Sent => erased += batch_len,
                SendOutcome::NotSent => return (erased, Err(EraseStop::Certain("permit_expired"))),
                SendOutcome::Failed => {
                    return (erased, Err(EraseStop::Uncertain("keystroke_failed")))
                }
            },
            Err(refusal) => return (erased, Err(EraseStop::Certain(refusal.code()))),
        }
    }
    (erased, Ok(()))
}

/// The first chunk binds the foreground application (macOS) and the control focused now
/// (strongest identity the platform gives), then passes the same gate as every later write, so
/// the secure read, a second control read and a second application observation all happen after
/// the binding reads. The application is classified FIRST, so a terminal or an unclassified
/// application refuses with its own tag whatever the control read would say.
fn bind_and_gate<I: StreamIo>(io: &I, kind: WriteKind) -> Result<(Binding, WritePermit), Refusal> {
    let app = io.capture_app().map_err(Refusal::App)?;
    let Some(control) = io.fresh_control(IdentityLevel::Element) else {
        return Err(Refusal::ControlUnknown);
    };
    // Streaming never binds on window and process alone: it needs a control (focus handle, AX
    // element or UIA element) it can tell apart from its neighbours in the same window.
    if control.level < IdentityLevel::Control {
        return Err(Refusal::ControlUnknown);
    }
    // The observed application and the focused control must be the same process.
    if app
        .as_ref()
        .is_some_and(|app| app.pid() != control.process_id)
    {
        return Err(Refusal::App(AppRefusal::IdentityChanged));
    }
    let bound = Binding { control, app };
    let permit = gate(io, kind, &bound)?;
    Ok((bound, permit))
}

/// A committed-text partial for the recording `session_id`. Types whatever
/// part of `committed` has not been typed yet.
pub fn on_committed(host: &StreamingHost, session_id: &str, committed: &str) {
    on_committed_with(&OsIo { host }, &SLOTS, session_id, committed);
}

fn on_committed_with<I: StreamIo>(io: &I, slots: &Slots, session_id: &str, committed: &str) {
    let Ok(mut slot) = slots.stream.lock() else {
        return;
    };
    if slot
        .as_ref()
        .is_none_or(|stream| stream.session_id != session_id)
    {
        *slot = Some(Stream {
            session_id: session_id.to_string(),
            typed: String::new(),
            bound: None,
            stopped: false,
            uncertain: false,
            last_attempt: None,
        });
        // A new session's Enter binding is its own.
        if let Ok(mut enter) = slots.enter.lock() {
            *enter = None;
        }
    }
    let Some(stream) = slot.as_mut() else {
        return;
    };
    if stream.stopped {
        return;
    }
    let Some(delta) = committed.strip_prefix(stream.typed.as_str()) else {
        // Committed text is append-only; anything else is a protocol fault.
        stream.stopped = true;
        io.emit("streaming_insertion_stopped", "reason=non_append_partial");
        return;
    };
    if delta.is_empty() {
        return;
    }
    // A held push-to-talk hotkey would turn typed characters into shortcuts. Only a positive
    // "held" defers here (the next partial carries this delta again); an unreadable state goes to
    // the gate, which refuses it.
    if io.input_quiet() == Some(false) {
        return;
    }
    // Batching rule: at most one gated attempt per interval. The delta is not lost; the next
    // partial carries it again.
    let min_interval = Duration::from_millis(io.config().min_write_interval_ms);
    if stream
        .last_attempt
        .is_some_and(|at| at.elapsed() < min_interval)
    {
        return;
    }
    stream.last_attempt = Some(Instant::now());
    // One authorization covers one prepared batch (at most `MAX_UNITS_PER_BATCH` UTF-16 units). A
    // longer delta is typed batch by batch across partials: the part not typed stays in the
    // delta and the next partial carries it, each time through its own gate pass.
    let take = gated_writes::typed_prefix_len(delta);
    if take == 0 {
        stream.stopped = true;
        io.emit("streaming_insertion_stopped", "reason=batch_not_preparable");
        return;
    }
    let delta = &delta[..take];

    // BUILD before anything is sampled: the control binding read, the secure read, the control
    // read and the modifier sample all come after this, and nothing but the redemption and the
    // send comes after them.
    let Some(batch) = io.prepare_type(delta) else {
        stream.stopped = true;
        io.emit("streaming_insertion_stopped", "reason=batch_not_preparable");
        return;
    };

    let permit = match stream.bound.clone() {
        Some(bound) => gate(io, WriteKind::StreamChunk, &bound),
        None => bind_and_gate(io, WriteKind::StreamChunk).map(|(bound, permit)| {
            stream.bound = Some(bound);
            permit
        }),
    };
    let permit = match permit {
        Ok(permit) => permit,
        Err(refusal) => {
            stream.stopped = true;
            io.emit(
                "streaming_insertion_stopped",
                &format!("reason=evidence_{}", refusal.code()),
            );
            return;
        }
    };
    match io.type_batch(permit, &batch) {
        SendOutcome::Sent => {
            stream.typed.push_str(delta);
            io.emit(
                "streaming_insertion_typed",
                &format!(
                    "session={session_id} chars={} total_chars={}",
                    delta.chars().count(),
                    stream.typed.chars().count()
                ),
            );
        }
        SendOutcome::NotSent => {
            // The permit expired at redemption: nothing was typed.
            stream.stopped = true;
            io.emit("streaming_insertion_stopped", "reason=permit_expired");
        }
        SendOutcome::Failed => {
            // The batch reached the OS and reported failure: part of it may be in the field.
            stream.stopped = true;
            stream.uncertain = true;
            io.emit(
                "streaming_insertion_stopped",
                "reason=keystroke_failed count_uncertain=true",
            );
        }
    }
}

/// The final transcript for `session_id` is about to be delivered. When text was streamed into
/// the same control, remove the streamed text that diverges from `final_text` and say what is
/// left. The result is an explicit [`ReconcileOutcome`]; there is no "none" that a caller can
/// read as "deliver everything":
///
/// * `Reconciled`: deliver the remainder normally (its own gate decides between paste and copy).
/// * `NothingWritten`: this session wrote nothing; deliver the whole transcript normally.
/// * `StoppedWritesRemain` / `StoppedCountUncertain`: streamed text is, or may be, still in the
///   field. Deliver nothing automatically and press no Return; keep the full transcript for
///   manual recovery.
pub fn take_for_final(
    host: &StreamingHost,
    session_id: &str,
    final_text: &str,
) -> ReconcileOutcome {
    take_for_final_with(&OsIo { host }, &SLOTS, session_id, final_text)
}

fn take_for_final_with<I: StreamIo>(
    io: &I,
    slots: &Slots,
    session_id: &str,
    final_text: &str,
) -> ReconcileOutcome {
    let Ok(mut slot) = slots.stream.lock() else {
        // The streaming state is unavailable, so what it typed is unknown.
        io.emit("streaming_insertion_final", "outcome=state_unavailable");
        return ReconcileOutcome::StoppedCountUncertain {
            cause: "state_unavailable",
        };
    };
    let Some(stream) = slot.take() else {
        return ReconcileOutcome::NothingWritten;
    };
    drop(slot);
    if let Ok(mut enter) = slots.enter.lock() {
        *enter = None;
    }
    if stream.session_id != session_id {
        return ReconcileOutcome::NothingWritten;
    }
    if stream.uncertain {
        io.emit("streaming_insertion_final", "outcome=count_uncertain");
        return ReconcileOutcome::StoppedCountUncertain {
            cause: "typed_batch_failed",
        };
    }
    if stream.typed.is_empty() {
        return ReconcileOutcome::NothingWritten;
    }
    let typed_chars = stream.typed.chars().count();
    let Some(bound) = stream.bound.clone() else {
        // Text was typed but no control is bound: where it went is unknown.
        io.emit("streaming_insertion_final", "outcome=count_uncertain");
        return ReconcileOutcome::StoppedCountUncertain {
            cause: "no_binding",
        };
    };
    let common_bytes = reconcile_point(&stream.typed, final_text);
    let erase = stream.typed[common_bytes..].chars().count();
    if erase > 0 {
        // The gate re-reads the control, the secure state and the modifiers itself, once per batch.
        let (erased, stop) = erase_in_batches(io, WriteKind::StreamBackspace, &bound, erase);
        match stop {
            Ok(()) => {}
            Err(EraseStop::Uncertain(code)) => {
                io.emit(
                    "streaming_insertion_final",
                    &format!("outcome=erase_failed_{code} count_uncertain=true"),
                );
                return ReconcileOutcome::StoppedCountUncertain { cause: code };
            }
            Err(EraseStop::Certain(code)) => {
                let remaining_chars = typed_chars.saturating_sub(erased);
                io.emit(
                    "streaming_insertion_final",
                    &format!(
                        "outcome=erase_refused_{code} partial_erased_chars={erased} remaining_chars={remaining_chars}"
                    ),
                );
                return ReconcileOutcome::StoppedWritesRemain {
                    remaining_chars,
                    cause: code,
                };
            }
        }
    } else {
        let moved = fresh_state(io, &bound);
        if moved != ControlMatch::Same {
            // Nothing to erase, but the remainder must not be delivered as a continuation of text
            // typed into a control (or an application) that is no longer focused or no longer
            // permitted, and the streamed text stays where it is.
            let cause = match moved {
                ControlMatch::App(refusal) => refusal.code(),
                _ => "destination_moved",
            };
            io.emit(
                "streaming_insertion_final",
                &format!("outcome=destination_moved cause={cause}"),
            );
            return ReconcileOutcome::StoppedWritesRemain {
                remaining_chars: typed_chars,
                cause,
            };
        }
    }
    if let Ok(mut enter) = slots.enter.lock() {
        *enter = Some(bound);
    }
    io.emit(
        "streaming_insertion_final",
        &format!(
            "outcome=reconciled streamed_chars={typed_chars} erased_chars={erase} remaining_chars={}",
            final_text[common_bytes..].chars().count()
        ),
    );
    ReconcileOutcome::Reconciled {
        remainder: final_text[common_bytes..].to_string(),
    }
}

/// The recording was cancelled: remove what was streamed, if the evidence gate confirms the
/// control is still the one it was typed into and is not a secure field. A refused or failed
/// erase leaves the typed text in place; the event carries how many characters were erased and
/// whether the count is uncertain, and nothing else is typed.
pub fn erase_on_cancel(host: &StreamingHost, session_id: &str) {
    erase_on_cancel_with(&OsIo { host }, &SLOTS, session_id);
}

fn erase_on_cancel_with<I: StreamIo>(io: &I, slots: &Slots, session_id: &str) {
    let Some(stream) = slots.stream.lock().ok().and_then(|mut slot| slot.take()) else {
        return;
    };
    if let Ok(mut enter) = slots.enter.lock() {
        *enter = None;
    }
    if stream.session_id != session_id || (stream.typed.is_empty() && !stream.uncertain) {
        return;
    }
    let count = stream.typed.chars().count();
    let (erased, why, uncertain) = match stream.bound.as_ref() {
        None => (false, "no_binding".to_string(), stream.uncertain),
        Some(_) if stream.uncertain => (false, "typed_batch_failed".to_string(), true),
        Some(bound) => match erase_in_batches(io, WriteKind::CancelErase, bound, count) {
            (_, Ok(())) => (true, "ok".to_string(), false),
            (0, Err(stop)) => (
                false,
                stop.code().to_string(),
                matches!(stop, EraseStop::Uncertain(_)),
            ),
            (erased, Err(stop)) => (
                false,
                format!("{}_after_{erased}", stop.code()),
                matches!(stop, EraseStop::Uncertain(_)),
            ),
        },
    };
    io.emit(
        "streaming_insertion_cancel",
        &format!("erased={erased} streamed_chars={count} detail={why} count_uncertain={uncertain}"),
    );
}

/// Submit when the streamed text already covers the whole final transcript. Requires the control
/// the last `take_for_final` reconciled into (a [`ReconcileOutcome::Reconciled`]), and a fresh
/// pass of the evidence gate with a real modifier sample; `false` when there is no such binding,
/// the batch cannot be built, or the gate refuses. A stopped or uncertain outcome never creates
/// that binding, so Return cannot follow one.
pub fn press_enter(host: &StreamingHost) -> bool {
    press_enter_with(&OsIo { host }, &SLOTS)
}

fn press_enter_with<I: StreamIo>(io: &I, slots: &Slots) -> bool {
    let Some(bound) = slots.enter.lock().ok().and_then(|mut enter| enter.take()) else {
        io.emit("streaming_insertion_enter", "outcome=refused_no_binding");
        return false;
    };
    // Everything that builds or waits comes first: the Return batch is built, then the stop
    // hotkey's modifiers are waited out, and only then does the gate sample (the modifier keys
    // last) and the send follow.
    let Some(batch) = io.prepare_enter() else {
        io.emit(
            "streaming_insertion_enter",
            "outcome=refused_batch_not_preparable",
        );
        return false;
    };
    if !io.settle_modifiers() {
        io.emit(
            "streaming_insertion_enter",
            &format!("outcome=refused_{}", Refusal::InputBusy.code()),
        );
        return false;
    }
    let result = authorize_with_input(
        WriteKind::Enter,
        Duration::from_millis(io.config().probe_budget_ms),
        || io.secure(&bound.control),
        || fresh_state(io, &bound),
        || io.input_quiet(),
    );
    match result {
        Ok(permit) => io.enter_batch(permit, &batch),
        Err(refusal) => {
            io.emit(
                "streaming_insertion_enter",
                &format!("outcome=refused_{}", refusal.code()),
            );
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;

    #[derive(Default)]
    struct Fake {
        /// Fresh control reads, consumed in order; the last one repeats.
        controls: RefCell<VecDeque<Option<ControlId>>>,
        secure: RefCell<VecDeque<Option<bool>>>,
        typed: RefCell<Vec<String>>,
        erased: RefCell<Vec<usize>>,
        enters: RefCell<usize>,
        events: RefCell<Vec<String>>,
        probe_delay: Duration,
        budget_ms: u64,
        interval_ms: u64,
        type_ok: bool,
        /// The modifier sample the fake returns.
        quiet: Option<bool>,
    }

    /// A built, unsent batch of the fake.
    enum FakeBatch {
        Type(String),
        Erase(usize),
        Enter,
    }

    fn control(runtime: i32) -> ControlId {
        ControlId::new(
            1,
            10,
            Some(20),
            Some(crate::evidence_gate::ElementKey {
                native_window: 20,
                runtime_id: vec![runtime],
            }),
        )
    }

    fn fake() -> Fake {
        Fake {
            budget_ms: 200,
            type_ok: true,
            quiet: Some(true),
            ..Fake::default()
        }
    }

    fn next<T: Clone>(queue: &RefCell<VecDeque<T>>) -> Option<T> {
        let mut queue = queue.borrow_mut();
        if queue.len() > 1 {
            queue.pop_front()
        } else {
            queue.front().cloned()
        }
    }

    impl StreamIo for Fake {
        type Batch = FakeBatch;
        fn fresh_control(&self, _want: IdentityLevel) -> Option<ControlId> {
            next(&self.controls).flatten()
        }
        fn terminal_state(&self, _control: &ControlId) -> Option<bool> {
            Some(false)
        }
        // The fake stands for the Windows shape: no application identity is bound.
        fn capture_app(&self) -> Result<Option<AppIdentity>, AppRefusal> {
            Ok(None)
        }
        fn recheck_app(&self, _bound: Option<&AppIdentity>) -> Result<(), AppRefusal> {
            Ok(())
        }
        fn secure(&self, _control: &ControlId) -> Option<bool> {
            std::thread::sleep(self.probe_delay);
            next(&self.secure).flatten()
        }
        fn input_quiet(&self) -> Option<bool> {
            self.quiet
        }
        fn settle_modifiers(&self) -> bool {
            true
        }
        fn prepare_type(&self, text: &str) -> Option<FakeBatch> {
            Some(FakeBatch::Type(text.to_string()))
        }
        fn prepare_backspaces(&self, count: usize) -> Option<FakeBatch> {
            Some(FakeBatch::Erase(count))
        }
        fn prepare_enter(&self) -> Option<FakeBatch> {
            Some(FakeBatch::Enter)
        }
        fn type_batch(&self, permit: WritePermit, batch: &FakeBatch) -> SendOutcome {
            assert!(permit.redeem(&[WriteKind::StreamChunk]).is_ok());
            let FakeBatch::Type(text) = batch else {
                return SendOutcome::Failed;
            };
            self.typed.borrow_mut().push(text.clone());
            if self.type_ok {
                SendOutcome::Sent
            } else {
                SendOutcome::Failed
            }
        }
        fn backspace_batch(&self, permit: WritePermit, batch: &FakeBatch) -> SendOutcome {
            assert!(permit
                .redeem(&[WriteKind::StreamBackspace, WriteKind::CancelErase])
                .is_ok());
            let FakeBatch::Erase(count) = batch else {
                return SendOutcome::Failed;
            };
            self.erased.borrow_mut().push(*count);
            SendOutcome::Sent
        }
        fn enter_batch(&self, permit: WritePermit, batch: &FakeBatch) -> bool {
            assert!(permit.redeem(&[WriteKind::Enter]).is_ok());
            assert!(matches!(batch, FakeBatch::Enter));
            *self.enters.borrow_mut() += 1;
            true
        }
        fn emit(&self, event: &str, detail: &str) {
            self.events.borrow_mut().push(format!("{event} {detail}"));
        }
        fn config(&self) -> StreamingConfig {
            StreamingConfig {
                probe_budget_ms: self.budget_ms,
                min_write_interval_ms: self.interval_ms,
            }
        }
    }

    fn slots() -> Slots {
        Slots {
            stream: Mutex::new(None),
            enter: Mutex::new(None),
        }
    }

    fn script(io: &Fake, controls: &[Option<ControlId>], secure: &[Option<bool>]) {
        *io.controls.borrow_mut() = controls.iter().cloned().collect();
        *io.secure.borrow_mut() = secure.iter().copied().collect();
    }

    /// The remainder of a `Reconciled` outcome; `None` for every other outcome.
    fn remainder(outcome: ReconcileOutcome) -> Option<String> {
        match outcome {
            ReconcileOutcome::Reconciled { remainder } => Some(remainder),
            _ => None,
        }
    }

    fn stopped(io: &Fake) -> bool {
        io.events
            .borrow()
            .iter()
            .any(|e| e.starts_with("streaming_insertion_stopped"))
    }

    #[test]
    fn types_only_after_the_gate_passes_and_keeps_typing_while_it_does() {
        let io = fake();
        let slots = slots();
        script(&io, &[Some(control(1))], &[Some(false)]);
        on_committed_with(&io, &slots, "s", "hello");
        on_committed_with(&io, &slots, "s", "hello world");
        assert_eq!(
            *io.typed.borrow(),
            vec!["hello".to_string(), " world".to_string()]
        );
        assert!(!stopped(&io));
    }

    #[test]
    fn unknown_security_on_the_first_chunk_types_nothing() {
        let io = fake();
        let slots = slots();
        script(&io, &[Some(control(1))], &[None]);
        on_committed_with(&io, &slots, "s", "hello");
        assert!(io.typed.borrow().is_empty());
        assert!(stopped(&io));
    }

    #[test]
    fn an_unreadable_control_on_the_first_chunk_types_nothing() {
        let io = fake();
        let slots = slots();
        script(&io, &[None], &[Some(false)]);
        on_committed_with(&io, &slots, "s", "hello");
        assert!(io.typed.borrow().is_empty());
        assert!(stopped(&io));
    }

    #[test]
    fn a_window_level_read_is_not_a_control_binding() {
        let io = fake();
        let slots = slots();
        script(&io, &[Some(ControlId::window_level(1, 10))], &[Some(false)]);
        on_committed_with(&io, &slots, "s", "hello");
        assert!(io.typed.borrow().is_empty());
        assert!(io
            .events
            .borrow()
            .iter()
            .any(|e| e.contains("control_unknown")));
    }

    #[test]
    fn focus_moving_to_another_control_in_the_same_window_stops_the_stream() {
        let io = fake();
        let slots = slots();
        // Bind read, the control read of the first gate, then a different control for chunk two.
        script(
            &io,
            &[Some(control(1)), Some(control(1)), Some(control(2))],
            &[Some(false)],
        );
        on_committed_with(&io, &slots, "s", "hello");
        on_committed_with(&io, &slots, "s", "hello world");
        assert_eq!(*io.typed.borrow(), vec!["hello".to_string()]);
        assert!(stopped(&io));
        // And it never types again, even if focus comes back.
        script(&io, &[Some(control(1))], &[Some(false)]);
        on_committed_with(&io, &slots, "s", "hello world again");
        assert_eq!(io.typed.borrow().len(), 1);
    }

    #[test]
    fn a_secure_field_appearing_mid_stream_stops_the_stream() {
        let io = fake();
        let slots = slots();
        script(&io, &[Some(control(1))], &[Some(false), Some(true)]);
        on_committed_with(&io, &slots, "s", "one");
        on_committed_with(&io, &slots, "s", "one two");
        on_committed_with(&io, &slots, "s", "one two three");
        assert_eq!(*io.typed.borrow(), vec!["one".to_string()]);
        assert!(stopped(&io));
    }

    #[test]
    fn a_secure_probe_that_times_out_stops_the_stream() {
        let mut io = fake();
        io.budget_ms = 5;
        io.probe_delay = Duration::from_millis(30);
        let slots = slots();
        script(&io, &[Some(control(1))], &[Some(false)]);
        on_committed_with(&io, &slots, "s", "hello");
        assert!(io.typed.borrow().is_empty());
        assert!(io
            .events
            .borrow()
            .iter()
            .any(|e| e.contains("evidence_timeout")));
    }

    #[test]
    fn the_batching_rule_skips_attempts_without_losing_the_delta() {
        let mut io = fake();
        io.interval_ms = 60_000;
        let slots = slots();
        script(&io, &[Some(control(1))], &[Some(false)]);
        on_committed_with(&io, &slots, "s", "a");
        on_committed_with(&io, &slots, "s", "a b"); // skipped, not refused
        on_committed_with(&io, &slots, "s", "a b c"); // skipped
        assert_eq!(*io.typed.borrow(), vec!["a".to_string()]);
        assert!(!stopped(&io));
    }

    #[test]
    fn reconcile_erases_only_after_a_fresh_gate_pass() {
        let io = fake();
        let slots = slots();
        script(&io, &[Some(control(1))], &[Some(false)]);
        on_committed_with(&io, &slots, "s", "hello wor");
        let remainder = remainder(take_for_final_with(&io, &slots, "s", "hello world"));
        // The cut moves back to the word boundary: " wor" is erased, " world" delivered.
        assert_eq!(remainder.as_deref(), Some(" world"));
        assert_eq!(*io.erased.borrow(), vec![4]);
    }

    #[test]
    fn reconcile_with_a_changed_control_erases_nothing_and_reports_the_text_left() {
        let io = fake();
        let slots = slots();
        script(&io, &[Some(control(1))], &[Some(false)]);
        on_committed_with(&io, &slots, "s", "hello wor");
        script(&io, &[Some(control(9))], &[Some(false)]);
        assert!(matches!(
            take_for_final_with(&io, &slots, "s", "hello world"),
            ReconcileOutcome::StoppedWritesRemain {
                remaining_chars: 9,
                ..
            }
        ));
        assert!(io.erased.borrow().is_empty());
    }

    #[test]
    fn reconcile_into_a_secure_field_erases_nothing() {
        let io = fake();
        let slots = slots();
        script(&io, &[Some(control(1))], &[Some(false)]);
        on_committed_with(&io, &slots, "s", "hello wor");
        script(&io, &[Some(control(1))], &[Some(true)]);
        assert!(matches!(
            take_for_final_with(&io, &slots, "s", "hello world"),
            ReconcileOutcome::StoppedWritesRemain {
                remaining_chars: 9,
                ..
            }
        ));
        assert!(io.erased.borrow().is_empty());
    }

    #[test]
    fn reconcile_with_nothing_to_erase_still_requires_the_same_control() {
        let io = fake();
        let slots = slots();
        script(&io, &[Some(control(1))], &[Some(false)]);
        on_committed_with(&io, &slots, "s", "hello");
        script(&io, &[Some(control(2))], &[Some(false)]);
        assert!(matches!(
            take_for_final_with(&io, &slots, "s", "hello world"),
            ReconcileOutcome::StoppedWritesRemain {
                remaining_chars: 5,
                ..
            }
        ));
    }

    #[test]
    fn cancel_erase_goes_through_the_gate() {
        let io = fake();
        let slots = slots();
        script(&io, &[Some(control(1))], &[Some(false)]);
        on_committed_with(&io, &slots, "s", "hello");
        erase_on_cancel_with(&io, &slots, "s");
        assert_eq!(*io.erased.borrow(), vec![5]);

        let io = fake();
        let slots = self::slots();
        script(&io, &[Some(control(1))], &[Some(false)]);
        on_committed_with(&io, &slots, "s", "hello");
        script(&io, &[Some(control(1))], &[None]);
        erase_on_cancel_with(&io, &slots, "s");
        assert!(io.erased.borrow().is_empty());
        assert!(io
            .events
            .borrow()
            .iter()
            .any(|e| e.contains("erased=false") && e.contains("secure_unknown")));
    }

    #[test]
    fn enter_needs_the_reconciled_binding_and_a_fresh_pass() {
        let io = fake();
        let slots = slots();
        // No binding at all.
        assert!(!press_enter_with(&io, &slots));
        assert_eq!(*io.enters.borrow(), 0);

        script(&io, &[Some(control(1))], &[Some(false)]);
        on_committed_with(&io, &slots, "s", "hello");
        assert_eq!(
            remainder(take_for_final_with(&io, &slots, "s", "hello")).as_deref(),
            Some("")
        );
        assert!(press_enter_with(&io, &slots));
        assert_eq!(*io.enters.borrow(), 1);
        // The binding is single use.
        assert!(!press_enter_with(&io, &slots));
    }

    #[test]
    fn enter_refuses_when_the_field_became_secure_or_unknown() {
        for secure in [Some(true), None] {
            let io = fake();
            let slots = slots();
            script(&io, &[Some(control(1))], &[Some(false)]);
            on_committed_with(&io, &slots, "s", "hello");
            take_for_final_with(&io, &slots, "s", "hello");
            script(&io, &[Some(control(1))], &[secure]);
            assert!(!press_enter_with(&io, &slots));
            assert_eq!(*io.enters.borrow(), 0);
        }
    }

    #[test]
    fn a_non_append_partial_stops_without_typing() {
        let io = fake();
        let slots = slots();
        script(&io, &[Some(control(1))], &[Some(false)]);
        on_committed_with(&io, &slots, "s", "hello");
        on_committed_with(&io, &slots, "s", "jello world");
        assert_eq!(io.typed.borrow().len(), 1);
        assert!(stopped(&io));
    }

    #[test]
    fn a_failed_keystroke_stops_the_stream() {
        let mut io = fake();
        io.type_ok = false;
        let slots = slots();
        script(&io, &[Some(control(1))], &[Some(false)]);
        on_committed_with(&io, &slots, "s", "hello");
        assert!(stopped(&io));
    }
}
