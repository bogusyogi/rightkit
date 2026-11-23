// rightkit-dictation-core consumed by the exact registry pin in Cargo.toml, outside the workspace.
// Public API only; in-memory values only (no audio device, no engine process).
use rightkit_dictation_core::audio_conditioning::rms;
use rightkit_dictation_core::engine::{
    validate_frame, DiagnosticsPayload, EngineFrame, EnginePayload, EngineSchemaName, HostCommand,
    LastPasteCase,
};
use rightkit_dictation_core::mic_selection::{resolve_mic_selection, MicDeviceSnapshot};
use rightkit_dictation_core::pill::PillEvent;

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
    let status = PillEvent::Status {
        label: "Saved".to_string(),
    };
    all &= check(
        serde_json::to_string(&status)
            .is_ok_and(|json| json == r#"{"kind":"status","label":"Saved"}"#),
        "status event serializes to source-derived wire shape",
    );
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
    let command = HostCommand::TransformLastPaste {
        delivery_id: "delivery-1".into(),
        transform: LastPasteCase::Upper,
    };
    let wire = command
        .clone()
        .into_reset_diagnostics("last insertion unavailable");
    all &= check(
        wire == serde_json::json!({
            "reset_to_armed": true,
            "command_failed": "last insertion unavailable",
            "host_command": {"kind": "transform_last_paste", "delivery_id": "delivery-1", "transform": "upper"}
        }) && HostCommand::from_reset_diagnostics(&wire) == Some(command),
        "last-paste command round-trips through reset diagnostics",
    );
    let legacy: DiagnosticsPayload =
        serde_json::from_value(wire.clone()).expect("legacy diagnostics remain readable");
    all &= check(
        legacy.reset_to_armed == Some(true)
            && legacy.command_failed.as_deref() == Some("last insertion unavailable"),
        "legacy parser retains the reset and visible failure",
    );
    let mut non_reset = wire.clone();
    non_reset["reset_to_armed"] = false.into();
    let mut malformed = wire;
    malformed["host_command"]["transform"] = "unknown".into();
    all &= check(
        HostCommand::from_reset_diagnostics(&non_reset).is_none()
            && HostCommand::from_reset_diagnostics(&malformed).is_none()
            && HostCommand::from_reset_diagnostics(&serde_json::json!({"reset_to_armed": true}))
                .is_none(),
        "non-reset, malformed and legacy frames request no host command",
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
