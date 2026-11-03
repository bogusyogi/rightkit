// Pure, Tauri-free start-sequence logic (issue #160, #172). Declared in
// app_lib.rs and re-declared under cfg(test) in lib.rs so these tests run
// in the lib unit-test target.
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
pub(crate) trait StartSteps {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct RecordingStart {
        log: Vec<&'static str>,
        fail_backend: bool,
        fail_live: bool,
        committed: bool,
    }

    impl StartSteps for RecordingStart {
        fn observe_target(&mut self) {
            self.log.push("observe_target");
        }
        fn allocate_interaction(&mut self) -> Result<(), String> {
            self.log.push("allocate_interaction");
            Ok(())
        }
        fn show_optimistic_recording(&mut self) {
            self.log.push("visual_recording");
        }
        fn backend_start(&mut self) -> Result<(), String> {
            self.log.push("backend_start");
            if self.fail_backend {
                Err("rejected".to_string())
            } else {
                Ok(())
            }
        }
        fn confirm_capture_live(&mut self) -> Result<(), String> {
            self.log.push("confirm_capture_live");
            if self.fail_live {
                Err("not live".to_string())
            } else {
                Ok(())
            }
        }
        fn acknowledge_recording(&mut self) -> Result<AppState, String> {
            self.log.push("commit_recording");
            self.committed = true;
            Ok(AppState::Armed)
        }
    }

    #[test]
    fn start_sequence_orders_target_visual_interaction_backend_commit() {
        let mut steps = RecordingStart::default();
        run_start_sequence(&mut steps, |op| op()).unwrap();
        assert_eq!(
            steps.log,
            [
                "observe_target",
                "visual_recording",
                "allocate_interaction",
                "backend_start",
                "confirm_capture_live",
                "commit_recording"
            ]
        );
    }

    #[test]
    fn start_without_live_capture_never_reaches_recording() {
        let mut steps = RecordingStart {
            fail_live: true,
            ..Default::default()
        };
        assert_eq!(
            run_start_sequence(&mut steps, |op| op()),
            Err("not live".to_string())
        );
        assert!(!steps.committed);
        assert_eq!(steps.log.last(), Some(&"confirm_capture_live"));
    }

    #[test]
    fn await_capture_live_returns_when_frames_arrive() {
        let mut polls = 0;
        let result = await_capture_live(
            || {
                polls += 1;
                if polls < 3 {
                    CaptureLiveness::Pending
                } else {
                    CaptureLiveness::Live
                }
            },
            |_| {},
            CAPTURE_LIVE_WINDOW,
            CAPTURE_LIVE_POLL,
        );
        assert_eq!(result, Ok(()));
        assert_eq!(polls, 3);
    }

    #[test]
    fn await_capture_live_is_bounded_and_message_maps_to_mic_error() {
        let mut slept = Duration::ZERO;
        let err = await_capture_live(
            || CaptureLiveness::Pending,
            |d| slept += d,
            CAPTURE_LIVE_WINDOW,
            CAPTURE_LIVE_POLL,
        )
        .unwrap_err();
        assert!(slept >= CAPTURE_LIVE_WINDOW && slept < CAPTURE_LIVE_WINDOW + CAPTURE_LIVE_POLL);
        assert!(crate::dictation_error::is_microphone_device_error(&err));
        assert!(crate::dictation_error::label_for_error(&err).starts_with("Microphone unavailable"));
    }

    #[test]
    fn await_capture_live_surfaces_reported_failure_immediately() {
        let err = await_capture_live(
            || CaptureLiveness::Failed("capture open: denied".into()),
            |_| panic!("must not sleep after a reported failure"),
            CAPTURE_LIVE_WINDOW,
            CAPTURE_LIVE_POLL,
        )
        .unwrap_err();
        assert_eq!(err, "capture open: denied");
    }

    #[test]
    fn liveness_signal_is_per_session_and_live_is_sticky() {
        assert_eq!(capture_liveness("s-none"), CaptureLiveness::Pending);
        note_capture_failed("s-a", "boom");
        assert_eq!(
            capture_liveness("s-a"),
            CaptureLiveness::Failed("boom".into())
        );
        assert_eq!(capture_liveness("s-b"), CaptureLiveness::Pending);
        note_capture_live("s-a");
        assert_eq!(capture_liveness("s-a"), CaptureLiveness::Live);
        note_capture_failed("s-a", "late");
        assert_eq!(capture_liveness("s-a"), CaptureLiveness::Live);
    }

    #[test]
    fn rejected_backend_start_never_reaches_recording() {
        let mut steps = RecordingStart {
            fail_backend: true,
            ..Default::default()
        };
        assert!(run_start_sequence(&mut steps, |op| op()).is_err());
        assert!(!steps.log.contains(&"commit_recording"));
        assert!(
            !steps.committed,
            "rejected start must never commit Recording state"
        );
        assert!(steps.log.contains(&"visual_recording"));
        assert_eq!(steps.log.last(), Some(&"backend_start"));
    }

    #[test]
    fn delivery_recorded_payload_carries_target_or_null() {
        let none = delivery_recorded_payload("d1", None);
        assert_eq!(none["delivery_id"], "d1");
        assert!(none["target"].is_null());
        let mut t = rightkit_transcript_library::delivery::TargetSnapshot::empty();
        t.process_name = Some("Slack".into());
        t.window_title = Some("general".into());
        let some = delivery_recorded_payload("d2", Some(&t));
        assert_eq!(some["target"]["process_name"], "Slack");
        assert_eq!(some["target"]["window_title"], "general");
    }
}
