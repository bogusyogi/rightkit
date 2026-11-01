// Shared imports live on the module root (runtime.rs); `use super::*` keeps
// this file on the same namespace the include!-based layout provided.
#[allow(unused_imports)]
use super::*;

pub fn engine_state_snapshot_is_transcribing_session(state: &str, session_id: &str) -> bool {
    state == format!("Transcribing {{ session_id: {session_id:?} }}")
}

pub fn sidecar_file_transcribe_timeout(path: &std::path::Path) -> Duration {
    let size_mb = std::fs::metadata(path)
        .map(|m| m.len() / (1024 * 1024))
        .unwrap_or(0);
    let timeout_secs = size_mb
        .saturating_mul(2)
        .saturating_add(SIDECAR_FILE_TRANSCRIBE_TIMEOUT_MIN_SECS)
        .clamp(
            SIDECAR_FILE_TRANSCRIBE_TIMEOUT_MIN_SECS,
            SIDECAR_FILE_TRANSCRIBE_TIMEOUT_MAX_SECS,
        );
    Duration::from_secs(timeout_secs)
}

#[derive(Debug, PartialEq, Eq)]
pub enum SidecarControlTail {
    Deliver,
    Reset,
}

pub fn apply_sidecar_control_tail(
    transcript: &mut String,
    want_enter: &mut bool,
) -> SidecarControlTail {
    let Some(command) = rightkit_text_polish::text_pipeline::parse_control_command(transcript)
    else {
        return SidecarControlTail::Deliver;
    };
    use rightkit_text_polish::text_pipeline::ControlIntent;
    match command.intent {
        ControlIntent::Cancel => SidecarControlTail::Reset,
        ControlIntent::Stop | ControlIntent::Send => {
            *transcript = command.clean_text;
            if matches!(command.intent, ControlIntent::Send) {
                *want_enter = true;
            }
            SidecarControlTail::Deliver
        }
    }
}

#[doc(hidden)]
pub fn apply_sidecar_control_tail_for_test(input: &str) -> (bool, String, bool) {
    let mut transcript = input.to_string();
    let mut want_enter = false;
    let action = apply_sidecar_control_tail(&mut transcript, &mut want_enter);
    (
        action == SidecarControlTail::Deliver,
        transcript,
        want_enter,
    )
}

#[cfg(debug_assertions)]
pub fn assert_capture_warning_contract_for_test() {
    let (supervisor, _events) = EngineSupervisor::new(false);
    let mut runtime = EngineRuntime {
        state: SharedState::new(AppState::Transcribing {
            session_id: "capture-test".into(),
        }),
        sidecar: SidecarHandle {
            supervisor: Arc::new(supervisor),
            active_session: Arc::new(StdMutex::new(Some("capture-test".into()))),
            engine_version: Arc::new(StdMutex::new(None)),
            state: SharedState::new(AppState::Armed),
            info_ready: Arc::new(AtomicBool::new(false)),
        },
        history: Some(Box::new(
            rightkit_transcript_library::history::MemoryHistoryStore::default(),
        )),
        history_generation_seen: history_storage_generation(),
        recent_cache: Vec::new(),
        last_delivery: None,
        sequence: 1,
        active_session: Some("capture-test".into()),
        stop_target: None,
        stop_focus_pin: None,
        start_capture_rx: None,
        start_target: None,
        start_dispatch_ready: None,
        start_focus_pin: None,
        start_context: None,
        start_content_fingerprint: None,
        pending_stop_context: PendingStopContextLatch::default(),
        pending_send_enter: false,
        event_rx: None,
        last_sent_engine_config: None,
        last_sent_vocab: None,
        last_synced_sidecar_generation: 0,
        cancel_to_history_session: Some("capture-test".into()),
        draft_throttle: None,
    };
    let mut observed = Vec::new();
    let state = runtime
        .accept_sidecar_transcript(
            "capture-test".into(),
            "Captured speech".into(),
            false,
            None,
            Some(3000),
            true,
            Some("No microphone audio detected".into()),
            None,
            None,
            || panic!("capture failure must never paste"),
            |state| observed.push(state.clone()),
        )
        .unwrap();
    assert!(runtime.active_session.is_none());
    assert_eq!(runtime.recent_cache.len(), 1);
    assert_eq!(runtime.recent_cache[0].transcript, "Captured speech");
    assert!(
        matches!(state, AppState::Error { message, last_transcript: Some(text) }
        if message == "No microphone audio detected" && text == "Captured speech")
    );
    assert_eq!(
        observed.len(),
        1,
        "history and warning have one terminal state"
    );
    // A delayed duplicate cannot alter a newer recording or add history.
    runtime.active_session = Some("next-session".into());
    runtime.state.set(AppState::Recording {
        session_id: "next-session".into(),
    });
    let state = runtime
        .accept_sidecar_transcript(
            "capture-test".into(),
            "Duplicate".into(),
            false,
            None,
            None,
            true,
            Some("stale warning".into()),
            None,
            None,
            || panic!("stale result must not paste"),
            |_| panic!("stale result must not emit"),
        )
        .unwrap();
    assert!(matches!(state, AppState::Recording { session_id } if session_id == "next-session"));
    assert_eq!(runtime.recent_cache.len(), 1);
}

/// Native QA harness calls the production recording-result entry, with only
/// provider receipts simulated. Compiled out of shipped release binaries.
#[cfg(debug_assertions)]
pub struct RecordingDeliveryProbe {
    runtime: EngineRuntime,
    bound: bool,
    expected: (u32, isize),
}

#[cfg(debug_assertions)]
impl RecordingDeliveryProbe {
    pub fn capture(mode: &str, expected: (u32, isize)) -> Result<Self, String> {
        let (initial, _) = crate::host::host().capture_stop_destination();
        if initial.process_id != Some(expected.0) || initial.window_handle != Some(expected.1) {
            return Err(format!(
                "probe start is outside expected fixture: expected={expected:?} observed={:?}/{:?}",
                initial.process_id, initial.window_handle
            ));
        }
        let (start_target, start_pin, mut context, fingerprint) =
            crate::host::host().capture_start_destination(initial);
        let bound = mode == "bound";
        if bound
            && (context.state != rightkit_dictation_core::engine::RecordingContextState::NonEmpty
                || fingerprint.is_none()
                || start_pin.is_none())
        {
            return Err("bound probe requires readable nonempty start editor".into());
        }
        if mode == "unreadable" {
            context.state = rightkit_dictation_core::engine::RecordingContextState::Unreadable;
            context.field_text = None;
        }
        let (supervisor, _events) = EngineSupervisor::new(false);
        let runtime = EngineRuntime {
            state: SharedState::new(AppState::Transcribing {
                session_id: "paste-probe".into(),
            }),
            sidecar: SidecarHandle {
                supervisor: Arc::new(supervisor),
                active_session: Arc::new(StdMutex::new(Some("paste-probe".into()))),
                engine_version: Arc::new(StdMutex::new(None)),
                state: SharedState::new(AppState::Armed),
                info_ready: Arc::new(AtomicBool::new(false)),
            },
            history: Some(Box::new(
                rightkit_transcript_library::history::MemoryHistoryStore::default(),
            )),
            history_generation_seen: history_storage_generation(),
            recent_cache: Vec::new(),
            last_delivery: None,
            sequence: 1,
            active_session: Some("paste-probe".into()),
            stop_target: None,
            stop_focus_pin: None,
            start_capture_rx: None,
            start_dispatch_ready: None,
            start_target: Some(start_target),
            start_focus_pin: start_pin,
            start_context: Some(context),
            start_content_fingerprint: fingerprint,
            pending_stop_context: PendingStopContextLatch::default(),
            pending_send_enter: false,
            event_rx: None,
            last_sent_engine_config: None,
            last_sent_vocab: None,
            last_synced_sidecar_generation: 0,
            cancel_to_history_session: None,
            draft_throttle: None,
        };
        Ok(Self {
            runtime,
            bound,
            expected,
        })
    }

    pub fn deliver(mut self, text: String, send: bool) -> Result<DeliveryRecord, String> {
        self.runtime.capture_stop_destination();
        if !self.runtime.stop_target.as_ref().is_some_and(|target| {
            target.process_id == Some(self.expected.0)
                && target.window_handle == Some(self.expected.1)
        }) {
            return Err("probe stop is outside expected fixture".into());
        }
        self.runtime.accept_sidecar_transcript(
            "paste-probe".into(),
            text,
            send,
            None,
            Some(1_000),
            false,
            None,
            Some(true),
            Some(self.bound),
            || {},
            |_| {},
        )?;
        self.runtime
            .last_delivery
            .take()
            .ok_or_else(|| "production entry returned no record".into())
    }
}

#[cfg(test)]
mod tests {
    use super::{apply_sidecar_control_tail, SidecarControlTail};

    #[test]
    fn shell_strips_sidecar_control_tail_before_delivery() {
        let mut text = "Keep this Zephyr stop".to_string();
        let mut enter = false;
        assert_eq!(
            apply_sidecar_control_tail(&mut text, &mut enter),
            SidecarControlTail::Deliver
        );
        assert_eq!(text, "Keep this");
        assert!(!enter);
    }

    #[test]
    fn shell_preserves_send_intent_from_sidecar_control_tail() {
        let mut text = "Ship it Zephyr send".to_string();
        let mut enter = false;
        assert_eq!(
            apply_sidecar_control_tail(&mut text, &mut enter),
            SidecarControlTail::Deliver
        );
        assert_eq!(text, "Ship it");
        assert!(enter);
    }

    #[test]
    fn shell_cancel_tail_resets_without_delivery() {
        let mut text = "Ignore this Zephyr cancel".to_string();
        let mut enter = false;
        assert_eq!(
            apply_sidecar_control_tail(&mut text, &mut enter),
            SidecarControlTail::Reset
        );
        assert_eq!(text, "Ignore this Zephyr cancel");
        assert!(!enter);
    }
}
