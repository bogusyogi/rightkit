// Pure, Tauri-free start-sequence logic (issue #160, #172). Declared in
// lib.rs and app_lib.rs.
use rightkit_dictation_core::state::AppState;
use std::sync::Mutex;
use std::time::Duration;

/// Bound on how long a start waits for the capture stream to report live
/// frames (issue #156). Covers a Bluetooth settle plus one cold reopen in the
/// worker (2 x 1.5 s) with margin; a healthy start sees its first level in
/// ~100-250 ms.
pub const CAPTURE_LIVE_WINDOW: Duration = Duration::from_millis(4_000);
pub const CAPTURE_LIVE_POLL: Duration = Duration::from_millis(15);

/// What the shell knows about capture for one session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureLiveness {
    Pending,
    Live,
    Failed(String),
}

static CAPTURE_LIVENESS: Mutex<Option<(String, CaptureLiveness)>> = Mutex::new(None);

/// First frames for `session_id` reached the worker pump (a `RecordingLevel`
/// event exists only after validated PCM was ingested).
pub fn note_capture_live(session_id: &str) {
    if let Ok(mut slot) = CAPTURE_LIVENESS.lock() {
        match slot.as_ref() {
            Some((id, CaptureLiveness::Live)) if id == session_id => {}
            _ => *slot = Some((session_id.to_string(), CaptureLiveness::Live)),
        }
    }
}

/// The engine reported an error for `session_id`. A session already proven
/// live is unaffected: mid-recording failures have their own handling.
pub fn note_capture_failed(session_id: &str, message: &str) {
    if let Ok(mut slot) = CAPTURE_LIVENESS.lock() {
        match slot.as_ref() {
            Some((id, CaptureLiveness::Live)) if id == session_id => {}
            _ => {
                *slot = Some((
                    session_id.to_string(),
                    CaptureLiveness::Failed(message.to_string()),
                ))
            }
        }
    }
}

pub fn capture_liveness(session_id: &str) -> CaptureLiveness {
    CAPTURE_LIVENESS
        .lock()
        .ok()
        .and_then(|slot| match slot.as_ref() {
            Some((id, state)) if id == session_id => Some(state.clone()),
            _ => None,
        })
        .unwrap_or(CaptureLiveness::Pending)
}

/// Message for a start whose capture never reported frames. Contains
/// "capture read" and "microphone permission" so `label_for_error` shows the
/// "Microphone unavailable" pill Error.
pub fn capture_not_live_message(window: Duration) -> String {
    format!(
        "capture read: no live audio frames within {} ms (microphone unavailable or microphone permission not granted)",
        window.as_millis()
    )
}

/// Poll `probe` until capture is live, an error is reported, or `window`
/// elapses. `sleep` is injected so the bound is testable without a clock.
pub fn await_capture_live(
    mut probe: impl FnMut() -> CaptureLiveness,
    mut sleep: impl FnMut(Duration),
    window: Duration,
    step: Duration,
) -> Result<(), String> {
    let mut waited = Duration::ZERO;
    loop {
        match probe() {
            CaptureLiveness::Live => return Ok(()),
            CaptureLiveness::Failed(message) => return Err(message),
            CaptureLiveness::Pending => {}
        }
        if waited >= window {
            return Err(capture_not_live_message(window));
        }
        sleep(step);
        waited += step;
    }
}

/// Payload for the `delivery-recorded` event. `target` is `{process_name,
/// window_title}` when the delivered target is known, else JSON null so the
/// pill falls back to its history scan.
pub fn delivery_recorded_payload(
    delivery_id: &str,
    target: Option<&rightkit_transcript_library::delivery::TargetSnapshot>,
) -> serde_json::Value {
    let target = target.map(
        |t| serde_json::json!({ "process_name": t.process_name, "window_title": t.window_title }),
    );
    serde_json::json!({ "delivery_id": delivery_id, "target": target })
}

/// Stages of the start sequence (issue #160). Production and tests implement
/// the same trait; the order lives only in `run_start_sequence`.
pub trait StartSteps {
    /// Observe the hotkey-time target. Evidence only; never a destination.
    fn observe_target(&mut self);
    /// Allocate the session/interaction (prepare under the runtime lock).
    fn allocate_interaction(&mut self) -> Result<(), String>;
    /// Optimistic Recording visual (not state, not the Starting spinner).
    fn show_optimistic_recording(&mut self);
    /// Dispatch the backend start and wait for acceptance.
    fn backend_start(&mut self) -> Result<(), String>;
    /// Wait (bounded) for the capture stream to report live frames. An `Err`
    /// must leave no active session or running capture behind.
    fn confirm_capture_live(&mut self) -> Result<(), String>;
    /// Commit `Recording` after backend acceptance.
    fn acknowledge_recording(&mut self) -> Result<AppState, String>;
}

/// The single start entry point: target observation, optimistic Recording
/// visual, then inside `gate` interaction allocation, backend start and the
/// Recording commit. The visual runs before the gate so it never waits on the
/// runtime lock, AX target capture, or sidecar work.
pub fn run_start_sequence<S: StartSteps>(
    steps: &mut S,
    gate: impl FnOnce(&mut dyn FnMut() -> Result<AppState, String>) -> Result<AppState, String>,
) -> Result<AppState, String> {
    steps.observe_target();
    steps.show_optimistic_recording();
    gate(&mut || {
        steps.allocate_interaction()?;
        steps.backend_start()?;
        steps.confirm_capture_live()?;
        steps.acknowledge_recording()
    })
}

#[allow(dead_code)]
pub fn should_send_enter_after_stop(state: &AppState) -> bool {
    matches!(
        state,
        AppState::Pasted {
            send_enter: true,
            ..
        }
    )
}
