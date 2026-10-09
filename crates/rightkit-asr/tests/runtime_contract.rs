use rightkit_ort::ort::session::builder::GraphOptimizationLevel;
use rightkit_asr::{ExecutionConfig, ExecutionProvider, Transducer, TransducerOptions};

#[test]
fn execution_controls_are_typed_and_app_neutral() {
    let config = ExecutionConfig::new()
        .with_execution_provider(ExecutionProvider::Cpu)
        .with_optimization_level(GraphOptimizationLevel::Level1)
        .with_device_id(2)
        .with_auxiliary_execution_provider(ExecutionProvider::Cpu);

    assert_eq!(config.execution_provider, ExecutionProvider::Cpu);
    assert_eq!(config.optimization_level, GraphOptimizationLevel::Level1);
    assert_eq!(config.device_id, Some(2));
    assert_eq!(config.auxiliary_execution_provider, ExecutionProvider::Cpu);
}

#[test]
fn transducer_defaults_lock_reconciliation_decisions() {
    let options = TransducerOptions::default();
    assert_eq!(options.max_tokens_per_step, None);
    assert_eq!(options.duration_cap, 2);
    assert_eq!(options.window_overlap_mel, 0);
    assert!(!options.debug_diagnostics);
}

#[test]
fn transducer_options_are_explicit_and_deterministic() {
    let options = TransducerOptions::new()
        .with_max_tokens_per_step(7)
        .with_duration_cap(3)
        .with_window_overlap_mel(128)
        .with_debug_diagnostics(true);

    assert_eq!(options.max_tokens_per_step, Some(7));
    assert_eq!(options.duration_cap, 3);
    assert_eq!(options.window_overlap_mel, 128);
    assert!(options.debug_diagnostics);
}

#[test]
fn shared_source_contains_no_app_prefixed_policy() {
    let source = [
        include_str!("../src/execution.rs"),
        include_str!("../src/model_nemotron.rs"),
        include_str!("../src/model_transducer.rs"),
        include_str!("../src/transducer.rs"),
    ]
    .join("\n");

    for forbidden in ["HR_", "HeardRight", "ScrapeRight", "HrTransducer", "ScrapeRightTransducer"] {
        assert!(!source.contains(forbidden), "shared source contains {forbidden}");
    }

    assert!(source.contains("ep.dml.disable_graph_fusion"));
    assert!(source.contains("config.window_overlap_mel"));
    assert!(source.contains("auxiliary_config()"));
}

#[test]
fn model_contract_rejects_a_missing_directory_before_runtime_loading() {
    let missing = std::env::temp_dir().join("rightkit-asr-contract-missing");
    let error = Transducer::load(&missing, None)
        .err()
        .expect("missing model directory must fail");
    assert!(error.to_string().contains("model path must be a directory"));
}

#[test]
fn provenance_and_patch_ledger_are_bundled_and_exact() {
    let upstream = include_str!("../UPSTREAM.toml");
    let ledger = include_str!("../PATCHES.md");
    let notice = include_str!("../THIRD_PARTY_NOTICES.md");
    let license = include_str!("../LICENSE");

    assert!(upstream.contains("7d680db3d62e6aeaf68bc93aaae0ab891a09fae7"));
    for patch in [
        "RKASR-001",
        "RKASR-002",
        "RKASR-003",
        "RKASR-004",
        "RKASR-005",
        "RKASR-006",
    ] {
        assert!(upstream.contains(patch));
        assert!(ledger.contains(patch));
    }
    assert!(notice.contains("Copyright (c) 2025 Enes Altun"));
    assert!(license.contains("Permission is hereby granted"));
}
