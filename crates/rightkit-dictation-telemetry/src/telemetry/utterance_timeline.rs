//! Shell-side per-utterance timeline (`utterance_timeline` telemetry event).
//!
//! One dictation is in its stop/delivery phase at a time, so one
//! process-global slot suffices. The origin is an `Instant` taken at the stop
//! trigger (or, for an engine-initiated hands-free stop the shell never drove,
//! at `final_received`). Every offset is milliseconds as f64 from that origin,
//! never wall time, and first-write-wins so an idempotent call site cannot move
//! a stage. The event carries no text content. It rides the existing
//! diagnostics gate (`append_or_stderr`) exactly like `delivery_recorded`.

use std::cell::RefCell;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use rightkit_dictation_core::engine::EngineTimeline;
use rightkit_transcript_library::delivery::{DeliveryOutcome, DeliveryRecord};
use serde::Serialize;
use serde_json::{json, Value};

/// A trigger hint older than this is a different gesture, not this stop.
const HINT_MAX_AGE_MS: u128 = 1_500;
/// A bound timeline younger than this is still in flight; a second stop
/// request does not replace it.
const LIVE_KEEP_MS: u128 = 5_000;

#[derive(Debug, Clone, Default, Serialize)]
pub struct ShellOffsets {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gate_acquired_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_sent_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_accepted_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination_captured_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pill_processing_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub final_received_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accept_started_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivery_started_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paste_issued_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paste_confirmed_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivery_recorded_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pill_pasted_ms: Option<f64>,
}

struct Timeline {
    session_id: Option<String>,
    origin: Instant,
    /// "trigger" when the origin is the stop gesture, "final_received" when
    /// the shell never drove the stop (engine hands-free auto-stop).
    origin_kind: &'static str,
    trigger_kind: String,
    shell: ShellOffsets,
    engine: Option<EngineTimeline>,
    delivery: Option<Value>,
    recording_ms: Option<u64>,
}

static SLOT: OnceLock<Mutex<Option<Timeline>>> = OnceLock::new();

fn slot() -> &'static Mutex<Option<Timeline>> {
    SLOT.get_or_init(|| Mutex::new(None))
}

fn lock() -> std::sync::MutexGuard<'static, Option<Timeline>> {
    slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

thread_local! {
    static HINT: RefCell<Option<(Instant, String)>> = const { RefCell::new(None) };
}

/// Remember the gesture that is about to call a stop on THIS thread. Called
/// from `log_input_trigger`, which every trigger site already calls on the
/// thread that then runs the stop.
pub fn note_trigger(kind: &str) {
    HINT.with(|hint| *hint.borrow_mut() = Some((Instant::now(), kind.to_string())));
}

/// A stop (or cancel-to-history) begins now: start a fresh timeline, using the
/// recent trigger hint for the origin and kind when there is one.
pub fn begin_stop(default_kind: &str) {
    let hint = HINT.with(|hint| hint.borrow_mut().take());
    let (origin, kind) = match hint {
        Some((at, kind))
            if at.elapsed().as_millis() <= HINT_MAX_AGE_MS && !kind.ends_with("_start") =>
        {
            (at, kind)
        }
        _ => (Instant::now(), default_kind.to_string()),
    };
    let mut guard = lock();
    // A redundant stop (hotkey + click race) must not discard the live,
    // already-bound timeline of the stop that is actually in flight.
    if guard.as_ref().is_some_and(|live| {
        live.session_id.is_some()
            && live.origin_kind == "trigger"
            && live.origin.elapsed().as_millis() < LIVE_KEEP_MS
    }) {
        return;
    }
    *guard = Some(Timeline {
        session_id: None,
        origin,
        origin_kind: "trigger",
        trigger_kind: kind,
        shell: ShellOffsets::default(),
        engine: None,
        delivery: None,
        recording_ms: None,
    });
}

/// The stop command for `session_id` is about to be sent: bind it.
pub fn bind_session(session_id: &str) {
    if let Some(timeline) = lock().as_mut() {
        if timeline.session_id.is_none() {
            timeline.session_id = Some(session_id.to_string());
        }
    }
}

/// Record the elapsed offset for one stage (first write wins). No-op when no
/// timeline is open.
pub fn mark(field: fn(&mut ShellOffsets) -> &mut Option<f64>) {
    if let Some(timeline) = lock().as_mut() {
        let elapsed_ms = timeline.origin.elapsed().as_secs_f64() * 1_000.0;
        field(&mut timeline.shell).get_or_insert(elapsed_ms);
    }
}

/// The engine's final (or engine-made delivery) for `session_id` arrived.
/// Attaches the engine timeline. When the shell never drove this stop the
/// timeline starts here and is labelled as such.
pub fn final_received(session_id: &str, engine: Option<EngineTimeline>) {
    let mut guard = lock();
    let matches = guard
        .as_ref()
        .is_some_and(|timeline| timeline.session_id.as_deref() == Some(session_id));
    if !matches {
        *guard = Some(Timeline {
            session_id: Some(session_id.to_string()),
            origin: Instant::now(),
            origin_kind: "final_received",
            trigger_kind: "engine_auto_stop".to_string(),
            shell: ShellOffsets::default(),
            engine: None,
            delivery: None,
            recording_ms: None,
        });
    }
    if let Some(timeline) = guard.as_mut() {
        let elapsed_ms = timeline.origin.elapsed().as_secs_f64() * 1_000.0;
        timeline.shell.final_received_ms.get_or_insert(elapsed_ms);
        if timeline.engine.is_none() {
            timeline.engine = engine;
        }
    }
}

/// Copy the delivery summary and stamp `delivery_recorded_ms`. Called at the
/// same point `delivery_recorded` is logged.
pub fn note_delivery(record: &DeliveryRecord) {
    let outcome = match &record.outcome {
        DeliveryOutcome::Pasted => json!({ "kind": "paste_keystroke_sent" }),
        outcome => json!(outcome),
    };
    let delivery = json!({
        "method": record.method,
        "outcome": outcome,
        "elapsed_ms": record.elapsed_ms,
        "steps": record.delivery_timing,
    });
    if let Some(timeline) = lock().as_mut() {
        let elapsed_ms = timeline.origin.elapsed().as_secs_f64() * 1_000.0;
        timeline
            .shell
            .delivery_recorded_ms
            .get_or_insert(elapsed_ms);
        timeline.delivery = Some(delivery);
        timeline.recording_ms = record.recording_ms;
    }
}

/// Emit the `utterance_timeline` event for `session_id` once and close the
/// slot. No-op when the open timeline belongs to another session.
pub fn emit(session_id: &str) {
    let timeline = {
        let mut guard = lock();
        let owned = guard
            .as_ref()
            .is_some_and(|timeline| timeline.session_id.as_deref() == Some(session_id));
        if owned {
            guard.take()
        } else {
            None
        }
    };
    let Some(timeline) = timeline else { return };
    let platform = if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "other"
    };
    super::append_or_stderr(json!({
        "event": "utterance_timeline",
        "level": "info",
        "session_id": session_id,
        "platform": platform,
        "trigger_kind": timeline.trigger_kind,
        "origin": timeline.origin_kind,
        "recording_ms": timeline.recording_ms,
        "shell": timeline.shell,
        "engine": timeline.engine,
        "delivery": timeline.delivery,
    }));
}
