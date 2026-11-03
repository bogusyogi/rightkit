//! User-facing labels for dictation failures.

/// Pill label for a capture open that Windows refused because microphone
/// access is off for the app. Contains no "permission"/"denied".
pub const MICROPHONE_ACCESS_OFF_LABEL: &str =
    "Microphone access is off for this app in Windows Settings";

pub fn label_for_error(message: &str) -> &'static str {
    if is_microphone_access_denied_error(message) {
        MICROPHONE_ACCESS_OFF_LABEL
    } else if is_explicit_microphone_mute_error(message) {
        "Microphone muted. Unmute it and try again."
    } else if is_no_microphone_audio_error(message) {
        "No audio detected. Check microphone and mute setting, then try again."
    } else if is_microphone_device_error(message) {
        "Microphone unavailable. Check connection and permissions, then try again."
    } else if is_no_clear_speech_error(message) {
        "No clear speech"
    } else {
        "Error"
    }
}

/// Only call input muted when capture reports an explicit mute state. A
/// zero-only stream can also mean quiet speech, a gated headset, or a dead
/// route, so those cases stay under the less certain no-audio wording.
pub fn is_explicit_microphone_mute_error(message: &str) -> bool {
    let normalized = message.to_ascii_lowercase();
    normalized.contains("microphone muted")
        || normalized.contains("microphone is muted")
        || normalized.contains("input muted")
        || normalized.contains("input is muted")
        || normalized.contains("device muted")
        || normalized.contains("mute_state=on")
}

/// WASAPI reports package/desktop microphone denial as E_ACCESSDENIED
/// (0x80070005, "Access is denied"). Scoped to capture-path errors so an
/// unrelated access-denied (for example an engine spawn failure) keeps its
/// own label.
pub fn is_microphone_access_denied_error(message: &str) -> bool {
    let normalized = message.to_ascii_lowercase();
    normalized.contains("capture")
        && (normalized.contains("access is denied")
            || normalized.contains("0x80070005")
            || normalized.contains("-2147024891")
            || normalized.contains("microphone permission may still be denied"))
}

pub fn is_no_microphone_audio_error(message: &str) -> bool {
    let normalized = message.to_ascii_lowercase();
    normalized.contains("no microphone audio")
        || normalized.contains("no audio detected")
        || normalized.contains("capture_stream_digital_zero")
        || normalized.contains("capture_zero_recovery_failed")
}

pub fn is_microphone_device_error(message: &str) -> bool {
    let normalized = message.to_ascii_lowercase();
    normalized.contains("capture open")
        || normalized.contains("capture resume")
        || normalized.contains("capture read")
        || normalized.contains("capture devices")
        || normalized.contains("no default input device")
        || normalized.contains("saved microphone is unavailable")
        || normalized.contains("microphone permission")
}

pub fn is_no_clear_speech_error(message: &str) -> bool {
    let normalized = message.to_ascii_lowercase();
    normalized.contains("e_quality_gate")
        || normalized.contains("empty_output")
        || normalized.contains("text_from_silent_audio")
        || normalized.contains("repetition_loop")
        || normalized.contains("low_token_diversity")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_access_denied_gets_the_specific_label() {
        let err = "capture open: build_input_stream failed: Access is denied. (0x80070005) (microphone permission may still be denied in Windows Settings)";
        assert_eq!(label_for_error(err), MICROPHONE_ACCESS_OFF_LABEL);
        assert!(!is_microphone_access_denied_error(
            "spawn_failed: could not start engine: Access is denied. (os error 5)"
        ));
    }

    #[test]
    fn capture_health_labels_preserve_evidence() {
        assert_eq!(
            label_for_error("input is muted by system"),
            "Microphone muted. Unmute it and try again."
        );
        assert_eq!(
            label_for_error("No microphone audio detected after reopening. Check your microphone or mute setting, then try again."),
            "No audio detected. Check microphone and mute setting, then try again."
        );
        assert_eq!(
            label_for_error("capture open: no default input device available"),
            "Microphone unavailable. Check connection and permissions, then try again."
        );
        assert!(!is_explicit_microphone_mute_error(
            "No microphone audio detected; check your mute setting"
        ));
    }

    #[test]
    fn quality_errors_keep_no_clear_speech_label() {
        assert_eq!(
            label_for_error("engine E_QUALITY_GATE: repetition_loop"),
            "No clear speech"
        );
        assert_eq!(label_for_error("engine io failed: pipe closed"), "Error");
    }
}
