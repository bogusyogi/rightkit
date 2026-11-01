use super::*;

#[test]
fn quarter_rms_is_content_free_and_handles_empty_quarters() {
    assert_eq!(quarter_rms(&[]), [0.0; 4]);
    assert_eq!(quarter_rms(&[1.0, -1.0, 2.0, -2.0]), [1.0, 1.0, 2.0, 2.0]);
}

#[test]
fn gap_list_preserves_leading_middle_and_trailing_positions() {
    let gaps = no_emission_gaps_over_2s(&[(1, 2.4, 1), (2, 5.6, 1)], 0.0, 100);
    assert_eq!(
        gaps,
        vec![
            NoEmissionGap {
                frame_start: 0,
                frame_len: 30,
            },
            NoEmissionGap {
                frame_start: 31,
                frame_len: 39,
            },
            NoEmissionGap {
                frame_start: 71,
                frame_len: 29,
            },
        ]
    );
}

#[test]
fn margin_uses_effective_bias_scores_without_token_identity() {
    let bonuses = std::collections::HashMap::from([(1, 0.5)]);
    let margin = top1_top2_margin(&[2.0, 1.7, f32::NAN], &bonuses).unwrap();
    assert!((margin - 0.2).abs() < 1e-5);
}

#[test]
fn late_quarter_health_localizes_flattening_and_nonfinite_values() {
    let health = quartered_tensor_health(&[1.0, -1.0, 2.0, -2.0, 3.0, -3.0, 0.0, f32::NAN]);
    assert_eq!(health[0].nonfinite, 0);
    assert_eq!(health[2].min, -3.0);
    assert_eq!(health[3].rms, 0.0);
    assert_eq!(health[3].min, 0.0);
    assert_eq!(health[3].max, 0.0);
    assert_eq!(health[3].nonfinite, 1);
}

#[test]
fn take_and_clear_prevent_stale_window_reuse() {
    publish_asr_window_stats(AsrWindowStats {
        frames_total: 7,
        ..AsrWindowStats::default()
    });
    assert_eq!(take_latest_asr_window_stats().unwrap().frames_total, 7);
    assert!(latest_asr_window_stats().is_none());
    publish_asr_window_stats(AsrWindowStats {
        frames_total: 9,
        ..AsrWindowStats::default()
    });
    clear_latest_asr_window_stats();
    assert!(take_latest_asr_window_stats().is_none());
}

#[test]
fn report_serializes_without_audio_or_token_content() {
    let json = serde_json::to_string(&AsrWindowStats {
        frames_total: 4,
        ..AsrWindowStats::default()
    })
    .unwrap();
    assert!(json.contains("\"frames_total\":4"));
    assert!(!json.contains("transcript"));
    assert!(!json.contains("token_id"));
}

#[test]
fn envelope_has_stable_event_contract() {
    let stats = AsrWindowStats {
        frames_total: 4,
        ..AsrWindowStats::default()
    };
    let value = serde_json::to_value(AsrWindowStatsEnvelope::new(
        &stats,
        "fnv1a64:test",
        Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        "cache_hit",
        "neural_engine",
        "cpu_gpu",
        "cpu_gpu",
        "cpu_only",
        12,
    ))
    .unwrap();
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["event"], "asr_window_stats");
    assert_eq!(value["model_fingerprint"], "fnv1a64:test");
    assert_eq!(
        value["model_dir_sha256"],
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    );
    assert_eq!(value["coreml_compile_cache"], "cache_hit");
    assert!(value["raw_pcm_rms_quarters"].is_null());
    assert!(value["recording_id"].is_null());
    assert!(value["window_type"].is_null());
    assert!(value["vad_speech_state_quarters"][0].is_null());
    assert_eq!(value["mel_compute"], "neural_engine");
    assert_eq!(value["joint_compute"], "cpu_gpu");
    assert_eq!(value["stats"]["frames_total"], 4);
    assert_eq!(value["encoder_compute"], "cpu_gpu");
    assert_eq!(value["decoder_compute"], "cpu_only");
    assert_eq!(value["elapsed_ms"], 12);
}
