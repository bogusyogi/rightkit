use super::*;

fn event(sequence: u64, cause: LifecycleCause, endpoint_id: Option<&str>) -> LifecycleEvent {
    LifecycleEvent {
        sequence,
        cause,
        observed_at: std::time::Instant::now(),
        endpoint_id: endpoint_id.map(str::to_string),
    }
}

#[test]
fn suspend_wins_primary_cause_over_endpoint_events_in_the_same_burst() {
    let burst = [
        event(1, LifecycleCause::DeviceStateChanged, Some("render-x")),
        event(2, LifecycleCause::Suspend, None),
        event(3, LifecycleCause::DefaultChanged, Some("capture-y")),
    ];
    assert_eq!(
        windows_audio_lifecycle_primary_cause(&burst),
        Some(LifecycleCause::Suspend)
    );
}

#[test]
fn device_removed_outranks_state_and_property_changes() {
    let burst = [
        event(1, LifecycleCause::PropertyChanged, Some("capture-a")),
        event(2, LifecycleCause::DeviceRemoved, Some("capture-a")),
        event(3, LifecycleCause::DeviceStateChanged, Some("capture-a")),
    ];
    assert_eq!(
        windows_audio_lifecycle_primary_cause(&burst),
        Some(LifecycleCause::DeviceRemoved)
    );
}

#[test]
fn empty_burst_has_no_primary_cause() {
    assert_eq!(windows_audio_lifecycle_primary_cause(&[]), None);
}

#[test]
fn last_power_transition_wins_within_one_burst() {
    let resume_last = [
        event(1, LifecycleCause::Suspend, None),
        event(2, LifecycleCause::Resume, None),
    ];
    let suspend_last = [
        event(3, LifecycleCause::Resume, None),
        event(4, LifecycleCause::Suspend, None),
    ];
    assert_eq!(
        windows_audio_lifecycle_primary_cause(&resume_last),
        Some(LifecycleCause::Resume)
    );
    assert_eq!(
        windows_audio_lifecycle_primary_cause(&suspend_last),
        Some(LifecycleCause::Suspend)
    );
}

#[test]
fn endpoint_filter_preserves_selected_mic_and_ignores_unrelated_devices() {
    let route = CaptureRouteSignature {
        device_id: 0,
        platform_id: Some("capture-selected".into()),
        uses_system_default: false,
        name: "Selected".into(),
        native_rate: 48_000,
        channels: 1,
        sample_format: "F32".into(),
        transport: CaptureTransport::Usb,
        form_factor: rightkit_audio::capture::CaptureFormFactor::Microphone,
    };
    assert!(windows_audio_lifecycle_event_relevant(
        &event(1, LifecycleCause::DeviceRemoved, Some("capture-selected")),
        Some("saved-selection"),
        true,
        Some(&route),
        &[],
        false,
    ));
    assert!(!windows_audio_lifecycle_event_relevant(
        &event(2, LifecycleCause::PropertyChanged, Some("render-speakers")),
        Some("saved-selection"),
        true,
        Some(&route),
        &[],
        false,
    ));
    assert!(!windows_audio_lifecycle_event_relevant(
        &event(3, LifecycleCause::DefaultChanged, Some("capture-other")),
        Some("saved-selection"),
        true,
        Some(&route),
        &[],
        false,
    ));
}

#[test]
fn diagnostics_payload_never_carries_endpoint_id_or_device_name() {
    let route = CaptureRouteSignature {
        device_id: 0,
        platform_id: Some("usb-a".into()),
        uses_system_default: true,
        name: "Definitely A Real Microphone Name".into(),
        native_rate: 48_000,
        channels: 1,
        sample_format: "F32".into(),
        transport: CaptureTransport::Usb,
        form_factor: rightkit_audio::capture::CaptureFormFactor::Microphone,
    };
    let payload = windows_capture_lifecycle_payload(
        1,
        "default_changed",
        "system_default",
        Some(&route),
        false,
        0,
        Some(42),
        None,
        "prepared",
    );
    let serialized = payload.to_string();
    assert!(!serialized.contains("Definitely A Real Microphone Name"));
    assert!(!serialized.contains("usb-a"));
    assert_eq!(payload["event"], "windows_capture_lifecycle");
    assert_eq!(payload["requested_backend"], "cpal_wasapi");
    assert_eq!(payload["actual_backend"], "cpal_wasapi");
    assert_eq!(payload["first_buffer_ms"], 42);
}
