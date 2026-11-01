use super::{backend_supports_scheduled_static15, BackgroundDecodeKind};

#[test]
fn parakeet_scheduled_only_on_tdt_alias() {
    assert!(backend_supports_scheduled_static15(
        BackgroundDecodeKind::Parakeet,
        "parakeet-tdt"
    ));
    assert!(!backend_supports_scheduled_static15(
        BackgroundDecodeKind::Parakeet,
        "parakeet-unified"
    ));
}

#[test]
fn coreml_parakeet_scheduled_only_on_tdt_alias() {
    assert!(backend_supports_scheduled_static15(
        BackgroundDecodeKind::CoreMlParakeet,
        "parakeet-tdt"
    ));
    assert!(!backend_supports_scheduled_static15(
        BackgroundDecodeKind::CoreMlParakeet,
        "rnnt"
    ));
}

#[test]
fn whisper_coreml_scheduled_unconditionally() {
    assert!(backend_supports_scheduled_static15(
        BackgroundDecodeKind::WhisperCoreMl,
        "whisper"
    ));
    // Backend id doesn't matter for Whisper Core ML — the runtime variant
    // itself only loads when Whisper was actually selected.
    assert!(backend_supports_scheduled_static15(
        BackgroundDecodeKind::WhisperCoreMl,
        "parakeet-tdt"
    ));
}

#[test]
fn whisper_win_scheduled_unconditionally() {
    // Measured on Windows: checkpoint-15 vs deferred30 is -3.32 WER
    // points and -3070ms median stop latency. Same unconditional
    // treatment as Whisper Core ML — no TDT-only constraint applies.
    assert!(backend_supports_scheduled_static15(
        BackgroundDecodeKind::WhisperWin,
        "whisper"
    ));
    assert!(backend_supports_scheduled_static15(
        BackgroundDecodeKind::WhisperWin,
        "parakeet-tdt"
    ));
}

#[test]
fn other_kinds_never_scheduled() {
    // Covers retired Unified/RNNT/KWS-probe runtimes.
    assert!(!backend_supports_scheduled_static15(
        BackgroundDecodeKind::Other,
        "whisper"
    ));
    assert!(!backend_supports_scheduled_static15(
        BackgroundDecodeKind::Other,
        "parakeet-tdt"
    ));
}
