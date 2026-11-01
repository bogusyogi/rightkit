#[test]
fn diagnostic_success_requests_vad_even_without_failure_upload() {
    assert!(super::should_measure_vad(true, false));
    assert!(super::should_measure_vad(false, true));
    assert!(!super::should_measure_vad(false, false));
}
use super::*;

#[test]
fn normalization_folds_case_punctuation_and_marks() {
    assert_eq!(normalize_transcript("Café—TEST,  now!"), "cafe test now");
}

#[test]
fn wer_is_zero_for_normalized_reference() {
    let gate = quality_gate(REFERENCE_TEXT, &normalize_transcript(REFERENCE_TEXT));
    assert_eq!(gate.wer, 0.0);
    assert!(!gate.premature_ending);
}

#[test]
fn deletion_guard_rejects_a_missing_tail() {
    let reference = "one two three four five six seven eight nine ten";
    let gate = quality_gate(reference, "one two three four five six");
    assert!(gate.premature_ending);
    assert!(gate.concentrated_deletion);
}

#[test]
fn fixed_script_wer_boundary_is_seven_percent() {
    let words = normalize_transcript(REFERENCE_TEXT)
        .split_whitespace()
        .map(str::to_string)
        .collect::<Vec<_>>();
    assert_eq!(words.len(), 79);
    let with_errors = |count: usize| {
        let mut actual = words.clone();
        for word in actual.iter_mut().take(count) {
            *word = "wrong".to_string();
        }
        actual.join(" ")
    };
    assert!(quality_gate(REFERENCE_TEXT, &with_errors(5)).wer <= MAX_WER);
    assert!(quality_gate(REFERENCE_TEXT, &with_errors(6)).wer > MAX_WER);
}

#[test]
fn windows_prefers_nvidia_then_other_dml_then_cpu() {
    let routes = candidate_routes(
        CalibrationPlatform::Windows,
        &[
            DmlAdapter {
                index: 4,
                name: "Intel Arc".into(),
            },
            DmlAdapter {
                index: 2,
                name: "NVIDIA RTX".into(),
            },
        ],
    );
    assert_eq!(
        routes
            .iter()
            .map(|route| route.id.as_str())
            .collect::<Vec<_>>(),
        vec!["dml-2", "dml-4", "cpu-only"]
    );
}

fn persisted_dml_route(
    adapter_index: i32,
    adapter_name: Option<&str>,
) -> PersistedRecognitionRoute {
    PersistedRecognitionRoute {
        id: format!("dml-{adapter_index}"),
        encoder_compute: "dml".into(),
        decoder_compute: "dml".into(),
        provider: "dml".into(),
        adapter_index: Some(adapter_index),
        adapter_name: adapter_name.map(str::to_string),
    }
}

#[test]
fn calibrated_dml_adapter_resolves_reordered_gpu_by_exact_name() {
    let route = persisted_dml_route(0, Some("RTX 4070"));
    let adapters = vec![
        DmlAdapter {
            index: 0,
            name: "Iris Xe".into(),
        },
        DmlAdapter {
            index: 1,
            name: "RTX 4070".into(),
        },
    ];
    assert_eq!(resolve_calibrated_dml_adapter(&route, &adapters), Ok(1));
}

#[test]
fn calibrated_dml_adapter_keeps_unchanged_index() {
    let route = persisted_dml_route(1, Some("RTX 4070"));
    let adapters = vec![
        DmlAdapter {
            index: 0,
            name: "Iris Xe".into(),
        },
        DmlAdapter {
            index: 1,
            name: "RTX 4070".into(),
        },
    ];
    assert_eq!(resolve_calibrated_dml_adapter(&route, &adapters), Ok(1));
}

#[test]
fn calibrated_dml_adapter_rejects_missing_or_ambiguous_name() {
    let missing = persisted_dml_route(0, Some("RTX 4070"));
    let unavailable = vec![DmlAdapter {
        index: 0,
        name: "Iris Xe".into(),
    }];
    assert!(resolve_calibrated_dml_adapter(&missing, &unavailable).is_err());

    let duplicate = vec![
        DmlAdapter {
            index: 0,
            name: "RTX 4070".into(),
        },
        DmlAdapter {
            index: 1,
            name: "RTX 4070".into(),
        },
    ];
    assert!(resolve_calibrated_dml_adapter(&missing, &duplicate).is_err());
}

#[test]
fn calibrated_dml_adapter_rejects_non_dml_or_invalid_name() {
    let mut route = persisted_dml_route(0, Some("RTX 4070"));
    route.provider = "cpu".into();
    let adapters = vec![DmlAdapter {
        index: 0,
        name: "RTX 4070".into(),
    }];
    assert!(resolve_calibrated_dml_adapter(&route, &adapters).is_err());

    let route = persisted_dml_route(0, Some("  "));
    assert!(resolve_calibrated_dml_adapter(&route, &adapters).is_err());
    let route = persisted_dml_route(0, None);
    assert!(resolve_calibrated_dml_adapter(&route, &adapters).is_err());
}

#[test]
fn apple_routes_keep_ane_first_and_cpu_last() {
    let routes = candidate_routes(CalibrationPlatform::AppleSilicon, &[]);
    assert_eq!(routes.first().unwrap().id, "ane-all");
    assert_eq!(routes.last().unwrap().id, "cpu-only");
    assert_eq!(routes.len(), 5);
    assert_eq!(
        routes
            .iter()
            .map(|route| (
                route.encoder_compute.as_str(),
                route.decoder_compute.as_str()
            ))
            .collect::<Vec<_>>(),
        vec![
            ("neural_engine", "neural_engine"),
            ("neural_engine", "cpu_only"),
            ("cpu_gpu", "neural_engine"),
            ("cpu_gpu", "cpu_only"),
            ("cpu_only", "cpu_only"),
        ]
    );
}

#[test]
fn preferred_failure_runs_complete_comparison_matrix() {
    let routes = ["one", "two", "three"]
        .into_iter()
        .map(|id| CalibrationRoute {
            id: id.into(),
            encoder_compute: "cpu_only".into(),
            decoder_compute: "cpu_only".into(),
            provider: "cpu".into(),
            adapter_index: None,
            adapter_name: None,
        })
        .collect();
    let (mut calls, mut persisted) = (Vec::new(), Vec::new());
    let result = run_routes(
        &vec![0.0; 480_000],
        true,
        routes,
        |route| {
            calls.push(route.id.clone());
            CalibrationAttempt {
                route: route.clone(),
                transcript: String::new(),
                wer: None,
                latency_ms: 0,
                warm_latency_ms: None,
                deletion_run_max: 0,
                premature_ending: false,
                concentrated_deletion: false,
                status: if route.id == "two" {
                    "passed"
                } else {
                    "route_process_exit_1"
                }
                .into(),
                error: None,
                model_fingerprint: None,
                model_dir_sha256: None,
                compile_cache: None,
                diagnostics: None,
                words_per_second: None,
                max_silent_emission_gap_seconds: None,
            }
        },
        |route| {
            persisted.push(route.id.clone());
            Ok(())
        },
    );
    assert_eq!(calls, ["one", "two", "three"]);
    assert_eq!(persisted, ["two"]);
    assert_eq!(result.attempts.len(), 3);
    assert_eq!(result.selected_route.unwrap().id, "two");
    assert!(result.upload_eligible);
}

#[test]
fn normal_onboarding_stops_at_first_working_fallback() {
    let routes = ["one", "two", "three"]
        .into_iter()
        .map(|id| CalibrationRoute {
            id: id.into(),
            encoder_compute: "cpu_only".into(),
            decoder_compute: "cpu_only".into(),
            provider: "cpu".into(),
            adapter_index: None,
            adapter_name: None,
        })
        .collect();
    let mut calls = Vec::new();
    let result = run_routes(
        &vec![0.0; 480_000],
        false,
        routes,
        |route| {
            calls.push(route.id.clone());
            CalibrationAttempt {
                route: route.clone(),
                transcript: String::new(),
                wer: None,
                latency_ms: 1,
                warm_latency_ms: None,
                deletion_run_max: 0,
                premature_ending: false,
                concentrated_deletion: false,
                status: if route.id == "two" {
                    "passed"
                } else {
                    "quality_failed"
                }
                .into(),
                error: None,
                model_fingerprint: None,
                model_dir_sha256: None,
                compile_cache: None,
                diagnostics: None,
                words_per_second: None,
                max_silent_emission_gap_seconds: None,
            }
        },
        |_| Ok(()),
    );
    assert_eq!(calls, ["one", "two"]);
    assert_eq!(result.selected_route.unwrap().id, "two");
    assert!(result.upload_eligible);
}

#[test]
fn preferred_success_stops_after_one_route() {
    let routes = ["one", "two"]
        .into_iter()
        .map(|id| CalibrationRoute {
            id: id.into(),
            encoder_compute: "cpu_only".into(),
            decoder_compute: "cpu_only".into(),
            provider: "cpu".into(),
            adapter_index: None,
            adapter_name: None,
        })
        .collect();
    let mut calls = Vec::new();
    let result = run_routes(
        &vec![0.0; 480_000],
        true,
        routes,
        |route| {
            calls.push(route.id.clone());
            CalibrationAttempt {
                route: route.clone(),
                transcript: String::new(),
                wer: Some(0.0),
                latency_ms: 1,
                warm_latency_ms: None,
                deletion_run_max: 0,
                premature_ending: false,
                concentrated_deletion: false,
                status: "passed".into(),
                error: None,
                model_fingerprint: None,
                model_dir_sha256: None,
                compile_cache: None,
                diagnostics: None,
                words_per_second: None,
                max_silent_emission_gap_seconds: None,
            }
        },
        |_| Ok(()),
    );
    assert_eq!(calls, ["one"]);
    assert_eq!(result.attempts.len(), 1);
    assert!(!result.upload_eligible);
}

#[test]
fn every_gpu_is_timed_and_the_fastest_passing_one_is_kept() {
    let gpu = |index: i32| CalibrationRoute {
        id: format!("dml-{index}"),
        encoder_compute: "dml".into(),
        decoder_compute: "dml".into(),
        provider: "dml".into(),
        adapter_index: Some(index),
        adapter_name: Some(format!("GPU {index}")),
    };
    let cpu = CalibrationRoute {
        id: "cpu-only".into(),
        encoder_compute: "cpu_only".into(),
        decoder_compute: "cpu_only".into(),
        provider: "cpu".into(),
        adapter_index: None,
        adapter_name: None,
    };
    // Listed first but slower; the second GPU must win; CPU is never needed.
    let warm = |id: &str| match id {
        "dml-1" => 1_700,
        "dml-0" => 90,
        _ => 400,
    };
    let (mut calls, mut persisted) = (Vec::new(), Vec::new());
    let result = run_routes(
        &vec![0.0; 480_000],
        false,
        vec![gpu(1), gpu(0), cpu],
        |route| {
            calls.push(route.id.clone());
            CalibrationAttempt {
                route: route.clone(),
                transcript: String::new(),
                wer: Some(0.0),
                latency_ms: 5_000,
                warm_latency_ms: Some(warm(&route.id)),
                deletion_run_max: 0,
                premature_ending: false,
                concentrated_deletion: false,
                status: "passed".into(),
                error: None,
                model_fingerprint: None,
                model_dir_sha256: None,
                compile_cache: None,
                diagnostics: None,
                words_per_second: None,
                max_silent_emission_gap_seconds: None,
            }
        },
        |route| {
            persisted.push(route.adapter_index);
            Ok(())
        },
    );
    assert_eq!(calls, ["dml-1", "dml-0"]);
    assert_eq!(persisted, [Some(0)]);
    assert_eq!(result.selected_route.unwrap().id, "dml-0");
    assert!(!result.upload_eligible);
}
