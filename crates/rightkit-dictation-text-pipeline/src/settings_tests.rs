use super::{
    asr_decoder_compute, asr_encoder_compute, onboarding_calibration_active,
    persist_recognition_route, valid_persisted_recognition_route, PersistedRecognitionRoute,
};

#[test]
fn accepts_canonical_dml_route() {
    assert!(valid_persisted_recognition_route(
        &PersistedRecognitionRoute {
            id: "dml-0".into(),
            encoder_compute: "dml".into(),
            decoder_compute: "dml".into(),
            provider: "dml".into(),
            adapter_index: Some(0),
            adapter_name: Some("GPU".into())
        }
    ));
}

#[test]
fn rejects_synthetic_cpu_route() {
    assert!(!valid_persisted_recognition_route(
        &PersistedRecognitionRoute {
            id: "two".into(),
            encoder_compute: "cpu_only".into(),
            decoder_compute: "cpu_only".into(),
            provider: "cpu".into(),
            adapter_index: None,
            adapter_name: None
        }
    ));
}

// RIGHTKIT_APP_DATA_DIR is also set by the l3_cleanup tests: both use the ONE crate-wide env
// lock (crate::test_support), since two locks let them race on the same variable.

/// The calibration candidate is process-local state, not environment: a resident dictation
/// process (no candidate applied) resolves compute from its persisted route, and only an
/// applied candidate overrides it. Regression for the 2026-08-09 incident, where an inherited
/// RIGHTKIT_CALIBRATION_*_COMPUTE value re-routed resident dictation to CPU.
#[test]
fn calibration_candidate_overrides_only_where_applied() {
    let _guard = crate::test_support::test_env_guard();
    let dir = std::env::temp_dir().join(format!(
        "rightkit-calibration-route-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    ));
    std::fs::create_dir_all(&dir).expect("create temp app data dir");
    let prev_app_data_dir = std::env::var_os("RIGHTKIT_APP_DATA_DIR");
    std::env::set_var("RIGHTKIT_APP_DATA_DIR", &dir);
    super::clear_calibration_route();

    let persisted = PersistedRecognitionRoute {
        id: "dml-0".into(),
        encoder_compute: "dml".into(),
        decoder_compute: "dml".into(),
        provider: "dml".into(),
        adapter_index: Some(0),
        adapter_name: Some("NVIDIA RTX".into()),
    };
    persist_recognition_route(&persisted).expect("persist dml route");
    let resident = (
        onboarding_calibration_active(),
        asr_encoder_compute(),
        asr_decoder_compute(),
    );

    let candidate = PersistedRecognitionRoute {
        encoder_compute: "cpu_only".into(),
        decoder_compute: "cpu_only".into(),
        ..persisted
    };
    super::apply_calibration_route_environment(&candidate);
    let calibrating = (
        onboarding_calibration_active(),
        asr_encoder_compute(),
        asr_decoder_compute(),
    );
    super::clear_calibration_route();

    match prev_app_data_dir {
        Some(value) => std::env::set_var("RIGHTKIT_APP_DATA_DIR", value),
        None => std::env::remove_var("RIGHTKIT_APP_DATA_DIR"),
    }
    let _ = std::fs::remove_dir_all(&dir);

    // A persisted dml route resolves to the "inherit" fallback (compute_profile only knows
    // neural_engine/cpu_gpu/cpu_only/inherit; the DirectML adapter is chosen elsewhere).
    assert_eq!(
        resident,
        (false, "inherit".to_string(), "inherit".to_string())
    );
    assert_eq!(
        calibrating,
        (true, "cpu_only".to_string(), "cpu_only".to_string())
    );
}
