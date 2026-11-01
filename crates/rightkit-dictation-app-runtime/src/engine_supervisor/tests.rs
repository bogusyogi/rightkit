use super::protocol::frame_to_engine_event;
use crate::delivery::{DeliveryOutcome, DeliveryRecord, TargetSnapshot};
use crate::engine_process::EngineEvent;
use rightkit_dictation_core::engine::{
    DiagnosticsPayload, EngineFrame, EnginePayload, EngineSchemaName,
};

#[test]
fn hands_free_stop_destination_survives_wire_and_shell_event_conversion() {
    let mut target = TargetSnapshot::test_target();
    target.stop_continuity = Some(rightkit_transcript_library::delivery::StopContinuity {
        captured_at: 9_007_199_254_740_993,
        process_instance: 1_789_342_496_123_456_789,
        native_control: Some(321),
    });
    let frame = sidecar_frame(
        EngineSchemaName::TranscribingStarted,
        "auto-stop-1",
        Some("session-1".into()),
        Some(EnginePayload::TranscribingStarted {
            stop_kind: None,
            stop_target: Some(target.clone()),
        }),
    );
    let wire = serde_json::to_vec(&frame).unwrap();
    let received: EngineFrame = serde_json::from_slice(&wire).unwrap();
    match frame_to_engine_event(&received) {
        Some(EngineEvent::TranscribingStarted {
            session_id,
            stop_target,
        }) => {
            assert_eq!(session_id, "session-1");
            assert_eq!(stop_target, Some(target));
        }
        other => panic!("expected stop destination in shell event, got {other:?}"),
    }
}

#[test]
fn capture_warning_travels_with_history_transcript() {
    let frame = sidecar_frame(
        EngineSchemaName::TranscriptFinal,
        "capture-recovery",
        Some("session-1".into()),
        Some(EnginePayload::TranscriptFinal {
            text: "Keep captured speech".into(),
            confidence: None,
            diagnostics: Some(
                serde_json::to_value(DiagnosticsPayload {
                    cancelled: Some(true),
                    capture_warning: Some("No microphone audio detected".into()),
                    ..Default::default()
                })
                .unwrap(),
            ),
        }),
    );
    assert!(matches!(frame_to_engine_event(&frame),
        Some(EngineEvent::TranscriptFinal { cancelled: true, capture_warning: Some(warning), text, .. })
        if warning == "No microphone audio detected" && text == "Keep captured speech"));
}

fn sidecar_frame(
    schema_name: EngineSchemaName,
    request_id: &str,
    session_id: Option<String>,
    payload: Option<EnginePayload>,
) -> EngineFrame {
    EngineFrame::base(
        schema_name,
        request_id,
        session_id,
        "sidecar-1",
        payload,
        None,
    )
}

// Model-subdir routing now lives in `crate::model_paths::subdir_for_backend`
// (dispatch #17 dedup). Assert the Windows layout through that source of truth.
// The TDT answer depends on what is on disk, so it is asserted below against isolated state.
#[cfg(not(target_os = "macos"))]
#[test]
fn windows_model_subdir_routes_unified_default() {
    use crate::model_paths::subdir_for_backend;
    const UNIFIED: &str = "unified_static15_b128_sym_bits4_timestamp_hybrid";
    assert_eq!(subdir_for_backend("parakeet-unified"), UNIFIED);
    assert_eq!(subdir_for_backend("parakeet"), UNIFIED);
    assert_eq!(subdir_for_backend("rnnt"), UNIFIED);
    assert_eq!(subdir_for_backend("bogus"), UNIFIED);
}

/// The Parakeet launch-frame model for `parakeet-tdt`, as the resident engine receives it.
#[cfg(feature = "test-mode")]
fn tdt_launch_dir() -> std::path::PathBuf {
    let launch = super::supervisor::engine_launch_for(&crate::SupervisorConfig {
        asr_backend: "parakeet-tdt".into(),
        ..Default::default()
    });
    assert_eq!(launch.backend, "parakeet-tdt");
    assert!(!launch.load_check);
    if cfg!(target_os = "macos") {
        assert_eq!(launch.asr_model_subdir, None);
        launch
            .coreml_model_dir
            .expect("macOS sends coreml_model_dir")
    } else {
        assert_eq!(launch.coreml_model_dir, None);
        std::path::PathBuf::from(
            launch
                .asr_model_subdir
                .expect("Windows sends asr_model_subdir"),
        )
    }
}

/// #331 follow-up. A relaunched shell has prepared nothing: the launch frame must still
/// carry the Ready shared Ultra copy when the activation record names it, and the nonexistent
/// app-data path (which the engine refuses loudly) otherwise.
#[cfg(feature = "test-mode")]
#[test]
fn launch_frame_carries_the_recorded_store_copy_in_a_process_that_prepared_nothing() {
    use rightkit_dictation_models::shared;
    use rightkit_dictation_models::test_support::{
        isolate_env, scratch_dir, write_activation_record, write_ready_ultra, write_record_text,
        write_retired_fileset,
    };
    let tmp = scratch_dir("launch-frame");
    let _env = isolate_env(tmp.path());
    let ultra_id = shared::legacy_subdir();
    let missing_app_data_dir = crate::model_paths::install_base().join(ultra_id);

    // Nothing anywhere: the download target, which does not exist.
    assert_eq!(tdt_launch_dir(), missing_app_data_dir);

    // Ready store copy, no record: this process never verified it, so it is not selected.
    let dir = write_ready_ultra();
    assert_eq!(tdt_launch_dir(), missing_app_data_dir);

    // Ready store copy and its record: selected although nothing was prepared here.
    write_activation_record(&dir);
    assert!(!shared::is_verified_active());
    assert_eq!(tdt_launch_dir(), dir);

    // The record beats a retired copy that is still on disk.
    let (retired_subdir, _) = crate::model_paths::RETIRED_TDT_MODELS[0];
    let retired_dir = crate::model_paths::install_base().join(retired_subdir);
    write_retired_fileset(&retired_dir);
    assert_eq!(
        crate::model_paths::subdir_for_backend("parakeet-tdt"),
        ultra_id
    );
    assert_eq!(tdt_launch_dir(), dir);

    // A record that does not match, or a denied copy, falls back to the retired copy.
    write_record_text("{torn");
    assert_eq!(
        crate::model_paths::subdir_for_backend("parakeet-tdt"),
        retired_subdir
    );
    assert_eq!(tdt_launch_dir(), retired_dir);
    write_activation_record(&dir);
    assert_eq!(tdt_launch_dir(), dir);
    shared::deactivate();
    assert_eq!(
        crate::model_paths::subdir_for_backend("parakeet-tdt"),
        retired_subdir
    );
    assert_eq!(tdt_launch_dir(), retired_dir);

    // A load check still pins its own candidate.
    shared::reset_activation_for_tests();
    std::fs::remove_dir_all(&retired_dir).unwrap();
    let candidate = tmp.path().join("candidate");
    let launch = shared::with_load_check(&candidate, || {
        super::supervisor::engine_launch_for(&crate::SupervisorConfig {
            asr_backend: "whisper".into(),
            ..Default::default()
        })
    });
    assert!(launch.load_check);
    assert_eq!(launch.backend, "parakeet-tdt");
}

/// What `subdir_for_backend("parakeet-tdt")` answers for every state of the disk, exactly.
#[cfg(feature = "test-mode")]
#[test]
fn tdt_subdir_is_exactly_ultra_or_the_newest_retired_copy() {
    use rightkit_dictation_models::test_support::{
        isolate_env, scratch_dir, write_retired_fileset,
    };
    let tmp = scratch_dir("tdt-subdir");
    let _env = isolate_env(tmp.path());
    let ultra = rightkit_dictation_models::shared::legacy_subdir();
    // No model anywhere: Ultra is the download target.
    assert_eq!(
        crate::model_paths::subdir_for_backend("parakeet-tdt"),
        ultra
    );
    assert_eq!(crate::model_paths::resolve_model_dir(ultra), None);
    // A machine that holds only a retired TDT model keeps serving it until Ultra is active.
    let (retired, _) = crate::model_paths::RETIRED_TDT_MODELS[0];
    write_retired_fileset(&crate::model_paths::install_base().join(retired));
    assert_eq!(
        crate::model_paths::subdir_for_backend("parakeet-tdt"),
        retired
    );
}

#[test]
fn delivery_completed_preserves_send_enter_from_diagnostics() {
    let record = DeliveryRecord::new(
        "delivery-1",
        "Ship it.",
        DeliveryOutcome::Pasted,
        TargetSnapshot::test_target(),
    );
    let diagnostics = serde_json::to_value(DiagnosticsPayload {
        delivery_record: Some(serde_json::to_value(&record).unwrap()),
        send_enter: Some(true),
        ..Default::default()
    })
    .unwrap();
    let frame = sidecar_frame(
        EngineSchemaName::TranscriptFinal,
        "request-1",
        Some("session-1".to_string()),
        Some(EnginePayload::TranscriptFinal {
            text: record.transcript.clone(),
            confidence: Some(1.0),
            diagnostics: Some(diagnostics),
        }),
    );

    let event = frame_to_engine_event(&frame).unwrap();

    assert!(matches!(
        event,
        EngineEvent::DeliveryCompleted {
            session_id,
            send_enter: true,
            ..
        } if session_id == "session-1"
    ));
}

#[test]
fn delivery_completed_legacy_diagnostics_default_send_enter_false() {
    let record = DeliveryRecord::new(
        "delivery-1",
        "Ship it.",
        DeliveryOutcome::Pasted,
        TargetSnapshot::test_target(),
    );
    let diagnostics = serde_json::json!({
        "delivery_record": record,
    });
    let frame = sidecar_frame(
        EngineSchemaName::TranscriptFinal,
        "request-1",
        Some("session-1".to_string()),
        Some(EnginePayload::TranscriptFinal {
            text: "Ship it.".to_string(),
            confidence: Some(1.0),
            diagnostics: Some(diagnostics),
        }),
    );

    let event = frame_to_engine_event(&frame).unwrap();

    assert!(matches!(
        event,
        EngineEvent::DeliveryCompleted {
            send_enter: false,
            ..
        }
    ));
}

#[test]
fn revocation_ack_requires_cancel_reply_without_error() {
    use super::revocation::{classify_revocation_reply, RevocationOutcome};
    use rightkit_dictation_core::engine::{EngineErrorPayload, StopKind};
    let ack = sidecar_frame(
        EngineSchemaName::TranscribingStarted,
        "cancel:1:cancel:watchdog",
        Some("s".into()),
        Some(EnginePayload::TranscribingStarted {
            stop_kind: Some(StopKind::Cancel),
            stop_target: None,
        }),
    );
    assert_eq!(classify_revocation_reply(Ok(ack)), RevocationOutcome::Acked);

    let wrong_kind = sidecar_frame(
        EngineSchemaName::TranscribingStarted,
        "cancel:2",
        Some("s".into()),
        Some(EnginePayload::TranscribingStarted {
            stop_kind: Some(StopKind::Stop),
            stop_target: None,
        }),
    );
    assert!(matches!(
        classify_revocation_reply(Ok(wrong_kind)),
        RevocationOutcome::Failed { .. }
    ));

    let mut errored = sidecar_frame(
        EngineSchemaName::TranscribingStarted,
        "cancel:3",
        Some("s".into()),
        None,
    );
    errored.error = Some(EngineErrorPayload {
        code: "x".into(),
        message: "boom".into(),
        retryable: false,
        diagnostics: None,
    });
    assert!(matches!(
        classify_revocation_reply(Ok(errored)),
        RevocationOutcome::Failed { .. }
    ));

    assert!(matches!(
        classify_revocation_reply(Err("sidecar call timeout/request failure: timed out".into())),
        RevocationOutcome::Failed { .. }
    ));
}

#[test]
fn revoke_with_ack_does_not_spawn_a_sidecar_when_none_runs() {
    use super::revocation::RevocationOutcome;
    use rightkit_dictation_core::engine::StopKind;
    let (supervisor, _rx) = super::EngineSupervisor::new(false);
    let frame = sidecar_frame(
        EngineSchemaName::TranscribingStarted,
        "cancel:9",
        Some("s".into()),
        Some(EnginePayload::TranscribingStarted {
            stop_kind: Some(StopKind::Cancel),
            stop_target: None,
        }),
    );
    assert_eq!(
        supervisor.revoke_with_ack(&frame, std::time::Duration::from_millis(50)),
        RevocationOutcome::NotRunning
    );
    assert_eq!(supervisor.generation(), 0);
}

#[test]
fn supervisor_final_wait_tracks_per_session() {
    let (supervisor, _rx) = super::EngineSupervisor::new(false);
    assert!(!supervisor.final_wait_expired("s"));
    supervisor.note_recording_started("s");
    supervisor.arm_final_wait("s");
    assert!(!supervisor.final_wait_expired("s"));
    supervisor.clear_final_wait("s");
    assert!(!supervisor.final_wait_expired("s"));
}

/// A crash is recovered by demanding RightKit's on-demand restart, never by calling `rk.start()`
/// again (which would reset the rolling budget). Every demand below runs against a stub engine that
/// says hello and then dies; with a budget of 3 restarts the stub is spawned exactly 4 times
/// (the run plus 3 restarts) however many `start()` demands follow, and the exhausted budget is
/// reported, not reset.
#[cfg(unix)]
#[test]
fn repeated_start_demands_cannot_reset_the_restart_breaker() {
    use rightkit_job_queue::{OwnedCommand, RestartMode, RestartPolicy};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    let spawns = Arc::new(AtomicUsize::new(0));
    let counter = spawns.clone();
    let policy = RestartPolicy::new(
        RestartMode::OnDemand,
        3,
        Duration::from_secs(60),
        Duration::from_millis(1),
        Duration::from_millis(5),
    )
    .unwrap();
    let (supervisor, _events) = super::EngineSupervisor::with_parts(
        policy,
        Arc::new(Mutex::new(None)),
        false,
        move || {
            counter.fetch_add(1, Ordering::SeqCst);
            let mut command = OwnedCommand::new("/bin/sh");
            command.env_allowlist(["PATH"]);
            command.command_mut().arg("-c").arg(
                r#"printf '%s\n' '{"type":"hello","engine":"t","version":"t/1","contract_version":1,"instance":"i"}'; sleep 0.3; exit 1"#,
            );
            Ok(command)
        },
    );
    assert!(
        supervisor.start().is_ok(),
        "the first start is the explicit new run"
    );
    let mut last = Ok(String::new());
    for _ in 0..8 {
        let deadline = Instant::now() + Duration::from_secs(5);
        while supervisor.is_running() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        last = supervisor.start();
    }
    assert_eq!(
        spawns.load(Ordering::SeqCst),
        4,
        "demands must not reset the budget: 1 run + 3 granted restarts"
    );
    assert!(
        last.is_err(),
        "the spent budget is reported to the caller: {last:?}"
    );

    // An intentional shutdown ends the run: the next start is an explicit new run.
    supervisor.shutdown();
    assert!(supervisor.start().is_ok());
    assert_eq!(spawns.load(Ordering::SeqCst), 5);
    supervisor.shutdown();
}

/// A stub engine that says hello, lives 0.3 s and dies, spawned through the production
/// `EngineSupervisor::with_parts`.
#[cfg(unix)]
fn dying_stub(
    spawns: &std::sync::Arc<std::sync::atomic::AtomicUsize>,
) -> impl Fn() -> std::io::Result<rightkit_job_queue::OwnedCommand> + Send + Sync + 'static {
    let counter = spawns.clone();
    move || {
        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut command = rightkit_job_queue::OwnedCommand::new("/bin/sh");
        command.env_allowlist(["PATH"]);
        command.command_mut().arg("-c").arg(
            r#"printf '%s\n' '{"type":"hello","engine":"t","version":"t/1","contract_version":1,"instance":"i"}'; sleep 0.3; exit 1"#,
        );
        Ok(command)
    }
}

/// engine-runtime#m2, engine-runtime#2, XF-07. A spent restart budget used to be permanent: RightKit
/// holds `Terminal::Failed` until `start()` runs again, and only `shutdown()` allowed that, so
/// dictation stayed down until a settings change or a relaunch. Once the cooldown has passed the
/// next `start()` is a new run with a fresh budget.
#[cfg(unix)]
#[test]
fn a_spent_restart_budget_recovers_after_the_cooldown() {
    use rightkit_job_queue::{RestartMode, RestartPolicy};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    let spawns = Arc::new(AtomicUsize::new(0));
    let policy = RestartPolicy::new(
        RestartMode::OnDemand,
        2,
        Duration::from_secs(10),
        Duration::from_millis(1),
        Duration::from_millis(5),
    )
    .unwrap();
    let (supervisor, _events) = super::EngineSupervisor::with_parts(
        policy,
        Arc::new(Mutex::new(None)),
        false,
        dying_stub(&spawns),
    );
    let supervisor = supervisor.with_abandon_cooldown(Duration::from_millis(500));
    assert!(
        supervisor.start().is_ok(),
        "the first start is the explicit new run"
    );
    let mut last = Ok(String::new());
    for _ in 0..8 {
        let deadline = Instant::now() + Duration::from_secs(5);
        while supervisor.is_running() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        last = supervisor.start();
    }
    assert_eq!(
        spawns.load(Ordering::SeqCst),
        3,
        "1 run + 2 granted restarts"
    );
    assert!(last.is_err(), "the spent budget is reported: {last:?}");
    let spent_generation = supervisor.generation();

    // Inside the cooldown the engine stays down and nothing is spawned.
    assert!(supervisor.start().is_err());
    assert_eq!(spawns.load(Ordering::SeqCst), 3);

    // After the cooldown the next demand is a new run.
    std::thread::sleep(Duration::from_millis(650));
    assert!(
        supervisor.start().is_ok(),
        "a demand after the cooldown starts a fresh run"
    );
    assert_eq!(spawns.load(Ordering::SeqCst), 4);
    assert!(supervisor.generation() > spent_generation);
    supervisor.shutdown();
}

#[cfg(unix)]
thread_local! {
    /// What the supervisor config provider below reports on THIS thread: BYOK delivery is eligible
    /// while set. `start()` reads the config on the caller's thread, so only the test that sets it
    /// sees it; the other supervisor tests in this binary run on their own threads and see none.
    static KEYS_ELIGIBLE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// engine-runtime#8, engine-runtime#c1. `KeyGate` keeps a generation's failed delivery for good,
/// so one lost ack left a cloud-polish user unable to start dictation until a settings change or
/// a relaunch. The failed generation is retired: the next `start()` spawns a fresh one that is
/// delivered to afresh.
#[cfg(unix)]
#[test]
fn a_failed_key_delivery_retires_its_generation_and_the_next_start_redelivers() {
    use rightkit_job_queue::{OwnedCommand, RestartMode, RestartPolicy};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    let _ = crate::install_supervisor_config_provider(|| crate::SupervisorConfig {
        ai_polish_enabled: KEYS_ELIGIBLE.get(),
        product_provider_ready: KEYS_ELIGIBLE.get(),
        ..Default::default()
    });
    KEYS_ELIGIBLE.set(true);

    let spawns = Arc::new(AtomicUsize::new(0));
    let counter = spawns.clone();
    let policy = RestartPolicy::new(
        RestartMode::OnDemand,
        3,
        Duration::from_secs(60),
        Duration::from_millis(1),
        Duration::from_millis(5),
    )
    .unwrap();
    // Says hello, then reads stdin forever without ever acknowledging the key hand-off; it leaves
    // when asked to shut down or when stdin closes.
    let (supervisor, _events) = super::EngineSupervisor::with_parts(
        policy,
        Arc::new(Mutex::new(None)),
        false,
        move || {
            counter.fetch_add(1, Ordering::SeqCst);
            let mut command = OwnedCommand::new("/bin/sh");
            command.env_allowlist(["PATH"]);
            command.command_mut().arg("-c").arg(
                r#"printf '%s\n' '{"type":"hello","engine":"t","version":"t/1","contract_version":1,"instance":"i"}'; while IFS= read -r line; do case "$line" in *shutdown*) exit 0;; esac; done"#,
            );
            Ok(command)
        },
    );

    let first = supervisor.start();
    let error = first.expect_err("no ack: the delivery failed");
    assert!(
        error.starts_with("provider key delivery failed:"),
        "typed delivery failure, got {error}"
    );
    let failed_generation = supervisor.generation();
    assert!(
        !supervisor.is_running(),
        "the generation whose delivery failed is retired"
    );

    // The failure was that generation's alone: with delivery eligible again a start succeeds on a
    // new generation instead of returning the old failure forever.
    KEYS_ELIGIBLE.set(false);
    assert!(
        supervisor.start().is_ok(),
        "the next start is a fresh generation"
    );
    assert!(supervisor.generation() > failed_generation);
    assert_eq!(spawns.load(Ordering::SeqCst), 2);
    supervisor.shutdown();
}

#[test]
fn key_delivery_errors_keep_the_typed_code_and_never_carry_key_material() {
    use super::supervisor::delivery_error_for_test;
    for code in [
        "timeout",
        "delivery_unknown",
        "engine_exited",
        "engine_write",
        "encode_failed",
        "ack_invalid",
    ] {
        assert_eq!(
            delivery_error_for_test(code),
            format!("provider key delivery failed: {code}")
        );
    }
    assert_eq!(
        delivery_error_for_test("anything else the engine said"),
        "provider key delivery failed: engine_rejected"
    );
}

#[cfg(feature = "test-mode")]
#[test]
fn lever_env_forwards_only_nonempty_hr_switches_the_supervisor_does_not_own() {
    use std::ffi::OsString;
    let vars = vec![
        (OsString::from("RIGHTKIT_KWS_ONSET"), OsString::from("1")),
        (
            OsString::from("RIGHTKIT_KWS_BACKEND"),
            OsString::from("rust"),
        ),
        (OsString::from("RIGHTKIT_KWS_TTL_FRAMES"), OsString::new()),
        (
            OsString::from("RIGHTKIT_ASR_BACKEND"),
            OsString::from("whisper"),
        ),
        (
            OsString::from("RIGHTKIT_APP_DATA_DIR"),
            OsString::from("/elsewhere"),
        ),
        (
            OsString::from("RIGHTKIT_OWNER_DIAGNOSTICS"),
            OsString::from("1"),
        ),
        (
            OsString::from("RIGHTKIT_OWNER_DIAGNOSTICS_DIR"),
            OsString::from("rel"),
        ),
        (
            OsString::from("RIGHTKIT_CALIBRATION_COLLECT_DIAGNOSTICS"),
            OsString::from("1"),
        ),
        (OsString::from("RIGHTKIT_L3_CLEANUP"), OsString::from("1")),
        (OsString::from("GROQ_API_KEY"), OsString::from("gsk_x")),
        (OsString::from("hr_kws_onset"), OsString::from("1")),
        (OsString::from("PATH"), OsString::from("/bin")),
    ];
    let levers = super::env::lever_env(vars);
    let names: Vec<&str> = levers.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, ["RIGHTKIT_KWS_BACKEND", "RIGHTKIT_KWS_ONSET"]);
    assert_eq!(levers[0].1, OsString::from("rust"));
}

#[cfg(feature = "test-mode")]
#[test]
fn qa_sidecar_command_carries_the_shells_levers() {
    // A name no other test or crate reads, so setting it cannot disturb a parallel test.
    const PROBE: &str = "RIGHTKIT_LEVER_FORWARD_PROBE";
    std::env::set_var(PROBE, "on");
    let mut owned = super::supervisor::build_sidecar_command_for_test(
        std::path::Path::new("rightkit-engine"),
        &crate::SupervisorConfig::default(),
    );
    std::env::remove_var(PROBE);
    let command = owned.command_mut();
    let forwarded = command
        .get_envs()
        .find(|(name, _)| *name == PROBE)
        .and_then(|(_, value)| value.map(|value| value.to_os_string()));
    assert_eq!(forwarded, Some(std::ffi::OsString::from("on")));
}
