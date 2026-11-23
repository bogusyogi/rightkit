//! User-facing labels for dictation failures.

/// Pill label for a capture open that Windows refused because microphone
/// access is off for the app. Contains no "permission"/"denied".
pub const MICROPHONE_ACCESS_OFF_LABEL: &str =
    "Microphone access is off for this app in Windows Settings";

pub fn label_for_error(message: &str) -> &'static str {
    if is_microphone_access_denied_error(message) {
        crate::host::microphone_off_label().unwrap_or(MICROPHONE_ACCESS_OFF_LABEL)
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

/// A stable, content-free code for an error string that may have been produced by another
/// component (capture, engine, delivery). Logged instead of the string, which can carry free text
/// (a device name, a file name, text from the user's content). Built from the same recognizers as
/// [`label_for_error`]; anything else is `unclassified`.
pub fn error_log_code(message: &str) -> &'static str {
    if is_microphone_access_denied_error(message) {
        "microphone_access_denied"
    } else if is_explicit_microphone_mute_error(message) {
        "microphone_muted"
    } else if is_no_microphone_audio_error(message) {
        "no_microphone_audio"
    } else if is_microphone_device_error(message) {
        "microphone_unavailable"
    } else if is_no_clear_speech_error(message) {
        "no_clear_speech"
    } else {
        "unclassified"
    }
}

/// A stable, content-free code for a history store error. The store's messages begin with a label
/// the app sets and then append the underlying library's text (a database or parser message, which
/// can quote stored content or a path); only the label is recognized here, never the tail.
pub fn history_error_log_code(message: &str) -> &'static str {
    const LABELS: &[(&str, &str)] = &[
        ("create data dir failed", "history_create_dir"),
        ("read history failed", "history_read"),
        ("parse history failed", "history_parse"),
        ("serialize history failed", "history_serialize"),
        ("write history failed", "history_write"),
        ("credential store", "history_credential_store"),
        ("rekey", "history_rekey"),
        ("key rotation", "history_key_rotation"),
        ("stage new key", "history_key_rotation"),
        ("promote recovered key", "history_key_recovery"),
        ("quarantine", "history_recovery"),
        ("recovery marker", "history_recovery"),
        ("commit recovery marker", "history_recovery"),
        ("create recovery marker dir", "history_recovery"),
        ("serialize recovery marker", "history_recovery"),
        ("sync dir", "history_recovery"),
        ("stat", "history_recovery"),
        ("open", "history_recovery"),
    ];
    LABELS
        .iter()
        .find(|(label, _)| message.starts_with(label))
        .map_or("history_other", |(_, code)| code)
}
