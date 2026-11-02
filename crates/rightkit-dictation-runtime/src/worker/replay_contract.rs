#[test]
pub(crate) fn wav_source_contains_no_clock_sleep_or_product_decision_logic() {
    let source = include_str!("wav_replay_source.rs");
    for forbidden in [
        "Instant::now",
        "thread::sleep",
        "transcribe_",
        "TAIL_",
        "CMD_",
        "PAUSE_GATE",
        "KWS",
    ] {
        assert!(
            !source.contains(forbidden),
            "WAV replay source duplicated forbidden runtime concern: {forbidden}"
        );
    }
}

#[test]
pub(crate) fn virtual_clock_contains_no_audio_or_control_policy() {
    let source = include_str!("worker_clock.rs");
    for forbidden in ["transcribe_", "TAIL_", "CMD_", "PAUSE_GATE", "KWS"] {
        assert!(
            !source.contains(forbidden),
            "worker clock duplicated forbidden runtime concern: {forbidden}"
        );
    }
}

#[test]
pub(crate) fn replay_driver_contains_no_wall_clock_pacing_or_recognition_policy() {
    let source = include_str!("replay_driver.rs");
    for forbidden in [
        "Instant::now",
        "thread::sleep",
        "transcribe_",
        "TAIL_",
        "CMD_",
        "PAUSE_GATE",
        "KWS",
    ] {
        assert!(
            !source.contains(forbidden),
            "replay driver duplicated forbidden runtime concern: {forbidden}"
        );
    }
}
