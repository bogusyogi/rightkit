#[allow(unused_imports)]
use super::*;
#[allow(unused_imports)]
use super::{capture_recovery::*, worker_api::*, worker_clock::*};

/// Wave W1 (docs/WINDOWS_IMPLEMENTATION_SET.md): endpoint & power lifecycle
/// adapter. `_guard` is held for the worker's lifetime purely so its `Drop`
/// unregisters every callback & releases COM at worker shutdown; guard and
/// receiver are `None` on non-Windows targets and on a registration failure.
pub(crate) struct WindowsLifecycle {
    _guard: Option<crate::windows_audio_lifecycle::WindowsAudioLifecycle>,
    rx: Option<Receiver<crate::windows_audio_lifecycle::LifecycleEvent>>,
    generation: u64,
    /// Windows suspended the microphone; recording and wake starts are refused
    /// until resume.
    pub(crate) suspended: bool,
}

impl WindowsLifecycle {
    pub(crate) fn register(wait_signal: Arc<WorkerWaitSignal>) -> Self {
        let notify: crate::windows_audio_lifecycle::LifecycleNotify =
            Arc::new(move || wait_signal.notify());
        let (guard, rx) =
            match crate::windows_audio_lifecycle::try_register_with_notify(Some(notify)) {
                Some((guard, rx)) => (Some(guard), Some(rx)),
                None => (None, None),
            };
        Self {
            _guard: guard,
            rx,
            generation: 0,
            suspended: false,
        }
    }

    /// Drain the endpoint queue plus the latest lock-free power transition
    /// since the last pass, then coalesce the whole burst into exactly one
    /// recovery generation. Returns the recovery outcome when one ran; the
    /// caller applies it to the recording and wake state.
    pub(crate) fn poll(
        &mut self,
        capture: &mut Option<WorkerCapture>,
        last_capture_route: &mut Option<CaptureRouteSignature>,
        last_saved_device: &mut Option<String>,
        recording: bool,
        wake_listening: bool,
        decision_clock: &dyn WorkerClock,
    ) -> Option<WindowsLifecycleRecoveryOutcome> {
        let lifecycle_rx = self.rx.as_ref()?;
        let mut burst: Vec<crate::windows_audio_lifecycle::LifecycleEvent> = Vec::new();
        while let Ok(event) = lifecycle_rx.try_recv() {
            burst.push(event);
        }
        if let Some(power_event) = self
            ._guard
            .as_ref()
            .and_then(|lifecycle| lifecycle.take_pending_power_event())
        {
            burst.push(power_event);
        }
        // Nothing arrived: skip the settings read and device inventory. This
        // runs every worker pass.
        if burst.is_empty() {
            return None;
        }
        let saved = crate::settings::input_device();
        let needs_capture_inventory = capture.is_none()
            && !self.suspended
            && burst.iter().any(|event| {
                matches!(
                    event.cause,
                    crate::windows_audio_lifecycle::LifecycleCause::DeviceStateChanged
                        | crate::windows_audio_lifecycle::LifecycleCause::PropertyChanged
                )
            });
        let available_capture_endpoint_ids: Vec<String> = if needs_capture_inventory {
            list_input_devices()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|device| device.platform_id)
                .collect()
        } else {
            Vec::new()
        };
        burst.retain(|event| {
            windows_audio_lifecycle_event_relevant(
                event,
                saved.as_deref(),
                capture.is_some(),
                last_capture_route.as_ref(),
                &available_capture_endpoint_ids,
                self.suspended,
            )
        });
        let primary_cause = windows_audio_lifecycle_primary_cause(&burst)?;
        self.generation = self.generation.wrapping_add(1);
        let suspend_only = primary_cause == crate::windows_audio_lifecycle::LifecycleCause::Suspend;
        if suspend_only {
            self.suspended = true;
        } else if primary_cause == crate::windows_audio_lifecycle::LifecycleCause::Resume {
            self.suspended = false;
        }
        let selection_label = if saved.is_some() {
            "explicit"
        } else {
            "system_default"
        };
        let recording_intent = recording || wake_listening;
        let outcome = run_windows_audio_lifecycle_recovery(
            saved.as_deref(),
            capture,
            last_capture_route,
            recording,
            wake_listening,
            suspend_only,
            decision_clock,
        );
        *last_saved_device = saved;
        emit_windows_capture_lifecycle(windows_capture_lifecycle_payload(
            self.generation,
            primary_cause.as_diagnostic_str(),
            selection_label,
            outcome.route.as_ref(),
            recording_intent,
            outcome.retry_count,
            outcome.first_buffer_ms,
            outcome.fallback_reason.as_deref(),
            outcome.outcome,
        ));
        if windows_recovery_failed(&outcome) {
            tracing::error!(
                generation = self.generation,
                cause = primary_cause.as_diagnostic_str(),
                outcome = outcome.outcome,
                "windows audio lifecycle recovery did not restore a live stream"
            );
        }
        Some(outcome)
    }
}

pub(crate) fn windows_recovery_failed(outcome: &WindowsLifecycleRecoveryOutcome) -> bool {
    matches!(
        outcome.outcome,
        "failed" | "saved_device_unavailable" | "permission_blocked"
    )
}
