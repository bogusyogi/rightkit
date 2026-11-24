//! Streaming insertion: type the engine's stable words into the focused field while recording,
//! then reconcile with the final transcript.
//!
//! The engine only publishes committed partials while streaming is on, and each partial is the
//! full committed text so far (append-only). This side types the new suffix.
//!
//! Every write (typed chunk, final suffix, Enter) is built first
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
//! streaming for the session (it never types again). A refused suffix leaves the typed text in
//! place, and `take_for_final` says so with an explicit [`ReconcileOutcome`]; the host must not
//! deliver the full transcript on top of text that is still in the field.
//!
//! Streaming NEVER deletes text. There is no Backspace anywhere in this module: editors may delete
//! a grapheme per key while this side counts Unicode scalars, and an erase cannot be bound to the
//! span that was typed. What streaming does instead:
//!
//! * Every typed piece (a streamed chunk, or the suffix at the final) is an append-only suffix of
//!   what was already typed, and the caret proof is taken INSIDE the evidence gate, as the last
//!   control sample before the modifier sample, i.e. immediately before the write: same control, a
//!   collapsed selection, the caret exactly `start_offset + typed length` characters into the field
//!   (`start_offset` was read at the first chunk), and the text between those two positions equal to
//!   what was typed (only CRLF reads as LF). A target that has not yet applied earlier keys shows a
//!   proper prefix of the typed text: that is "pending", the write is skipped (streaming) or waited
//!   for up to [`SPAN_SETTLE_MS`] (final), never forced.
//! * A control that cannot give a caret and selection (no TextPattern selection on Windows, no AX
//!   selected range or AXValue on macOS, unreadable, another element focused) is never typed into.
//! * Final: if the final transcript starts with the streamed text, only the remaining suffix is
//!   typed (each batch through the same gated proof) and nothing is left for the host to paste. If
//!   the final differs from what was streamed (a rewrite) there is no way here to replace the span
//!   in one range-bound operation (neither `rightkit-automation` nor this crate can select an
//!   exact UIA or AX range and verify it), so nothing is erased or pasted: `StoppedWritesRemain`,
//!   the streamed text stays and the host offers the final as "Ready to copy". A paste on top of
//!   streamed text would duplicate it.
//! * Cancel leaves the streamed text in place (an event says so).
//!
//! Host inputs: [`StreamingHost`], passed to every call. The host supplies the secure-field probe,
//! the fresh focused-control probe, the probe budget and the event sink; the keystroke
//! primitives are reached only through `gated_writes`.

mod reconcile;

pub use reconcile::reconcile_point;

use std::cell::Cell;
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
    /// Read the caret of `control` now: the text from the start of the field to the caret and
    /// whether the selection is collapsed (`paste_delivery::focused_control_caret` is the
    /// implementation: Windows UIA TextPattern; macOS AX `AXValue` + `AXSelectedTextRange` of the
    /// focused element whose identity equals the binding's). `None` in the field: the platform has
    /// no caret readback, so streaming does not type at all. `Some` returning `None`: unreadable
    /// now (no selection range, other element focused, timeout), same refusal.
    pub focused_caret: Option<fn(control: &ControlId) -> Option<CaretRead>>,
}

/// What a caret readback returns: the text from the start of the field to the caret, as the
/// platform reports it, and whether the selection is collapsed (no range selected). The caret is
/// at the end of `before_caret`.
pub struct CaretRead {
    pub before_caret: String,
    pub collapsed: bool,
}

/// How long `take_for_final` waits for the streamed text to be fully applied (typed keys still
/// queued in the target) before it gives up on proving the span. Only the "pending" state waits;
/// any other mismatch fails at once.
pub const SPAN_SETTLE_MS: u64 = 400;
/// Pause between two readbacks while waiting.
pub const SPAN_POLL_MS: u64 = 40;

/// What `take_for_final` did to the field, as a closed set. The host decides what to deliver from
/// this and from nothing else: only [`ReconcileOutcome::NothingWritten`] and
/// [`ReconcileOutcome::Reconciled`] allow an automatic delivery. Nothing here ever erases.
///
/// Deliberately not `Debug`: `Reconciled` carries transcript text, so a `{:?}` would put content
/// in a log. Log [`ReconcileOutcome::tag`].
pub enum ReconcileOutcome {
    /// The field now holds the whole final transcript: the streamed text was a prefix of it and
    /// the missing suffix (if any) was typed here after a caret proof. `remainder` is therefore
    /// always empty and the host must paste nothing; [`press_enter`] may submit.
    Reconciled { remainder: String },
    /// This session wrote nothing into any field (no stream, a stream of another session, or no
    /// chunk was ever typed, and no send failed). The host delivers the whole transcript through
    /// the normal gated delivery, once.
    NothingWritten,
    /// Streaming stopped and text it typed is still in the field. `remaining_chars` is exactly
    /// the number of streamed characters left there. `cause` is a stable tag (a gate refusal code,
    /// a caret-proof cause such as `caret_moved`, `final_diverges_from_streamed`,
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
    /// Characters of field text before the caret when the first chunk was about to be typed (LF
    /// normalized, selection proven collapsed). `None` until bound, and where no caret readback
    /// is required (test seam). The streamed span must end exactly `typed` characters after it.
    start_offset: Option<usize>,
    /// A gate refused, a keystroke failed, or the partial was not append-only.
    stopped: bool,
    /// A typed batch was handed to the OS and reported failure: part of it may be in the field,
    /// so `typed` is no longer a count of what is there.
    uncertain: bool,
    last_attempt: Option<Instant>,
}

/// The span the Enter that follows a reconcile is authorized against: the binding, the caret offset
/// where the streamed span began, and the full final transcript that must now sit between that
/// offset and the caret.
struct EnterSpan {
    bound: Binding,
    start_offset: Option<usize>,
    expected: String,
}

struct Slots {
    stream: Mutex<Option<Stream>>,
    /// What the last reconcile left in the field, for the Enter that follows it.
    enter: Mutex<Option<EnterSpan>>,
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
    fn prepare_enter(&self) -> Option<Self::Batch>;
    /// Redeem the permit and send a built batch; nothing else.
    fn type_batch(&self, permit: WritePermit, batch: &Self::Batch) -> SendOutcome;
    fn enter_batch(&self, permit: WritePermit, batch: &Self::Batch) -> bool;
    fn emit(&self, event: &str, detail: &str);
    fn config(&self) -> StreamingConfig;
    /// The caret of the bound control now. The default (`NotRequired`) exists only for the unit
    /// test fake, which models no field; `OsIo` always answers `Read` or `Unavailable`.
    fn caret(&self, _control: &ControlId) -> CaretState {
        CaretState::NotRequired
    }
    /// Wait between two readbacks.
    fn pause(&self, millis: u64) {
        std::thread::sleep(Duration::from_millis(millis));
    }
}

/// The answer of a caret readback.
enum CaretState {
    /// Test seam only (see `StreamIo::caret`): no proof is demanded.
    NotRequired,
    /// The platform or the control cannot give a caret and selection now: fail closed.
    Unavailable,
    Read(CaretRead),
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
    fn prepare_enter(&self) -> Option<Self::Batch> {
        gated_writes::prepare_enter()
    }
    fn type_batch(&self, permit: WritePermit, batch: &Self::Batch) -> SendOutcome {
        gated_writes::type_prepared(permit, batch)
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
    fn caret(&self, control: &ControlId) -> CaretState {
        match self.host.focused_caret.and_then(|read| read(control)) {
            Some(read) => CaretState::Read(read),
            None => CaretState::Unavailable,
        }
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

/// Line breaks as the field reports them: CRLF reads as LF. Nothing else is normalized (a line
/// break is not a space; case, runs of spaces and every other character compare exactly).
fn lf(text: &str) -> String {
    text.replace("\r\n", "\n")
}

/// Why a caret proof failed, as a stable tag.
type ProofCause = &'static str;

/// Not a failure of the control: the target has applied only a proper prefix of the typed text so
/// far (keys still queued). Streaming skips the write and tries on the next partial; the final
/// waits up to [`SPAN_SETTLE_MS`]. It never lets a write through.
const PENDING: ProofCause = "pending";

/// The text before the caret, LF-normalized, with the selection proven collapsed. `Ok(None)` only
/// for the test seam (`CaretState::NotRequired`).
fn read_collapsed_caret<I: StreamIo>(
    io: &I,
    control: &ControlId,
) -> Result<Option<String>, ProofCause> {
    match io.caret(control) {
        CaretState::NotRequired => Ok(None),
        CaretState::Unavailable => Err("caret_unavailable"),
        CaretState::Read(read) if !read.collapsed => Err("selection_not_collapsed"),
        CaretState::Read(read) => Ok(Some(lf(&read.before_caret))),
    }
}

/// Characters (not bytes) of `text`.
fn chars_of(text: &str) -> usize {
    text.chars().count()
}

/// The part of `text` after its first `skip` characters.
fn after_chars(text: &str, skip: usize) -> Option<&str> {
    match text.char_indices().nth(skip) {
        Some((at, _)) => Some(&text[at..]),
        None => (chars_of(text) == skip).then_some(""),
    }
}

/// One proof attempt that the streamed span is exactly where it was typed and that appending at
/// the caret continues it: selection collapsed, caret exactly `start` + typed length characters
/// into the field, and the text between `start` and the caret equal to `typed`. The control
/// identity is the caller's (`fresh_state`), taken immediately before this read.
fn prove_span_once<I: StreamIo>(
    io: &I,
    control: &ControlId,
    start: Option<usize>,
    typed: &str,
) -> Result<(), ProofCause> {
    let Some(before) = read_collapsed_caret(io, control)? else {
        return Ok(());
    };
    let Some(start) = start else {
        return Err("caret_start_unknown");
    };
    let typed = lf(typed);
    let want = start + chars_of(&typed);
    let have = chars_of(&before);
    if have == want {
        return match after_chars(&before, start) {
            Some(span) if span == typed => Ok(()),
            _ => Err("streamed_span_mismatch"),
        };
    }
    if have >= start && have < want {
        if let Some(span) = after_chars(&before, start) {
            if typed.starts_with(span) {
                return Err(PENDING);
            }
        }
    }
    Err("caret_moved")
}

/// [`prove_span_once`], retried for at most [`SPAN_SETTLE_MS`] while (and only while) the result is
/// [`PENDING`]. Used where nothing is written (the final with nothing to add).
fn prove_span<I: StreamIo>(
    io: &I,
    control: &ControlId,
    start: Option<usize>,
    typed: &str,
) -> Result<(), ProofCause> {
    let started = Instant::now();
    loop {
        match prove_span_once(io, control, start, typed) {
            Err(PENDING) if started.elapsed() < Duration::from_millis(SPAN_SETTLE_MS) => {
                io.pause(SPAN_POLL_MS);
            }
            other => return other,
        }
    }
}

/// The caret proof a gate pass carries.
enum Proof<'a> {
    /// First chunk: the caret must be collapsed; its offset is recorded here (the span starts there).
    Bind(&'a Cell<Option<usize>>),
    /// Every later write: the streamed span `typed`, started at `start`, ends exactly at the caret.
    Span {
        start: Option<usize>,
        typed: &'a str,
    },
}

/// Why a gate pass refused.
enum GateStop {
    Evidence(Refusal),
    Proof(ProofCause),
}

impl GateStop {
    fn code(&self) -> &'static str {
        match self {
            GateStop::Evidence(refusal) => refusal.code(),
            GateStop::Proof(cause) => cause,
        }
    }
}

/// The gate for one streaming write against the binding: secure read, then the control and
/// application read followed IMMEDIATELY by the caret proof (the last control sample), then the
/// modifier sample last. The caller has already built the batch; the next thing it does with the
/// permit is the effect-only send. Nothing but the modifier sample sits between the caret proof and
/// the write, so a same-control caret move cannot slip past control-only gating.
fn gate_proven<I: StreamIo>(
    io: &I,
    kind: WriteKind,
    bound: &Binding,
    proof: Proof<'_>,
) -> Result<WritePermit, GateStop> {
    let cause: Cell<Option<ProofCause>> = Cell::new(None);
    let result = authorize_with_input(
        kind,
        Duration::from_millis(io.config().probe_budget_ms),
        || io.secure(&bound.control),
        || {
            let matched = fresh_state(io, bound);
            if matched != ControlMatch::Same {
                return matched;
            }
            let proven = match &proof {
                Proof::Bind(offset) => read_collapsed_caret(io, &bound.control)
                    .map(|before| offset.set(before.map(|before| chars_of(&before)))),
                Proof::Span { start, typed } => prove_span_once(io, &bound.control, *start, typed),
            };
            match proven {
                Ok(()) => ControlMatch::Same,
                Err(why) => {
                    cause.set(Some(why));
                    ControlMatch::Changed
                }
            }
        },
        || io.input_quiet(),
    );
    result.map_err(|refusal| match cause.get() {
        Some(why) => GateStop::Proof(why),
        None => GateStop::Evidence(refusal),
    })
}

/// The first chunk binds the foreground application (macOS) and the control focused now
/// (strongest identity the platform gives), then passes the same gate as every later write, so
/// the secure read, a second control read and a second application observation all happen after
/// the binding reads. The application is classified FIRST, so a terminal or an unclassified
/// application refuses with its own tag whatever the control read would say.
fn bind_and_gate<I: StreamIo>(
    io: &I,
    kind: WriteKind,
) -> Result<(Binding, Option<usize>, WritePermit), GateStop> {
    let app = io
        .capture_app()
        .map_err(|r| GateStop::Evidence(Refusal::App(r)))?;
    let Some(control) = io.fresh_control(IdentityLevel::Element) else {
        return Err(GateStop::Evidence(Refusal::ControlUnknown));
    };
    // Streaming never binds on window and process alone: it needs a control (focus handle, AX
    // element or UIA element) it can tell apart from its neighbours in the same window.
    if control.level < IdentityLevel::Control {
        return Err(GateStop::Evidence(Refusal::ControlUnknown));
    }
    // The observed application and the focused control must be the same process.
    if app
        .as_ref()
        .is_some_and(|app| app.pid() != control.process_id)
    {
        return Err(GateStop::Evidence(Refusal::App(
            AppRefusal::IdentityChanged,
        )));
    }
    // The first caret read is part of the gate pass (`Proof::Bind`): a control that cannot give a
    // collapsed caret is never typed into, and the start offset is read immediately before the write.
    let bound = Binding { control, app };
    let offset = Cell::new(None);
    let permit = gate_proven(io, kind, &bound, Proof::Bind(&offset))?;
    Ok((bound, offset.get(), permit))
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
            start_offset: None,
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
        Some(bound) => gate_proven(
            io,
            WriteKind::StreamChunk,
            &bound,
            Proof::Span {
                start: stream.start_offset,
                typed: &stream.typed,
            },
        ),
        None => bind_and_gate(io, WriteKind::StreamChunk).map(|(bound, start_offset, permit)| {
            stream.bound = Some(bound);
            stream.start_offset = start_offset;
            permit
        }),
    };
    let permit = match permit {
        Ok(permit) => permit,
        // The target has not applied the earlier keys yet: write nothing, the next partial carries
        // this delta again.
        Err(GateStop::Proof(PENDING)) => return,
        Err(stop) => {
            stream.stopped = true;
            let reason = match stop {
                GateStop::Evidence(refusal) => format!("evidence_{}", refusal.code()),
                GateStop::Proof(cause) => format!("caret_proof_{cause}"),
            };
            io.emit("streaming_insertion_stopped", &format!("reason={reason}"));
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

/// The final transcript for `session_id` is about to be delivered. Nothing is ever erased. The
/// result is an explicit [`ReconcileOutcome`]; there is no "none" that a caller can read as
/// "deliver everything":
///
/// * `Reconciled`: the final starts with the streamed text; the missing suffix was typed here
///   after a renewed caret proof (same control, caret exactly at the end of the streamed span).
///   `remainder` is empty; the host pastes nothing.
/// * `NothingWritten`: this session wrote nothing; deliver the whole transcript normally.
/// * `StoppedWritesRemain` / `StoppedCountUncertain`: streamed text is, or may be, still in the
///   field (the final differs from it, the proof failed, a gate refused). Deliver nothing
///   automatically and press no Return; keep the full transcript for manual copy.
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
    let typed_chars = chars_of(&stream.typed);
    let Some(bound) = stream.bound.clone() else {
        // Text was typed but no control is bound: where it went is unknown.
        io.emit("streaming_insertion_final", "outcome=count_uncertain");
        return ReconcileOutcome::StoppedCountUncertain {
            cause: "no_binding",
        };
    };
    // A rewrite: the final does not start with what was streamed. No Backspace exists here and no
    // range-bound replacement is available, so the streamed text stays, nothing is pasted on top of
    // it, and the host offers the final as "Ready to copy".
    let Some(suffix) = final_text.strip_prefix(stream.typed.as_str()) else {
        io.emit(
            "streaming_insertion_final",
            &format!("outcome=final_diverges_copy_fallback remaining_chars={typed_chars}"),
        );
        return ReconcileOutcome::StoppedWritesRemain {
            remaining_chars: typed_chars,
            cause: "final_diverges_from_streamed",
        };
    };
    let mut typed = stream.typed.clone();
    if !suffix.is_empty() {
        // The stop hotkey's modifiers must lift first; the gate refuses a held one regardless.
        let _ = io.settle_modifiers();
        let mut rest = suffix;
        while !rest.is_empty() {
            let take = gated_writes::typed_prefix_len(rest);
            let piece = &rest[..take];
            let batch = if take == 0 {
                None
            } else {
                io.prepare_type(piece)
            };
            let Some(batch) = batch else {
                return stopped_with_text(io, &typed, "batch_not_preparable");
            };
            // Every batch is preceded by its own proof inside the gate: same control, collapsed
            // selection, caret exactly at the end of what has been typed so far, span equal to it.
            let waiting_since = Instant::now();
            let permit = loop {
                match gate_proven(
                    io,
                    WriteKind::StreamChunk,
                    &bound,
                    Proof::Span {
                        start: stream.start_offset,
                        typed: &typed,
                    },
                ) {
                    Ok(permit) => break permit,
                    Err(GateStop::Proof(PENDING))
                        if waiting_since.elapsed() < Duration::from_millis(SPAN_SETTLE_MS) =>
                    {
                        io.pause(SPAN_POLL_MS);
                    }
                    Err(stop) => return stopped_with_text(io, &typed, stop.code()),
                }
            };
            match io.type_batch(permit, &batch) {
                SendOutcome::Sent => {
                    typed.push_str(piece);
                    rest = &rest[take..];
                }
                SendOutcome::NotSent => return stopped_with_text(io, &typed, "permit_expired"),
                SendOutcome::Failed => {
                    io.emit(
                        "streaming_insertion_final",
                        "outcome=suffix_failed_keystroke_failed count_uncertain=true",
                    );
                    return ReconcileOutcome::StoppedCountUncertain {
                        cause: "keystroke_failed",
                    };
                }
            }
        }
    }
    // For EVERY stop (Return requested or not), after the last write or when there was none: the
    // bounded readback (`prove_span`, retrying only while `pending`, at most `SPAN_SETTLE_MS`) must
    // show, in the same control, a collapsed caret at the end of the span and the text from the
    // start offset to the caret equal to the full final transcript. Sent is not applied: a target
    // can ignore keys. Missing, changed or unreadable: `Stopped`, the host offers the full final.
    if let Err(cause) = prove_span(io, &bound.control, stream.start_offset, &typed) {
        io.emit(
            "streaming_insertion_final",
            &format!(
                "outcome=final_span_unproven cause={cause} suffix_chars={} remaining_chars={}",
                chars_of(suffix),
                chars_of(&typed)
            ),
        );
        return ReconcileOutcome::StoppedWritesRemain {
            remaining_chars: chars_of(&typed),
            cause: "final_span_unproven",
        };
    }
    let moved = fresh_state(io, &bound);
    if moved != ControlMatch::Same {
        let cause = match moved {
            ControlMatch::App(refusal) => refusal.code(),
            _ => "destination_moved",
        };
        io.emit(
            "streaming_insertion_final",
            &format!("outcome=destination_moved cause={cause}"),
        );
        return ReconcileOutcome::StoppedWritesRemain {
            remaining_chars: chars_of(&typed),
            cause,
        };
    }
    if let Ok(mut enter) = slots.enter.lock() {
        *enter = Some(EnterSpan {
            bound,
            start_offset: stream.start_offset,
            expected: typed,
        });
    }
    io.emit(
        "streaming_insertion_final",
        &format!(
            "outcome=reconciled streamed_chars={typed_chars} suffix_chars={} erased_chars=0 remaining_chars=0",
            chars_of(suffix)
        ),
    );
    ReconcileOutcome::Reconciled {
        remainder: String::new(),
    }
}

/// The suffix could not be (fully) typed: whatever is in `typed` is in the field and stays.
fn stopped_with_text<I: StreamIo>(io: &I, typed: &str, cause: &'static str) -> ReconcileOutcome {
    let remaining_chars = chars_of(typed);
    io.emit(
        "streaming_insertion_final",
        &format!("outcome=suffix_refused_{cause} remaining_chars={remaining_chars}"),
    );
    ReconcileOutcome::StoppedWritesRemain {
        remaining_chars,
        cause,
    }
}

/// The recording was cancelled. Streamed text is NOT removed: no Backspace exists in this module
/// (editors may delete a grapheme per key while the count here is Unicode scalars), and no
/// range-bound deletion of an exact verified span is available. The event records that the text
/// was left in place.
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
    let count = chars_of(&stream.typed);
    io.emit(
        "streaming_insertion_cancel",
        &format!(
            "erased=false streamed_chars={count} detail=left_in_place_no_backspace count_uncertain={}",
            stream.uncertain
        ),
    );
}

/// Submit when the field holds the whole final transcript. Requires what the last
/// [`ReconcileOutcome::Reconciled`] left: the binding, the start offset and the full final text.
/// Return is sent only after a bounded readback (at most [`SPAN_SETTLE_MS`], retrying while the
/// target is still applying keys) proves, through `gate_proven` immediately before the key, that in
/// the same control the text from the start offset to a collapsed caret equals the full final
/// transcript and the caret is at its end. `false` (NO Return was sent) when there is no such
/// record, the batch cannot be built, the proof or the gate refuses or times out. The caller must
/// then treat the final as not delivered and offer it as "Ready to copy": the field may hold an
/// incomplete or changed text and a Return would submit it.
pub fn press_enter(host: &StreamingHost) -> bool {
    press_enter_with(&OsIo { host }, &SLOTS)
}

fn press_enter_with<I: StreamIo>(io: &I, slots: &Slots) -> bool {
    let Some(span) = slots.enter.lock().ok().and_then(|mut enter| enter.take()) else {
        io.emit("streaming_insertion_enter", "outcome=refused_no_binding");
        return false;
    };
    // Everything that builds or waits comes first: the Return batch is built, then the stop
    // hotkey's modifiers are waited out, and only then does the gate sample (the caret proof last
    // among the control samples, the modifier keys after it) and the send follow.
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
    let waiting_since = Instant::now();
    let permit = loop {
        match gate_proven(
            io,
            WriteKind::Enter,
            &span.bound,
            Proof::Span {
                start: span.start_offset,
                typed: &span.expected,
            },
        ) {
            Ok(permit) => break permit,
            Err(GateStop::Proof(PENDING))
                if waiting_since.elapsed() < Duration::from_millis(SPAN_SETTLE_MS) =>
            {
                io.pause(SPAN_POLL_MS);
            }
            Err(stop) => {
                io.emit(
                    "streaming_insertion_enter",
                    &format!("outcome=refused_{}", stop.code()),
                );
                return false;
            }
        }
    };
    io.enter_batch(permit, &batch)
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
    fn a_final_that_extends_the_typed_text_types_only_the_suffix() {
        let io = fake();
        let slots = slots();
        script(&io, &[Some(control(1))], &[Some(false)]);
        on_committed_with(&io, &slots, "s", "hello wor");
        let remainder = remainder(take_for_final_with(&io, &slots, "s", "hello world"));
        // Prefix match: only "ld" is typed here, nothing is left for the host to paste.
        assert_eq!(remainder.as_deref(), Some(""));
        assert_eq!(
            *io.typed.borrow(),
            vec!["hello wor".to_string(), "ld".to_string()]
        );
    }

    #[test]
    fn reconcile_with_a_changed_control_types_nothing_and_reports_the_text_left() {
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
        assert_eq!(io.typed.borrow().len(), 1);
    }

    #[test]
    fn reconcile_into_a_secure_field_types_nothing() {
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
        assert_eq!(io.typed.borrow().len(), 1);
    }

    #[test]
    fn reconcile_with_a_suffix_still_requires_the_same_control() {
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
    fn cancel_leaves_the_streamed_text_in_place() {
        let io = fake();
        let slots = slots();
        script(&io, &[Some(control(1))], &[Some(false)]);
        on_committed_with(&io, &slots, "s", "hello");
        erase_on_cancel_with(&io, &slots, "s");
        assert_eq!(*io.typed.borrow(), vec!["hello".to_string()]);
        assert!(io
            .events
            .borrow()
            .iter()
            .any(|e| e.contains("erased=false") && e.contains("left_in_place_no_backspace")));
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
