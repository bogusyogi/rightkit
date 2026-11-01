// rightkit-dictation-core consumed by the exact registry pin in Cargo.toml, outside the workspace.
// Public API only; in-memory values only (no audio device, no engine process).
use rightkit_dictation_core::audio_conditioning::rms;
use rightkit_dictation_core::engine::{
    validate_frame, EngineFrame, EnginePayload, EngineSchemaName,
};
use rightkit_dictation_core::mic_selection::{resolve_mic_selection, MicDeviceSnapshot};

fn check(ok: bool, what: &str) -> bool {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
    }
    ok
}

fn main() {
    let mut all = true;
    let request = EngineFrame::base(
        EngineSchemaName::FileTranscriptionRequest,
        "r1",
        None,
        "t1",
        Some(EnginePayload::FileTranscriptionRequest {
            path: "sample.wav".into(),
        }),
        None,
    );
    all &= check(
        validate_frame(&request).is_ok(),
        "file transcription request validates",
    );
    let mismatched = EngineFrame::base(
        EngineSchemaName::FileTranscriptionRequest,
        "r2",
        None,
        "t2",
        None,
        None,
    );
    all &= check(
        validate_frame(&mismatched).is_err(),
        "request without its payload is refused",
    );
    let devices = [MicDeviceSnapshot {
        id: 0,
        name: "Built-in".into(),
        is_default: true,
        platform_id: None,
    }];
    let selection = resolve_mic_selection(Some("Missing USB mic"), &devices);
    all &= check(
        selection.fallback,
        "missing saved mic falls back to the default",
    );
    all &= check(rms(&[0.0; 160]) == 0.0, "silent buffer has zero RMS");
    if !all {
        std::process::exit(1);
    }
}
