//! Delivery — pure record shaping (no clipboard / window / keystroke calls).
//!
//! The OS side effects (clipboard write, Ctrl+V / Enter SendInput, foreground
//! window snapshot, the real delivery backend) live in the host application
//! and build these shapes. Time/pid helpers use std only (no GUI), so they're
//! fine here.

use serde::{Deserialize, Serialize};
use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

// Every delivery path (real backend, fallback, file transcription) shares this
// sequence. Per-backend counters restart whenever a backend is rebuilt, which
// happens for every sidecar transcript and could collide within one millisecond.
static NEXT_DELIVERY_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DeliveryId(String);

impl DeliveryId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CopyFallbackReason {
    ClipboardUnavailable,
    ElevatedTarget,
    FocusChanged,
    PasteFailed,
    EmptyTranscript,
    /// The focused target was not a text input, so there was nowhere to paste —
    /// the transcript is copied and offered in the recent-transcripts popup.
    NoTextField,
    /// Cross-platform: no stop-time destination could be captured (both
    /// platforms produce this; the former "macOS only" comment was stale —
    /// behavior was already identical on Windows). Editor identity is
    /// OPTIONAL evidence and never produces this outcome by itself; this
    /// reason is reserved for a genuinely missing stop destination, which
    /// forces clipboard-only recovery (spec sections 3.2/3.3). Distinct from
    /// `FocusChanged` so the two causes stay separable in telemetry.
    NoFocusPin,
    /// Windows only: the pre-stage clipboard snapshot could not represent
    /// every format present (GDI handles, owner-display, oversized data), so
    /// running the destructive transcript transaction would silently drop
    /// them. Delivery spec section 4.5 forbids that: route to visible
    /// recovery and let explicit Copy replace the clipboard instead.
    ClipboardLossySnapshot,
    UnsupportedPlatform,
    Other(String),
}

impl std::fmt::Display for CopyFallbackReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ClipboardUnavailable => write!(f, "clipboard_unavailable"),
            Self::ElevatedTarget => write!(f, "elevated_target"),
            Self::FocusChanged => write!(f, "focus_changed"),
            Self::PasteFailed => write!(f, "paste_failed"),
            Self::EmptyTranscript => write!(f, "empty_transcript"),
            Self::NoTextField => write!(f, "no_text_field"),
            Self::NoFocusPin => write!(f, "no_focus_pin"),
            Self::ClipboardLossySnapshot => write!(f, "clipboard_lossy_snapshot"),
            Self::UnsupportedPlatform => write!(f, "unsupported_platform"),
            Self::Other(reason) => write!(f, "{reason}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeliveryOutcome {
    Pasted,
    /// Input was posted to the frozen target, but target-attributable
    /// readback was unavailable or inconclusive. Transcript remains
    /// recoverable; callers must never retry insertion or submit Enter.
    PasteSentUnconfirmed {
        reason: String,
    },
    CopiedFallback {
        reason: CopyFallbackReason,
    },
    Error {
        code: String,
        message: String,
    },
    /// A cancel (any source) that finalized to encrypted history instead of an
    /// OS delivery — no clipboard write, no paste, no Enter. `DeliveryRecord`
    /// carries the raw/L0 transcript so the history filter can hide it by
    /// default (see `crate::history::HistoryFilter::cancelled_only`).
    Cancelled,
    /// An in-progress snapshot of the committed transcript, written during
    /// recording from `EngineEvent::TranscriptPartial` so a crash mid-
    /// dictation still has something recoverable (crash-recovery draft
    /// history rows). Never a terminal, user-visible outcome: a normal
    /// delivery or an explicit cancel supersedes/removes the draft row for
    /// its session before writing its own terminal record (see
    /// `EngineRuntime::accept_sidecar_delivery` / `accept_cancel_to_history`
    /// in the shell crate). Any `Draft` still present at the next launch
    /// means the process died mid-recording — the startup sweep
    /// (`recover_orphaned_drafts`) flips it to `Cancelled`. Hidden from the
    /// default history view exactly like `Cancelled` (see
    /// `HistoryFilter::cancelled_only`).
    Draft,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryMethod {
    ClipboardThenPaste,
    ClipboardThenPasteUnconfirmed,
    ClipboardOnly,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryTimingStep {
    pub name: String,
    pub elapsed_ms: u64,
}

impl DeliveryTimingStep {
    pub fn new(name: impl Into<String>, elapsed_ms: u64) -> Self {
        Self {
            name: name.into(),
            elapsed_ms,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryTimings {
    pub total_ms: u64,
    pub steps: Vec<DeliveryTimingStep>,
}

impl DeliveryTimings {
    pub fn new(total_ms: u64) -> Self {
        Self {
            total_ms,
            steps: Vec::new(),
        }
    }

    pub fn from_steps(total_ms: u64, steps: Vec<DeliveryTimingStep>) -> Self {
        Self { total_ms, steps }
    }

    pub fn with_step(mut self, name: impl Into<String>, elapsed_ms: u64) -> Self {
        self.steps.push(DeliveryTimingStep::new(name, elapsed_ms));
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopContinuity {
    /// OS-wide monotonic clock; comparable between engine & shell processes.
    pub captured_at: u64,
    pub process_instance: u64,
    #[serde(default)]
    pub native_control: Option<isize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetSnapshot {
    pub process_id: Option<u32>,
    pub process_name: Option<String>,
    pub window_title: Option<String>,
    pub window_handle: Option<isize>,
    #[serde(default)]
    pub focused_control_handle: Option<isize>,
    #[serde(default)]
    pub foreground_target: Option<ForegroundTarget>,
    #[serde(default)]
    pub focused_text_input: Option<bool>,
    pub is_elevated: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_continuity: Option<StopContinuity>,
    /// Full executable path (Windows) or app bundle path (macOS) of the
    /// destination process. Used only to resolve the app icon in History;
    /// absent on older rows and when the platform could not report it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exe_path: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ForegroundTarget {
    WindowHandle { handle: isize },
    ProcessId { pid: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopOrigin {
    Generic,
    Pill,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingStopContext {
    pub session_id: String,
    pub origin: StopOrigin,
    pub overlay_handles: Vec<isize>,
}

impl PendingStopContext {
    pub fn new(
        session_id: impl Into<String>,
        origin: StopOrigin,
        overlay_handles: Vec<isize>,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            origin,
            overlay_handles,
        }
    }

    pub fn applies_to_overlay(&self, session_id: &str, target: &TargetSnapshot) -> bool {
        self.session_id == session_id
            && self.origin == StopOrigin::Pill
            && target
                .window_handle
                .is_some_and(|handle| self.overlay_handles.contains(&handle))
    }
}

#[derive(Debug, Default)]
pub struct PendingStopContextLatch {
    context: Option<PendingStopContext>,
}

impl PendingStopContextLatch {
    pub fn latch_first(&mut self, context: PendingStopContext) -> bool {
        if self.context.is_some() {
            return false;
        }
        self.context = Some(context);
        true
    }

    pub fn as_ref(&self) -> Option<&PendingStopContext> {
        self.context.as_ref()
    }

    pub fn clear(&mut self) {
        self.context = None;
    }
}

impl TargetSnapshot {
    /// All-`None` snapshot for records that never touched a delivery target —
    /// currently only draft rows (`DeliveryRecord::new_draft`), which exist
    /// purely so a crash mid-recording still leaves something recoverable in
    /// history.
    pub fn empty() -> Self {
        Self {
            process_id: None,
            process_name: None,
            window_title: None,
            window_handle: None,
            focused_control_handle: None,
            foreground_target: None,
            focused_text_input: None,
            is_elevated: None,
            stop_continuity: None,
            exe_path: None,
        }
    }

    pub fn test_target() -> Self {
        Self {
            process_id: Some(1),
            process_name: Some("test-target".to_string()),
            window_title: Some("Test Target".to_string()),
            window_handle: Some(1),
            focused_control_handle: Some(2),
            foreground_target: Some(ForegroundTarget::WindowHandle { handle: 1 }),
            focused_text_input: Some(true),
            is_elevated: Some(false),
            stop_continuity: None,
            exe_path: None,
        }
    }

    /// Conservative identity proof for irreversible delivery. A field handle,
    /// when both observations expose one, is stronger than app/window identity.
    /// Missing/unknown identity never proves that two targets are identical.
    pub fn same_stable_target(&self, other: &Self) -> bool {
        match (self.focused_control_handle, other.focused_control_handle) {
            (Some(left), Some(right)) => return left == right,
            (Some(_), None) | (None, Some(_)) => return false,
            (None, None) => {}
        }
        match (&self.foreground_target, &other.foreground_target) {
            (
                Some(ForegroundTarget::WindowHandle { handle: left }),
                Some(ForegroundTarget::WindowHandle { handle: right }),
            ) => left == right,
            (
                Some(ForegroundTarget::ProcessId { pid: left }),
                Some(ForegroundTarget::ProcessId { pid: right }),
            ) => left == right,
            _ => false,
        }
    }
}

pub fn target_matches_expected(
    current: &TargetSnapshot,
    expected: &TargetSnapshot,
    require_editable: bool,
) -> bool {
    current.process_id == expected.process_id
        && expected
            .window_handle
            .is_none_or(|window| current.window_handle == Some(window))
        && current.same_stable_target(expected)
        && if require_editable {
            current.focused_text_input == Some(true)
        } else {
            current.focused_text_input != Some(false)
        }
}

pub fn legacy_non_windows_target_is_pasteable(
    target: &TargetSnapshot,
    own_process_id: u32,
) -> bool {
    target.process_id != Some(own_process_id) && target.focused_text_input != Some(false)
}

/// Deliver only to destination frozen at recording stop. Never restore focus
/// or substitute a later focused app/window for a missing stop target.
pub fn execute_targeted_delivery<R>(
    stop_target: Option<&TargetSnapshot>,
    own_process_id: u32,
    mut snapshot: impl FnMut() -> TargetSnapshot,
    mut paste: impl FnMut(TargetSnapshot, bool) -> R,
    mut fallback: impl FnMut(CopyFallbackReason, TargetSnapshot) -> R,
    send_enter: bool,
) -> R {
    let Some(stopped) = stop_target else {
        return fallback(CopyFallbackReason::FocusChanged, TargetSnapshot::empty());
    };
    if stopped.process_id.is_none() || stopped.process_id == Some(own_process_id) {
        return fallback(CopyFallbackReason::NoTextField, stopped.clone());
    }
    if stopped.focused_text_input == Some(false) {
        return fallback(CopyFallbackReason::NoTextField, stopped.clone());
    }
    let live = snapshot();
    if !target_matches_expected(&live, stopped, false) {
        return fallback(CopyFallbackReason::FocusChanged, stopped.clone());
    }
    paste(stopped.clone(), send_enter)
}

/// Where a history record came from. `None`/absent (the serde default) means a
/// normal dictation delivery — every record predating this field deserializes as
/// dictation, so no DB migration is needed. `FileTranscription` marks a "Transcribe
/// a file" result so the History UI can label it distinctly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum DeliverySource {
    Dictation,
    FileTranscription {
        file_name: String,
    },
    /// A transcript pulled from CloudKit that originated on another of the
    /// user's devices (iPhone, Watch, or another Mac). `device` carries the
    /// CloudKit `source` field verbatim ("iPhone"/"Watch"/"Mac") so the
    /// History UI can badge it. Frontend reads `record.source.device`.
    CloudSync {
        device: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryRecord {
    pub delivery_id: String,
    pub transcript: String,
    pub outcome: DeliveryOutcome,
    pub method: DeliveryMethod,
    pub elapsed_ms: u64,
    pub target: TargetSnapshot,
    pub delivered_at_ms: u64,
    /// The transcript BEFORE AI polish (L3). `None` for records predating this
    /// field or when no AI polish ran — lets the UI toggle raw <-> polished
    /// (undo/redo the AI edit) without re-running anything.
    #[serde(default)]
    pub raw_transcript: Option<String>,
    /// Recorded audio length in ms (the clip duration), when known.
    #[serde(default)]
    pub recording_ms: Option<u64>,
    /// Per-step delivery timings. Additive and optional so old history records,
    /// IPC payloads, and tests that only know `elapsed_ms` keep deserializing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_timing: Option<DeliveryTimings>,
    /// Origin of this record. Absent = dictation (back-compat default).
    #[serde(default)]
    pub source: Option<DeliverySource>,
}

impl DeliveryRecord {
    pub fn new(
        delivery_id: impl Into<String>,
        transcript: impl Into<String>,
        outcome: DeliveryOutcome,
        target: TargetSnapshot,
    ) -> Self {
        Self::new_with_timing(delivery_id, transcript, outcome, target, 0)
    }

    pub fn new_with_timing(
        delivery_id: impl Into<String>,
        transcript: impl Into<String>,
        outcome: DeliveryOutcome,
        target: TargetSnapshot,
        elapsed_ms: u64,
    ) -> Self {
        let method = method_for_outcome(&outcome);
        Self {
            delivery_id: delivery_id.into(),
            transcript: transcript.into(),
            method,
            elapsed_ms,
            outcome,
            target,
            delivered_at_ms: now_ms(),
            raw_transcript: None,
            recording_ms: None,
            delivery_timing: None,
            source: None,
        }
    }

    /// Attach the pre-AI-polish transcript + clip duration. Builder, called at the
    /// delivery site where both are known (the polished text is `transcript`).
    pub fn with_raw(mut self, raw_transcript: Option<String>, recording_ms: Option<u64>) -> Self {
        self.raw_transcript = raw_transcript.filter(|r| !r.is_empty());
        self.recording_ms = recording_ms;
        self
    }

    pub fn with_delivery_timing(mut self, timing: DeliveryTimings) -> Self {
        self.delivery_timing = Some(timing);
        self
    }

    /// Tag this record's origin (e.g. a file transcription). Builder; dictation
    /// records simply never call it (source stays None).
    pub fn with_source(mut self, source: DeliverySource) -> Self {
        self.source = Some(source);
        self
    }

    pub fn for_test(
        delivery_id: DeliveryId,
        transcript: impl Into<String>,
        outcome: DeliveryOutcome,
    ) -> Self {
        Self::new(
            delivery_id.into_string(),
            transcript,
            outcome,
            TargetSnapshot::test_target(),
        )
    }

    /// Build (or rebuild) the draft row for a session. `delivery_id` is
    /// deterministic (`draft_delivery_id`) so every store's `upsert_draft`
    /// targets the same row across repeated calls for the same session.
    pub fn new_draft(session_id: &str, transcript: impl Into<String>) -> Self {
        Self::new(
            draft_delivery_id(session_id),
            transcript,
            DeliveryOutcome::Draft,
            TargetSnapshot::empty(),
        )
    }
}

pub trait DeliveryBackend: Send {
    fn deliver(&mut self, transcript: &str) -> Result<DeliveryRecord, DeliveryError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryError {
    pub code: String,
    pub message: String,
}

impl DeliveryError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for DeliveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for DeliveryError {}

pub fn method_for_outcome(outcome: &DeliveryOutcome) -> DeliveryMethod {
    match outcome {
        DeliveryOutcome::Pasted => DeliveryMethod::ClipboardThenPaste,
        DeliveryOutcome::PasteSentUnconfirmed { .. } => {
            DeliveryMethod::ClipboardThenPasteUnconfirmed
        }
        DeliveryOutcome::CopiedFallback { .. } => DeliveryMethod::ClipboardOnly,
        DeliveryOutcome::Error { .. } => DeliveryMethod::None,
        DeliveryOutcome::Cancelled => DeliveryMethod::None,
        DeliveryOutcome::Draft => DeliveryMethod::None,
    }
}

/// Deterministic delivery_id for a session's in-progress draft row (crash-
/// recovery draft history rows). Stable per session so repeated
/// `HistoryStore::upsert_draft` calls target the SAME row instead of
/// accumulating one per `TranscriptPartial` tick.
pub fn draft_delivery_id(session_id: &str) -> String {
    format!("draft-{session_id}")
}

pub fn next_delivery_id(_legacy_counter: u64) -> String {
    let sequence = NEXT_DELIVERY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("delivery-{}-{}-{sequence}", std::process::id(), now_ms())
}

pub fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis() as u64
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub fn copied_fallback_record_from_clipboard_result(
    transcript: &str,
    reason: CopyFallbackReason,
    target: TargetSnapshot,
    started: Instant,
    clipboard_result: Result<(), CopyFallbackReason>,
) -> DeliveryRecord {
    let outcome = match clipboard_result {
        Ok(()) => DeliveryOutcome::CopiedFallback { reason },
        Err(err) => DeliveryOutcome::CopiedFallback { reason: err },
    };
    DeliveryRecord::new_with_timing(
        next_delivery_id(0),
        transcript,
        outcome,
        target,
        elapsed_ms(started),
    )
}

pub fn copy_fallback_record_with_clipboard(
    transcript: &str,
    reason: CopyFallbackReason,
    target: TargetSnapshot,
    write_clipboard: impl FnOnce(&str) -> Result<(), CopyFallbackReason>,
) -> DeliveryRecord {
    copied_fallback_record_from_clipboard_result(
        transcript,
        reason,
        target,
        Instant::now(),
        write_clipboard(transcript),
    )
}

pub fn copy_text_with_clipboard(
    text: &str,
    write_clipboard: impl FnOnce(&str) -> Result<(), CopyFallbackReason>,
) -> Result<(), DeliveryError> {
    write_clipboard(text).map_err(|reason| DeliveryError::new("E_CLIPBOARD", reason.to_string()))
}

pub fn target_process_id(
    target: &TargetSnapshot,
    message: &'static str,
) -> Result<i32, DeliveryError> {
    target
        .process_id
        .map(|pid| pid as i32)
        .ok_or_else(|| DeliveryError::new("E_NO_TARGET", message))
}

pub fn focus_restore_error(message: &'static str) -> DeliveryError {
    DeliveryError::new("E_FOCUS_RESTORE", message)
}

pub fn unsupported_focus_restore() -> Result<(), DeliveryError> {
    Err(DeliveryError::new(
        "E_UNSUPPORTED",
        "focus restore is unsupported on this platform",
    ))
}

/// Did `needle` land in the focused element *because of the paste we just
/// posted*, rather than merely being present already?
///
/// Presence alone is not proof and must never gate Enter or a clipboard
/// restore. A field that already held the same text — the user dictating
/// "thanks" twice into one box, or re-dictating after a correction — reads as
/// "contains the transcript" the instant the paste is posted, before the
/// target has processed it. Treating that as consumption restores the prior
/// clipboard mid-flight and fires Enter on whatever the target reads next,
/// which is exactly the field failure the evidence-gated delivery path exists
/// to prevent.
///
/// Attribution therefore requires the value to have CHANGED and to contain
/// exactly one more occurrence of `needle` than it did before. Anything else —
/// unchanged text, a same-count change, an autocorrect that mangled the
/// insertion — returns false, which callers treat as "not consumed": the
/// transcript stays on the clipboard and Enter never fires. Failing closed is
/// always the safe direction here.
///
/// Pure and platform-independent so the contract is executable on the host
/// running the suite, not only on the OS whose probe feeds it. macOS keeps a
/// strictly stronger check of its own (exact head+needle+tail reconstruction
/// from the pre-paste selection range, in the host's macOS platform layer); this
/// is the proof available where a reliable pre-paste caret range is not.
pub fn insertion_attributable(before: &str, after: &str, needle: &str) -> bool {
    let needle = needle.trim();
    if needle.is_empty() {
        return false;
    }
    if after == before {
        return false;
    }
    after.matches(needle).count() == before.matches(needle).count() + 1
}

/// [`insertion_attributable`] for a terminal character grid (a UIA text-pattern read of the
/// terminal's rows). A grid wraps a long transcript across several rows and a TUI input box
/// (Claude Code's prompt) draws a border glyph at the edge of every row, so the transcript never
/// appears as one contiguous string even when the paste landed. The comparison therefore drops
/// whitespace and box-drawing glyphs from the before, after and needle, and keeps the same
/// contract: the grid changed and now holds exactly one more occurrence of the transcript.
/// Applied only to terminal grids; text controls keep the strict contiguous check.
pub fn terminal_grid_insertion_attributable(before: &str, after: &str, needle: &str) -> bool {
    fn squash(text: &str) -> String {
        text.chars()
            .filter(|c| !c.is_whitespace() && !('\u{2500}'..='\u{259F}').contains(c))
            .collect()
    }
    let needle = squash(needle);
    if needle.is_empty() {
        return false;
    }
    let (before, after) = (squash(before), squash(after));
    if after == before {
        return false;
    }
    after.matches(needle.as_str()).count() == before.matches(needle.as_str()).count() + 1
}

/// Attributable proof that a specific insertion attempt actually landed in
/// the target control — HR-DELIVERY-006. The only way to obtain one is
/// [`InsertionProof::from_confirmed_delta`], which requires the caller to
/// have ALREADY performed a real read-back comparison (macOS
/// `insertion_confirmed_for_pid`, Windows `insertion_attributable` against a
/// before/after UIA read, or an equivalent native-write read-back) — never a
/// bare `Ok(())`/success return from a setter or synthetic keystroke API.
///
/// `pasted_delivery_record` is the sanctioned constructor for a `Pasted`
/// delivery record in this module; every delivery call site this lane owns
/// (the host's dictation and command delivery backends)
/// routes through it instead of building `DeliveryOutcome::Pasted` directly,
/// so holding a proof token is required to report success. `DeliveryOutcome`
/// itself stays a plain public enum (many crates outside this lane's
/// ownership match/construct `DeliveryOutcome::Pasted` today — history,
/// telemetry, controller, state — so narrowing the variant's shape would be a
/// breaking change across files this lane does not own and cannot compile-
/// verify under the standing build hold); the guarantee is therefore
/// enforced at the construction site within owned files, not by the
/// compiler across the whole workspace. See
/// `docs/dispatch/evidence/native-insertion-proof.md` for the full
/// reasoning and the associated escalation.
#[derive(Debug, Clone, Copy)]
pub struct InsertionProof {
    _sealed: (),
}

impl InsertionProof {
    /// `delta_confirmed` MUST be the result of an actual before/after
    /// read-back comparison attributable to THIS insertion attempt — e.g.
    /// a macOS accessibility read-back for the target pid, or
    /// `insertion_attributable(before, after, transcript)` against a genuine
    /// pre-insertion snapshot. Passing a bare success/`Ok(())` boolean from a
    /// setter or keystroke call defeats the entire point of this type; there
    /// is no way for the compiler to catch that misuse, so it remains a
    /// reviewable invariant of every call site that constructs one.
    pub fn from_confirmed_delta(delta_confirmed: bool) -> Option<Self> {
        delta_confirmed.then_some(Self { _sealed: () })
    }
}

/// Build a `Pasted` delivery record. This is the sanctioned way (within this
/// lane's owned files) to report a successful native or clipboard+paste
/// insertion — it cannot be called without an [`InsertionProof`], which
/// itself cannot be constructed without a real read-back delta. See
/// `InsertionProof` for the full contract (HR-DELIVERY-006).
pub fn pasted_delivery_record(
    delivery_id: impl Into<String>,
    transcript: impl Into<String>,
    target: TargetSnapshot,
    total_elapsed_ms: u64,
    _proof: InsertionProof,
) -> DeliveryRecord {
    DeliveryRecord::new_with_timing(
        delivery_id,
        transcript,
        DeliveryOutcome::Pasted,
        target,
        total_elapsed_ms,
    )
}

/// May the start capture bind polish context and an "original" destination?
/// Only when the proof taken AT the hotkey names the focused editor. A proof
/// acquired later, or one without an editor identity, cannot say which box had
/// focus at the hotkey: accepting whichever editor shows up afterwards lets a
/// same-window A -> B switch turn B's text into context and B into the bound
/// destination. Such sessions deliver through the stop-time path only.
pub fn start_capture_may_bind(
    proof_at_hotkey: bool,
    proof_editor: Option<isize>,
    pinned_editor: isize,
) -> bool {
    proof_at_hotkey && proof_editor == Some(pinned_editor)
}

/// Container-aware variant of `start_capture_may_bind`. Chromium-class apps
/// report a web-area/document ANCESTOR of the real editor as focused, so the
/// hotkey proof can name that container instead of the pinned editor. When the
/// native layer has proven (bounded walk, non-secure, editable, never reaching
/// the top-level window) that the proof's control is an ancestor container of
/// the pin, the proof counts as naming the pin. A missing/late proof still
/// never binds: the rewrite happens only when a proof editor exists.
pub fn start_capture_may_bind_via_container(
    proof_at_hotkey: bool,
    proof_editor: Option<isize>,
    pinned_editor: isize,
    proof_names_pin_container: bool,
) -> bool {
    let effective_pin = match proof_editor {
        Some(editor) if proof_names_pin_container => editor,
        _ => pinned_editor,
    };
    start_capture_may_bind(proof_at_hotkey, proof_editor, effective_pin)
}

/// Is a focused element under a web-area container bindable as the editor?
/// Positively non-secure, keyboard-focusable, and exposing a writable Value or
/// a TextEdit target. A plain page-level Document (readable text, no edit
/// capability) never binds, so whole-page text cannot reach polish as context.
pub fn web_area_editor_is_bindable(
    is_password: Option<bool>,
    keyboard_focusable: bool,
    writable_value: bool,
    text_edit: bool,
) -> bool {
    is_password == Some(false) && keyboard_focusable && (writable_value || text_edit)
}

/// Stop-time recovery for an UNBOUND start (no box text was sent anywhere):
/// the stop target must be the same, non-secure, continuous editor. Same place
/// (process + window), continuity unbroken before AND after the native secure
/// read, a positively non-secure result, and either the same focused element or
/// a positively editable one. An unknown editable state never passes unless the
/// element is the same one; a known non-editable never passes.
pub fn unbound_stop_editor_is_safe(
    same_place: bool,
    continuous_before: bool,
    continuous_after: bool,
    same_element: bool,
    non_secure: Option<bool>,
    editable: Option<bool>,
) -> bool {
    same_place
        && continuous_before
        && continuous_after
        && non_secure == Some(true)
        && editable != Some(false)
        && (same_element || editable == Some(true))
}

/// A failed native read is not evidence that the user's editor changed.
/// Only Unchanged authorizes context-bound output; Unreadable may be retried
/// against the same retained editor, without selecting a new destination.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorContentStatus {
    Unchanged,
    Changed,
    Unreadable,
    Destroyed,
    MissingBaseline,
}

impl EditorContentStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Unchanged => "unchanged",
            Self::Changed => "changed",
            Self::Unreadable => "unreadable",
            Self::Destroyed => "destroyed",
            Self::MissingBaseline => "missing_baseline",
        }
    }
}

pub fn editor_content_status(
    expected: Option<u64>,
    observed: Option<u64>,
    destroyed: bool,
) -> EditorContentStatus {
    match (expected, observed, destroyed) {
        (_, _, true) => EditorContentStatus::Destroyed,
        (None, _, false) => EditorContentStatus::MissingBaseline,
        (Some(expected), Some(observed), false) if expected == observed => {
            EditorContentStatus::Unchanged
        }
        (Some(_), Some(_), false) => EditorContentStatus::Changed,
        (Some(_), None, false) => EditorContentStatus::Unreadable,
    }
}

/// Whether dictation inserted at `caret_utf16` (UTF-16 units into `value`,
/// `None` = end of field) must be preceded by one space. True only when a
/// character exists immediately before the caret and it is neither whitespace
/// nor an opening bracket/quote. Pure: callers decide placeholder/terminal.
pub fn leading_space_needed(value: &str, caret_utf16: Option<usize>) -> bool {
    let units: Vec<u16> = value.encode_utf16().collect();
    let caret = caret_utf16.unwrap_or(units.len()).min(units.len());
    let before = String::from_utf16_lossy(&units[..caret]);
    match before.chars().next_back() {
        None => false,
        Some(c) => {
            !c.is_whitespace()
                && !matches!(
                    c,
                    '(' | '['
                        | '{'
                        | '"'
                        | '\''
                        | '\u{201C}'
                        | '\u{2018}'
                        | '/'
                        | '@'
                        | '#'
                        | '\u{FEFF}'
                        | '\u{200B}'
                )
        }
    }
}

/// Apply the start-time leading-space hint to the text about to be delivered.
/// Never double-spaces and never separates leading punctuation from the word
/// it attaches to.
pub fn with_leading_space(transcript: &str, needed: bool) -> std::borrow::Cow<'_, str> {
    let attaches = transcript.chars().next().is_none_or(|c| {
        c.is_whitespace()
            || matches!(
                c,
                '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}' | '\u{2019}' | '\u{201D}'
            )
    });
    if needed && !attaches {
        std::borrow::Cow::Owned(format!(" {transcript}"))
    } else {
        std::borrow::Cow::Borrowed(transcript)
    }
}

/// Terminal emulators generally do not expose their character-grid content
/// through UI Automation's Edit/Document control types or Value/TextEdit
/// patterns the way normal text boxes do — there is no "text control" there
/// from Windows' perspective, just a rendered grid. Callers that need to know
/// whether UIA readback/identity is trustworthy for a given target (the live
/// dictation delivery path and the non-live focus probe) both need this same
/// process/class/title allowlist. Kept here, shared through this crate, so
/// the two copies cannot drift the way they previously did.
#[cfg(target_os = "windows")]
pub fn windows_terminal_target(
    process_name: Option<&str>,
    window_title: Option<&str>,
    class_name: Option<&str>,
) -> bool {
    let process = process_name.unwrap_or("").to_ascii_lowercase();
    let class = class_name.unwrap_or("").to_ascii_lowercase();
    let title = window_title.unwrap_or("").to_ascii_lowercase();

    matches!(
        process.as_str(),
        "windowsterminal.exe"
            | "wt.exe"
            | "openconsole.exe"
            | "conhost.exe"
            | "cmd.exe"
            | "powershell.exe"
            | "pwsh.exe"
            | "alacritty.exe"
            | "wezterm.exe"
            | "wezterm-gui.exe"
            | "mintty.exe"
    ) || class.contains("cascadia")
        || class == "consolewindowclass"
        || class.contains("mintty")
        || title.contains("command prompt")
        || title.contains("powershell")
        || title.contains("windows terminal")
}

#[cfg(test)]
#[path = "delivery_tests.rs"]
mod tests;

/// An app can REPLACE the editor whose text went to polish while the user is
/// still dictating (Claude re-creates its composer).
/// The retained original is then dead, but the box the user sees is the same
/// place. A different element may receive the text only when it is provably
/// that box: same process and window, not secure, and holding EXACTLY the text
/// that was captured at start. Anything else stays a clipboard recovery.
pub fn replacement_editor_may_receive(
    same_process_and_window: bool,
    start_fingerprint: Option<u64>,
    candidate_fingerprint: Option<u64>,
    candidate_secure: Option<bool>,
) -> bool {
    same_process_and_window
        && candidate_secure == Some(false)
        && start_fingerprint.is_some()
        && start_fingerprint == candidate_fingerprint
}

#[cfg(test)]
#[path = "delivery_replacement_editor_tests.rs"]
mod replacement_editor_tests;

/// Time left in the 200 ms restore quiet window given the last receipt.
pub fn paste_restore_quiet_remaining(
    last_receipt: Option<std::time::Instant>,
    now: std::time::Instant,
) -> std::time::Duration {
    const QUIET: std::time::Duration = std::time::Duration::from_millis(200);
    match last_receipt {
        Some(last) => (last + QUIET).saturating_duration_since(now),
        None => std::time::Duration::ZERO,
    }
}

/// Shared paste success rule. No shipping dictation app gates success on
/// AX/UIA readback; the failure signal is that no application requested the
/// promised clipboard data. Success = keystroke sent AND (clipboard receipt
/// OR (target key event seen AND readback confirmed)). Readback never
/// delays a paste that has a receipt.
pub fn paste_outcome(
    keystroke_sent: bool,
    receipt_observed: bool,
    target_event_seen: bool,
    readback_confirmed: bool,
) -> Result<(), CopyFallbackReason> {
    if !keystroke_sent {
        return Err(CopyFallbackReason::PasteFailed);
    }
    if receipt_observed || (target_event_seen && readback_confirmed) {
        Ok(())
    } else {
        Err(CopyFallbackReason::Other(
            "paste_receipt_unconfirmed".to_string(),
        ))
    }
}

/// A terminal target whose paste could not be proven AND that has no way to
/// prove it: no clipboard receipt came inside the short grace window and no
/// readback baseline exists (the terminal exposes no UIA text, or the bound-pane
/// baseline did not finish inside its pre-paste budget). Holding the pill for
/// the full terminal readback deadline cannot change the answer, so the paste
/// is reported as sent-with-unknown consumption: no clip card, no Enter (Return
/// needs proven consumption), transcript left staged on the clipboard.
/// A terminal WITH a baseline keeps the full race and, on a miss, the
/// unconfirmed recovery card.
pub fn terminal_paste_has_no_confirm_channel(
    terminal_target: bool,
    readback_baseline_available: bool,
) -> bool {
    terminal_target && !readback_baseline_available
}

#[cfg(test)]
mod paste_outcome_tests {
    use super::*;

    #[test]
    fn receipt_alone_is_success() {
        assert!(paste_outcome(true, true, false, false).is_ok());
    }

    #[test]
    fn event_plus_readback_without_receipt_is_success() {
        assert!(paste_outcome(true, false, true, true).is_ok());
    }

    #[test]
    fn readback_without_target_event_or_receipt_is_unconfirmed() {
        assert_eq!(
            paste_outcome(true, false, false, true),
            Err(CopyFallbackReason::Other(
                "paste_receipt_unconfirmed".to_string()
            ))
        );
    }

    #[test]
    fn no_receipt_no_readback_is_unconfirmed() {
        assert!(paste_outcome(true, false, true, false).is_err());
    }

    #[test]
    fn terminal_without_baseline_has_no_confirm_channel() {
        assert!(terminal_paste_has_no_confirm_channel(true, false));
    }

    #[test]
    fn terminal_with_baseline_keeps_its_readback_channel() {
        assert!(!terminal_paste_has_no_confirm_channel(true, true));
    }

    #[test]
    fn non_terminal_targets_never_get_the_unknown_outcome() {
        assert!(!terminal_paste_has_no_confirm_channel(false, false));
        assert!(!terminal_paste_has_no_confirm_channel(false, true));
    }

    #[test]
    fn no_channel_rule_never_overrides_a_failed_keystroke() {
        // The unknown outcome is only consulted after a keystroke was sent.
        assert_eq!(
            paste_outcome(false, false, true, false),
            Err(CopyFallbackReason::PasteFailed)
        );
    }

    #[test]
    fn unsent_keystroke_never_succeeds() {
        assert_eq!(
            paste_outcome(false, true, true, true),
            Err(CopyFallbackReason::PasteFailed)
        );
    }

    #[test]
    fn quiet_window_counts_from_last_receipt() {
        let now = std::time::Instant::now();
        assert_eq!(
            paste_restore_quiet_remaining(None, now),
            std::time::Duration::ZERO
        );
        let rem = paste_restore_quiet_remaining(Some(now), now);
        assert_eq!(rem, std::time::Duration::from_millis(200));
        let later = now + std::time::Duration::from_millis(500);
        assert_eq!(
            paste_restore_quiet_remaining(Some(now), later),
            std::time::Duration::ZERO
        );
    }
}
