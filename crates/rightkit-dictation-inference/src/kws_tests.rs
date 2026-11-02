use super::*;
#[cfg(any(target_os = "macos", target_os = "windows"))]
use serde::Deserialize;

#[test]
fn production_decoder_contract_is_cross_platform() {
    assert_eq!(NUM_THREADS, 1);
    assert_eq!(MAX_ACTIVE_PATHS, 8);
    assert_eq!(NUM_TRAILING_BLANKS, 0);
    assert_eq!(CONFIRM_CONTEXT_SAMPLES, SAMPLE_RATE as usize * 2);
    assert_eq!(FRESH_CONFIRMATION_TAIL_SAMPLES, SAMPLE_RATE as usize / 5);
    assert_eq!(command_text("@ZEPHYR_STOP"), Some("zephyr stop"));
    assert_eq!(command_text("ZEPHYR_SEND"), Some("zephyr send"));
    assert_eq!(command_text("@ZEPHYR_CANCEL"), Some("zephyr cancel"));
}

#[test]
fn hey_wake_result_passes_filter_without_becoming_command_text() {
    assert!(accepts_keyword_result("@HEY_ZEPHYR"));
    assert_eq!(command_text("@HEY_ZEPHYR"), None);
}

#[test]
fn first_decode_boundary_is_relative_to_native_stream_origin() {
    assert_eq!(next_decode_boundary(123), 123 + KWS_DECODE_STEP_SAMPLES);
    assert_eq!(
        next_decode_boundary(KWS_DECODE_STEP_SAMPLES),
        KWS_DECODE_STEP_SAMPLES * 2
    );
}

#[test]
fn cascade_clock_deduplicates_overlapping_wake_and_accrues_confirmation() {
    let mut clock = CascadeClock::default();
    assert!(clock.observe_wake(10_000, 18_000, 10_000));
    assert!(!clock.observe_wake(10_240, 18_320, 10_240));
    assert!(!clock.expire_if_needed(41_999));
    assert!(clock.expire_if_needed(42_001));
    assert!(clock.candidate().is_none());
}

#[test]
fn cascade_clock_replaces_close_non_overlapping_wake() {
    let mut clock = CascadeClock::default();
    assert!(clock.observe_wake(10_000, 14_000, 10_000));
    // A second command can begin inside the old fixed one-second
    // refractory window. It must own confirmation and the eventual cut.
    assert!(clock.observe_wake(14_001, 18_000, 14_001));
    assert_eq!(
        clock.candidate(),
        Some(WakeCandidate {
            start_sample: 14_001,
            end_sample: 18_000,
            cut_start_sample: 14_001,
        })
    );
}

#[test]
fn cascade_clock_expires_unconfirmed_wake_after_confirmation_timeout() {
    let mut clock = CascadeClock::default();
    assert!(clock.observe_wake(10_000, 14_000, 10_000));
    assert!(!clock.expire_if_needed(14_000 + CONFIRM_TIMEOUT_SAMPLES));
    assert!(clock.expire_if_needed(14_001 + CONFIRM_TIMEOUT_SAMPLES));
    assert_eq!(clock.candidate(), None);
}

#[test]
fn wake_decode_accepts_each_unseen_arrival_without_timer_gating() {
    assert_eq!(
        classify_confirmation_feed(320, 0, 640),
        ConfirmationFeed::Suffix { offset: 320 }
    );
    assert_eq!(
        classify_confirmation_feed(640, 0, 800),
        ConfirmationFeed::Suffix { offset: 640 }
    );
}

#[test]
fn persistent_confirmation_cursor_feeds_only_unseen_audio() {
    assert_eq!(
        classify_confirmation_feed(10_000, 9_000, 10_000),
        ConfirmationFeed::Duplicate
    );
    assert_eq!(
        classify_confirmation_feed(10_000, 9_000, 11_000),
        ConfirmationFeed::Suffix { offset: 1_000 }
    );
    assert_eq!(
        classify_confirmation_feed(10_000, 9_500, 11_000),
        ConfirmationFeed::Suffix { offset: 500 }
    );
}

#[test]
fn persistent_confirmation_cursor_rejects_stale_and_detects_gaps() {
    assert_eq!(
        classify_confirmation_feed(10_000, 9_000, 9_999),
        ConfirmationFeed::Stale
    );
    assert_eq!(
        classify_confirmation_feed(10_000, 10_001, 11_000),
        ConfirmationFeed::Gap
    );
}

#[test]
fn persistent_confirmation_origin_maps_timestamps_once() {
    let seed_origin = 24_000usize;
    let snapshot_start = 16_000usize;
    let snapshot_relative = seed_origin - snapshot_start;
    let result = KeywordResult {
        keyword: "@ZEPHYR_SEND".into(),
        timestamps: vec![0.5, 1.0],
        detection: None,
    };
    let mapped = transcription_result(result, snapshot_relative);
    assert_eq!(mapped.text, "zephyr send");
    assert_eq!(mapped.tokens[0].start, 1.0);
    assert_eq!(mapped.tokens[0].end, 1.58);
}

#[test]
fn confirmed_action_boundary_uses_acoustic_wake_candidate() {
    let mapped = transcription_result_at_wake(
        KeywordResult {
            keyword: "@ZEPHYR_SEND".into(),
            // Confirmation timestamps are deliberately later than wake.
            timestamps: vec![1.4, 1.9],
            detection: None,
        },
        WakeCandidate {
            start_sample: 24_000,
            end_sample: 31_000,
            cut_start_sample: 24_000,
        },
        16_000,
    );
    assert_eq!(mapped.text, "zephyr send");
    assert_eq!(mapped.tokens[0].start, 0.5);
    assert_eq!(mapped.tokens[0].end, 0.9375);
}

#[test]
fn production_cascade_resources_split_wake_from_action() {
    let resource_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../src-tauri/resources/kws");
    let wake = std::fs::read_to_string(resource_dir.join("keywords-wake.txt")).unwrap();
    let confirm = std::fs::read_to_string(resource_dir.join("keywords-confirm.txt")).unwrap();

    assert!(wake.contains("#0.05 @ZEPHYR"));
    assert!(!wake.contains("ZEPHYR_SEND"));
    assert!(confirm.contains("#0.15 @ZEPHYR_STOP"));
    assert!(confirm.contains("#0.18 @ZEPHYR_SEND"));
    assert!(confirm.contains("#0.18 @ZEPHYR_CANCEL"));
    assert_eq!(command_text("@ZEPHYR"), Some("zephyr"));
}

#[test]
fn deferred_seems_phrase_is_not_bundled_or_a_control_action() {
    let resource_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../src-tauri/resources/kws");
    let confirm = std::fs::read_to_string(resource_dir.join("keywords-confirm.txt")).unwrap();
    assert!(!confirm.contains("@ZEPHYR_SEEMS"));
    assert_eq!(command_text("@ZEPHYR_SEEMS"), None);
    assert!(!accepts_keyword_result("ZEPHYR_SEEMS"));
}

/// The always-on wake keyword must stay a SEPARATE file from the tail
/// cascade: adding "hey zephyr" must never widen what can fire a
/// STOP/SEND/CANCEL action, and the tail wake stays hey-free so ordinary
/// mid-dictation "zephyr stop" is unaffected.
#[test]
fn hey_wake_resource_is_isolated_from_tail_cascade() {
    let resource_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../src-tauri/resources/kws");
    let hey = std::fs::read_to_string(resource_dir.join(HEY_WAKE_KEYWORDS_FILE)).unwrap();
    let wake = std::fs::read_to_string(resource_dir.join("keywords-wake.txt")).unwrap();
    let confirm = std::fs::read_to_string(resource_dir.join("keywords-confirm.txt")).unwrap();

    assert!(hey.contains("HH EY1 Z EH1 F ER0"));
    assert!(hey.contains(&format!("@{HEY_WAKE_KEYWORD}")));
    assert!(!hey.contains("ZEPHYR_STOP"));
    assert!(!hey.contains("ZEPHYR_SEND"));
    assert!(!hey.contains("ZEPHYR_CANCEL"));
    assert!(!wake.contains(HEY_WAKE_KEYWORD));
    assert!(!confirm.contains(HEY_WAKE_KEYWORD));
}

#[test]
fn result_timestamps_remain_relative_to_app_owned_stream() {
    let timestamps = [0.24_f32, 0.92_f32];
    assert!(keyword_timestamps_are_plausible(&timestamps));
    assert_eq!(timestamps.first().copied(), Some(0.24));
    assert_eq!(
        timestamps.last().copied().map(|value| value + 0.08),
        Some(1.0)
    );
}

#[test]
fn rejects_keyword_paths_spanning_unrelated_speech() {
    assert!(!keyword_timestamps_are_plausible(&[]));
    assert!(!keyword_timestamps_are_plausible(&[7.6, 21.64]));
    assert!(!keyword_timestamps_are_plausible(&[1.0, 0.8]));
    assert!(keyword_timestamps_are_plausible(&[0.36, 1.48]));
}

#[test]
fn accepts_compact_path_without_inventing_a_second_timestamp_clock() {
    assert!(keyword_timestamps_are_plausible(&[7.6, 8.4]));
    assert!(keyword_timestamps_are_plausible(&[0.36, 1.12]));
}

#[cfg(target_os = "macos")]
#[test]
fn scheduled_fresh_confirmation_rejects_expired_candidate_and_bad_cursor() {
    let model_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../src-tauri/resources/kws");
    let mut kws = load_test_kws(&model_dir).expect("load bundled KWS");
    kws.clock.observe_wake(0, 800, 0);
    let live = 800 + CONFIRM_TIMEOUT_SAMPLES + 1;
    let audio = vec![0.0; live];
    let (result, onset, fresh) = kws
        .transcribe_scheduled_probe_result_with_wake_start(&audio, 0, live, true, true)
        .unwrap();
    assert!(result.text.is_empty());
    assert!(onset.is_none());
    assert!(!fresh);
    assert!(kws.clock.candidate().is_none());
    kws.clock.observe_wake(0, 800, 0);
    assert!(kws
        .transcribe_scheduled_probe_result_with_wake_start(&audio, 1, live, true, true,)
        .is_err());
    assert!(kws.clock.candidate().is_none());
}

/// Saved quiet/noise-gated sessions where the persistent wake stream missed
/// the FIRST `zephyr send` entirely and only the louder repeat fired. Replays
/// the production scheduled-probe entry and requires the action to pair with
/// a wake onset from the first attempt, not the repeat.
#[cfg(target_os = "macos")]
#[test]
#[ignore = "explicit saved quiet-session replay; diagnostic WAVs are owner-private"]
fn saved_quiet_sessions_recover_first_zephyr_send() {
    let capture_dir =
        PathBuf::from("/Users/Shared/rightkit-dictation-owner-captures/diagnostic-captures/audio");
    // (file, upper bound on wake_start seconds): the first spoken attempt
    // must own the candidate; the louder repeat sits above this bound.
    let cases = [
        ("1790872896458-5438-session-4.wav", 20.5_f32),
        ("1790872999315-5438-session-6.wav", 17.8_f32),
        ("1790873062716-5438-session-7.wav", 20.0_f32),
    ];
    let model_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../src-tauri/resources/kws");
    let mut kws = load_test_kws(&model_dir).expect("load bundled KWS cascade");
    for (name, max_wake_start_s) in cases {
        let path = capture_dir.join(name);
        assert!(path.is_file(), "missing saved session {}", path.display());
        let audio = read_test_wav(&path);
        kws.reset_stream().unwrap();
        let mut padded = audio.clone();
        padded.resize(audio.len() + CONFIRM_TIMEOUT_SAMPLES, 0.0);
        let mut live = SAMPLE_RATE as usize / 4;
        let mut confirmation_armed = false;
        let mut detected: Option<(String, usize)> = None;
        while live <= padded.len() {
            let snapshot_start = live.saturating_sub(SAMPLE_RATE as usize * 3);
            let (result, wake_start, _fresh_used) = kws
                .transcribe_scheduled_probe_result_with_wake_start(
                    &padded[snapshot_start..live],
                    snapshot_start,
                    live,
                    confirmation_armed,
                    confirmation_armed,
                )
                .unwrap_or_else(|error| panic!("{name}: {error}"));
            match result.text.as_str() {
                "zephyr" => confirmation_armed = true,
                "zephyr stop" | "zephyr send" | "zephyr cancel" => {
                    detected = Some((result.text, wake_start.expect("action owns wake onset")));
                    break;
                }
                "" => confirmation_armed = false,
                other => panic!("{name}: unexpected cascade output {other}"),
            }
            if live == padded.len() {
                break;
            }
            live = (live
                + if confirmation_armed {
                    CONFIRM_STEP_SAMPLES
                } else {
                    SAMPLE_RATE as usize / 4
                })
            .min(padded.len());
        }
        let (text, wake_start) =
            detected.unwrap_or_else(|| panic!("{name}: no zephyr send detected"));
        assert_eq!(text, "zephyr send", "{name}");
        let wake_s = wake_start as f32 / SAMPLE_RATE as f32;
        eprintln!("{name}: {text}, first wake onset {wake_s:.3}s");
        assert!(
            wake_s < max_wake_start_s,
            "{name}: wake_start {wake_s}s must be the first attempt, bound {max_wake_start_s}s"
        );
    }
}

#[cfg(target_os = "macos")]
fn replay_corpus_control(kws: &mut Kws, audio: &[f32], fresh: bool) -> Option<String> {
    replay_corpus_control_with_confirmation(kws, audio, fresh)
}

#[cfg(target_os = "macos")]
fn replay_corpus_control_with_confirmation(
    kws: &mut Kws,
    audio: &[f32],
    prefer_fresh_confirmation: bool,
) -> Option<String> {
    kws.reset_stream().expect("reset corpus stream");
    let mut padded = audio.to_vec();
    padded.resize(audio.len() + CONFIRM_TIMEOUT_SAMPLES, 0.0);
    let mut live = (SAMPLE_RATE as usize / 4).min(padded.len());
    let mut armed = false;
    loop {
        let start = live.saturating_sub(SAMPLE_RATE as usize * 3);
        let (result, _, _) = kws
            .transcribe_scheduled_probe_result_with_wake_start(
                &padded[start..live],
                start,
                live,
                armed,
                prefer_fresh_confirmation && armed,
            )
            .expect("production corpus probe");
        match result.text.as_str() {
            "zephyr" => armed = true,
            "zephyr stop" | "zephyr send" | "zephyr cancel" => return Some(result.text),
            "" => armed = false,
            other => panic!("unexpected corpus output {other}"),
        }
        if live == padded.len() {
            return None;
        }
        live = (live
            + if armed {
                CONFIRM_STEP_SAMPLES
            } else {
                SAMPLE_RATE as usize / 4
            })
        .min(padded.len());
    }
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "explicit locked local corpus regression; 252 clips, two paths"]
fn canonical_corpus_preserves_actions_without_false_triggers() {
    #[derive(Deserialize)]
    struct Clip {
        id: String,
        sha256: String,
    }
    #[derive(Deserialize)]
    struct PositiveManifest {
        raw_audio: Vec<Clip>,
    }
    let root = PathBuf::from("/Volumes/D/kwstest/canonical");
    let positives: PositiveManifest = serde_json::from_slice(
        &std::fs::read(root.join("positive14/qualification-raw.json")).unwrap(),
    )
    .unwrap();
    let negatives: Vec<Clip> = serde_json::from_slice(
        &std::fs::read(root.join("negative238/candidate_manifest.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(positives.raw_audio.len(), 14);
    assert_eq!(negatives.len(), 238);
    let model_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../src-tauri/resources/kws");
    let mut kws = load_test_kws(&model_dir).expect("load native corpus cascade");
    let mut outputs = Vec::new();
    for (positive, clips, directory) in [
        (true, positives.raw_audio, "positive14/audio"),
        (false, negatives, "negative238/candidate_audio"),
    ] {
        for clip in clips {
            let path = root.join(directory).join(format!("{}.wav", clip.id));
            let bytes = std::fs::read(&path).expect("read locked corpus WAV");
            let hex = rightkit_fs::digest::sha256_bytes(&bytes);
            assert_eq!(
                hex,
                clip.sha256.to_ascii_lowercase(),
                "changed WAV {}",
                clip.id
            );
            let audio = read_test_wav(&path);
            let baseline = replay_corpus_control(&mut kws, &audio, false);
            let candidate = replay_corpus_control(&mut kws, &audio, true);
            eprintln!(
                "corpus {}/252 {} baseline={baseline:?} candidate={candidate:?}",
                outputs.len() + 1,
                clip.id
            );
            outputs.push((clip.id, positive, baseline, candidate));
        }
    }
    // Score after both production paths have produced outputs. Neither path
    // receives truth, filename, expected action, or per-clip exceptions.
    let mut baseline_hits = 0;
    let mut candidate_hits = 0;
    let mut failures = Vec::new();
    for (id, positive, baseline, candidate) in outputs {
        if positive {
            baseline_hits += usize::from(baseline.is_some());
            candidate_hits += usize::from(candidate.is_some());
            if baseline.is_some() && baseline != candidate {
                failures.push(format!(
                    "{id}: action changed {baseline:?} -> {candidate:?}"
                ));
            }
        } else if baseline.is_some() || candidate.is_some() {
            failures.push(format!(
                "{id}: false action baseline={baseline:?} candidate={candidate:?}"
            ));
        }
    }
    eprintln!("native corpus positives baseline={baseline_hits}/14 candidate={candidate_hits}/14; negatives=238");
    assert!(
        failures.is_empty(),
        "native corpus regressions: {failures:#?}"
    );
    assert_eq!(baseline_hits, 13, "persistent control recall changed");
    assert_eq!(
        candidate_hits, 14,
        "candidate must recover all canonical controls"
    );
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "explicit labeled owner corpus regression; 42 positives"]
fn owner_positive_corpus_preserves_every_control_action() {
    #[derive(Deserialize)]
    struct Manifest {
        rows: Vec<Row>,
    }
    #[derive(Deserialize)]
    struct Row {
        id: String,
        path: String,
        sha256: String,
        expected_detection: Option<String>,
    }
    let root = PathBuf::from("/Volumes/D/kwstest");
    let models = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../src-tauri/resources/kws");
    let mut kws = load_test_kws(&models).expect("load native owner corpus cascade");
    let mut outputs = Vec::new();
    for platform in ["mac", "windows"] {
        let manifest: Manifest = serde_json::from_slice(
            &std::fs::read(root.join(format!("owner/{platform}/owner-kws-manifest.json"))).unwrap(),
        )
        .unwrap();
        for row in manifest
            .rows
            .into_iter()
            .filter(|row| row.expected_detection.is_some())
        {
            let normalized = row.path.replace('\\', "/");
            let relative = normalized
                .split_once("/kwstest/")
                .expect("canonical owner path")
                .1;
            let path = root.join(relative);
            let bytes = std::fs::read(&path).expect("read locked owner WAV");
            let hex = rightkit_fs::digest::sha256_bytes(&bytes);
            assert_eq!(
                hex,
                row.sha256.to_ascii_lowercase(),
                "changed WAV {}",
                row.id
            );
            let audio = read_test_wav(&path);
            let baseline = replay_corpus_control(&mut kws, &audio, false);
            let candidate = replay_corpus_control(&mut kws, &audio, true);
            eprintln!(
                "owner {}/42 {} baseline={baseline:?} candidate={candidate:?}",
                outputs.len() + 1,
                row.id
            );
            outputs.push((row.id, row.expected_detection.unwrap(), baseline, candidate));
        }
    }
    assert_eq!(outputs.len(), 42);
    let mut regressions = Vec::new();
    let mut baseline_misses = Vec::new();
    let mut candidate_misses = Vec::new();
    for (id, expected, baseline, candidate) in outputs {
        let expected = expected.to_ascii_lowercase().replace('_', " ");
        if baseline.as_deref() != Some(&expected) {
            baseline_misses.push(id.clone());
        }
        if candidate.as_deref() != Some(&expected) {
            candidate_misses.push(id.clone());
        }
        if baseline.as_deref() == Some(&expected) && candidate != baseline {
            regressions.push(id);
        }
    }
    eprintln!("owner baseline misses={baseline_misses:?}; candidate misses={candidate_misses:?}");
    assert!(
        regressions.is_empty(),
        "owner control regressions: {regressions:?}"
    );
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "explicit labeled owner negative corpus; 79 clips"]
fn owner_negative_corpus_rejects_false_controls() {
    let root = PathBuf::from("/Volumes/D/kwstest");
    let models = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../src-tauri/resources/kws");
    let mut kws = load_test_kws(&models).expect("load native owner corpus cascade");
    let mut checked = 0;
    let mut failures = Vec::new();
    for platform in ["mac", "windows"] {
        let manifest: serde_json::Value = serde_json::from_slice(
            &std::fs::read(root.join(format!("owner/{platform}/owner-kws-manifest.json"))).unwrap(),
        )
        .unwrap();
        for row in manifest["rows"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["expected_detection"].is_null())
        {
            let id = row["id"].as_str().unwrap();
            let normalized = row["path"].as_str().unwrap().replace('\\', "/");
            let path = root.join(normalized.split_once("/kwstest/").unwrap().1);
            let bytes = std::fs::read(&path).unwrap();
            let hex = rightkit_fs::digest::sha256_bytes(&bytes);
            assert_eq!(
                hex,
                row["sha256"].as_str().unwrap().to_ascii_lowercase(),
                "changed WAV {id}"
            );
            let audio = read_test_wav(&path);
            let baseline = replay_corpus_control(&mut kws, &audio, false);
            let candidate = replay_corpus_control(&mut kws, &audio, true);
            checked += 1;
            eprintln!(
                "owner negative {checked}/79 {id} baseline={baseline:?} candidate={candidate:?}"
            );
            if baseline.is_some() || candidate.is_some() {
                failures.push(format!(
                    "{id}: baseline={baseline:?} candidate={candidate:?}"
                ));
            }
        }
    }
    assert_eq!(checked, 79);
    assert!(failures.is_empty(), "owner false controls: {failures:#?}");
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "explicit owner Send regression diagnosis"]
fn owner_send_regression_diagnostic() {
    let audio = read_test_wav(Path::new(
        "/Volumes/D/kwstest/owner/windows/audio/1785658814809-222316-session-6.wav",
    ));
    let models = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../src-tauri/resources/kws");
    let mut kws = load_test_kws(&models).unwrap();
    let persistent = replay_corpus_control(&mut kws, &audio, false);
    let fresh_confirmation = replay_corpus_control_with_confirmation(&mut kws, &audio, true);
    eprintln!("owner Send: persistent={persistent:?}; fresh_confirmation={fresh_confirmation:?}");
    assert_eq!(persistent.as_deref(), Some("zephyr send"));
    assert_eq!(fresh_confirmation.as_deref(), Some("zephyr send"));
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "explicit known cancel-miss diagnosis; no production defaults changed"]
fn canonical_cancel_miss_confirmation_diagnostic() {
    let path = PathBuf::from("/Volumes/D/kwstest/canonical/positive14/audio/take_9.wav");
    let audio = read_test_wav(&path);
    let models = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../src-tauri/resources/kws");
    let mut kws = load_test_kws(&models).unwrap();
    let persistent = replay_corpus_control(&mut kws, &audio, false);
    let fresh_confirmation = replay_corpus_control_with_confirmation(&mut kws, &audio, true);
    eprintln!(
        "take_9 cancel: persistent={persistent:?}; fresh_confirmation={fresh_confirmation:?}"
    );
    assert_eq!(persistent, None, "known persistent miss changed");
    assert_eq!(fresh_confirmation.as_deref(), Some("zephyr cancel"));
}

#[cfg(target_os = "macos")]
#[test]
fn bundled_runtime_loads_and_accepts_silence() {
    let model_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../src-tauri/resources/kws");
    let mut kws = load_test_kws(&model_dir).expect("load bundled KWS");
    assert!(
        kws.hey_wake_spotter.is_none(),
        "idle spotter must load lazily"
    );
    let result = kws
        .transcribe_result(&vec![0.0; SAMPLE_RATE as usize / 2])
        .expect("decode silence");
    assert!(result.text.is_empty());
    assert!(result.tokens.is_empty());
    kws.reset_stream().expect("reset bundled KWS stream");
}

#[cfg(target_os = "macos")]
#[test]
fn bundled_idle_wake_stream_is_direct_and_marker_owner_lazy() {
    let model_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../src-tauri/resources/kws");
    let mut kws = load_test_kws(&model_dir).expect("load bundled KWS");
    assert!(kws.hey_wake_available());
    kws.start_idle_wake(0.25).expect("start idle wake");
    assert!(kws.hey_wake_spotter.is_some());
    assert_eq!(
        kws.feed_idle_wake(&vec![0.0; 320], 0, 320)
            .expect("feed one arrival"),
        None
    );
    kws.stop_idle_wake();
    assert!(kws.idle_wake_stream.is_none());
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "explicit saved wake-soak replay"]
fn saved_idle_wake_positive_replays_return_hits() {
    let relative_soak_root =
        Path::new("Library/Application Support/RightKitDictationTest/wake-soaks");
    let isolated_home_root = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(relative_soak_root))
        .filter(|path| path.is_dir());
    let user_home_roots = std::fs::read_dir("/Users")
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path().join(relative_soak_root))
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    let mut soak_roots = isolated_home_root
        .into_iter()
        .chain(user_home_roots)
        .collect::<Vec<_>>();
    soak_roots.sort();
    soak_roots.dedup();
    let required_chunks = [0, 8, 49];
    let mut complete_soaks = soak_roots
        .into_iter()
        .flat_map(|root| std::fs::read_dir(root).expect("open saved wake-soak root"))
        .map(|entry| entry.expect("read saved wake-soak entry").path())
        .filter(|path| path.is_dir())
        .map(|path| path.join("chunks"))
        .filter(|chunks_dir| {
            required_chunks
                .iter()
                .all(|chunk| chunks_dir.join(format!("chunk-{chunk:06}.wav")).is_file())
        })
        .collect::<Vec<_>>();
    complete_soaks.sort();
    let chunks_dir = complete_soaks
        .last()
        .expect("complete saved wake-soak exists");
    let model_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../src-tauri/resources/kws");

    for chunk in required_chunks {
        let path = chunks_dir.join(format!("chunk-{chunk:06}.wav"));
        let audio = read_test_wav(&path);
        let mut kws = load_test_kws(&model_dir).expect("load bundled KWS");
        kws.start_idle_wake(0.25).expect("start idle wake");
        let mut hit = None;
        for (chunk_index, chunk) in audio.chunks(KWS_FEED_CHUNK_SAMPLES).enumerate() {
            let start = chunk_index * KWS_FEED_CHUNK_SAMPLES;
            hit = kws
                .feed_idle_wake(chunk, start, start + chunk.len())
                .expect("feed saved wake audio");
            if hit.is_some() {
                break;
            }
        }
        assert!(hit.is_some(), "expected wake hit for {}", path.display());
    }
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "explicit owner diagnostic replay; set RIGHTKIT_KWS_BOUNDARY_WAV"]
fn owner_boundary_replay_cuts_at_acoustic_wake_before_final_asr() {
    let external_path = std::env::var_os("RIGHTKIT_KWS_BOUNDARY_WAV");
    let recent_failed_stop = PathBuf::from(
        "/Users/Shared/rightkit-dictation-owner-captures/diagnostic-captures/audio/1787554350347-83118-session-2.wav",
    );
    let (path, external_replay) = if let Some(path) = external_path {
        (PathBuf::from(path), true)
    } else if recent_failed_stop.is_file() {
        (recent_failed_stop, true)
    } else {
        (
            PathBuf::from(
                "/Users/Shared/rightkit-dictation-owner-captures/diagnostic-captures/audio/1787385919530-85728-session-2.wav",
            ),
            false,
        )
    };
    assert!(
        path.is_file(),
        "missing owner boundary WAV {}",
        path.display()
    );
    let audio = read_test_wav(&path);
    let model_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../src-tauri/resources/kws");
    let mut kws = load_test_kws(&model_dir).expect("load bundled KWS cascade");
    let mut padded = audio.clone();
    padded.resize(audio.len() + CONFIRM_TIMEOUT_SAMPLES, 0.0);
    let mut live = SAMPLE_RATE as usize / 4;
    let mut confirmation_armed = false;
    let (action, wake_start) = loop {
        let snapshot_start = live.saturating_sub(SAMPLE_RATE as usize * 3);
        let (result, wake_start) = kws
            .transcribe_cascade_result_with_wake_start(
                &padded[snapshot_start..live],
                snapshot_start,
                live,
                confirmation_armed,
            )
            .expect("decode owner boundary replay");
        match result.text.as_str() {
            "zephyr" => confirmation_armed = true,
            "zephyr stop" | "zephyr send" | "zephyr cancel" => {
                break (
                    result.text,
                    wake_start.expect("confirmed action owns wake onset"),
                );
            }
            "" => confirmation_armed = false,
            other => panic!("unexpected cascade output {other}"),
        }
        assert!(
            live < padded.len(),
            "owner replay never confirmed an action"
        );
        live = (live
            + if confirmation_armed {
                CONFIRM_STEP_SAMPLES
            } else {
                SAMPLE_RATE as usize / 4
            })
        .min(padded.len());
    };

    assert_eq!(action, "zephyr stop");
    let cut_at = match crate::resolve_trigger_audio_cut(Some(wake_start), None, Some(6_239), &audio)
    {
        crate::TriggerAudioCut::Cut { cut_at, .. } => cut_at,
        crate::TriggerAudioCut::FullBuffer { .. } => audio.len(),
    };
    assert!(cut_at <= audio.len());
    let policy =
        std::env::var("RIGHTKIT_AUDIO_CONDITIONING_POLICY").unwrap_or_else(|_| "default".into());
    let conditioned = rightkit_dictation_core::audio_conditioning::condition_for_asr(
        &audio[..cut_at],
        SAMPLE_RATE as u32,
        &policy,
    );
    let models = path
        .ancestors()
        .find_map(|ancestor| {
            let candidate = ancestor.join("models");
            candidate.is_dir().then_some(candidate)
        })
        .unwrap_or_else(|| PathBuf::from("/Users/Shared/rightkit-dictation-owner-captures/models"));
    let mut asr = crate::asr::AsrRuntime::load(&models, crate::asr::AsrEp::Cpu)
        .expect("load production final ASR");
    let final_result = asr
        .transcribe_result(&conditioned)
        .expect("transcribe owner replay before wake boundary");
    let final_text = rightkit_text_polish::text_pipeline::strip_fired_control_tail(
        &final_result.text,
        rightkit_text_polish::text_pipeline::ControlIntent::Stop,
    );
    let normalized = final_text
        .trim()
        .trim_end_matches(|c: char| c.is_ascii_punctuation())
        .to_ascii_lowercase();
    assert!(!normalized.contains("zephyr"), "final text: {final_text}");
    eprintln!(
        "owner boundary replay wake_start={wake_start} cut_at={cut_at} command_excluded={}",
        !normalized.contains("zephyr")
    );
    if external_replay {
        assert!(!normalized.is_empty(), "final text was empty");
        return;
    }
    assert!(normalized.ends_with("prompt"), "final text: {final_text}");
    assert!(
        !normalized.ends_with("prompt zero"),
        "final text: {final_text}"
    );
    let timed_tokens = final_result
        .tokens
        .into_iter()
        .map(|token| rightkit_text_polish::text_pipeline::TimedToken {
            text: token.text,
            start: token.start,
            end: token.end,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        rightkit_text_polish::text_pipeline::parse_timed_ai_transform_command(
            &final_text,
            &timed_tokens,
        )
        .map(|command| command.intent),
        Some(rightkit_text_polish::text_pipeline::AiTransformIntent::Prompt),
        "final text did not route to L2: {final_text}"
    );
}

#[cfg(target_os = "windows")]
#[test]
#[ignore = "provisioned Windows: needs the staged runtime (onnxruntime.dll in src-tauri/resources/runtime), which is not tracked"]
fn bundled_windows_runtime_matches_app_ort() {
    let resource_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../src-tauri/resources");
    // Production loader must bind bundled ORT itself, without a test preload.
    let mut kws = load_test_kws(&resource_dir.join("kws")).expect("load bundled KWS");
    let result = kws
        .transcribe_result(&vec![0.0; SAMPLE_RATE as usize / 2])
        .expect("decode silence");
    assert!(result.text.is_empty());
    assert!(result.tokens.is_empty());
    kws.reset_stream().expect("reset bundled KWS stream");
}

#[cfg(target_os = "windows")]
#[test]
#[ignore = "explicit centralized-corpus production replay"]
fn production_cascade_replays_reports() {
    #[derive(Deserialize)]
    struct Report {
        rows: Vec<ReportRow>,
    }
    #[derive(Deserialize)]
    struct ReportRow {
        row_id: String,
        audio_path: PathBuf,
        expected_detection: Option<String>,
    }

    let report_paths = std::env::var("RIGHTKIT_KWS_CASCADE_REPORTS")
        .expect("RIGHTKIT_KWS_CASCADE_REPORTS must contain semicolon-separated report paths");
    let resources = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../src-tauri/resources");
    let ort = resources.join("runtime/onnxruntime.dll");
    // The spotter runs on the process-wide ORT binding, which the engine makes at
    // startup and this replay makes here.
    bind_test_ort(&ort);
    let mut kws = Kws::load(&resources.join("kws")).expect("load bundled KWS cascade");
    let mut checked = 0usize;
    let mut positives = 0usize;
    let mut false_actions = Vec::new();
    let mut misses = Vec::new();

    for report_path in report_paths
        .split(';')
        .filter(|value| !value.trim().is_empty())
    {
        let report: Report =
            serde_json::from_slice(&std::fs::read(report_path).expect("read cascade report"))
                .expect("parse cascade report");
        for row in report.rows {
            kws.reset_stream().unwrap();
            let audio = read_test_wav(&row.audio_path);
            let mut padded = audio.clone();
            padded.resize(audio.len() + CONFIRM_TIMEOUT_SAMPLES, 0.0);
            let mut live = TAIL_WAKE_NEW_AUDIO_SAMPLES_FOR_TEST;
            let mut confirmation_armed = false;
            let mut detected = None;
            while live <= padded.len() {
                let at_end = live == padded.len();
                let snapshot_start = live.saturating_sub(SAMPLE_RATE as usize * 3);
                let result = kws
                    .transcribe_cascade_result(
                        &padded[snapshot_start..live],
                        snapshot_start,
                        live,
                        confirmation_armed,
                    )
                    .unwrap_or_else(|error| panic!("{}: {error}", row.row_id));
                match result.text.as_str() {
                    "zephyr" => confirmation_armed = true,
                    "zephyr stop" | "zephyr send" | "zephyr cancel" => {
                        detected = Some(result.text.to_ascii_uppercase().replace(' ', "_"));
                        break;
                    }
                    "" => confirmation_armed = false,
                    other => panic!("{}: unexpected cascade output {other}", row.row_id),
                }
                if at_end {
                    break;
                }
                live = (live
                    + if confirmation_armed {
                        CONFIRM_STEP_SAMPLES
                    } else {
                        SAMPLE_RATE as usize / 4
                    })
                .min(padded.len());
            }

            checked += 1;
            if let Some(expected) = row.expected_detection {
                positives += 1;
                if detected.as_deref() != Some(expected.as_str()) {
                    misses.push(format!(
                        "{} expected={expected} got={detected:?}",
                        row.row_id
                    ));
                }
            } else if let Some(action) = detected {
                false_actions.push(format!("{} got={action}", row.row_id));
            }
            if checked.is_multiple_of(10) {
                eprintln!(
                    "production cascade replay progress: checked={checked}/362 report={report_path}"
                );
            }
        }
    }

    assert_eq!(checked, 362, "centralized corpus row count changed");
    assert_eq!(positives, 46, "centralized positive count changed");
    // One assertion for both halves: failing recall first would hide the
    // precision result, and a corpus run this expensive must never report
    // half its outcome.
    assert!(
        misses.is_empty() && false_actions.is_empty(),
        "production cascade: {} misses of {positives} positives {misses:#?}\n{} false actions of {} negatives {false_actions:#?}",
        misses.len(),
        false_actions.len(),
        checked - positives
    );
}

#[cfg(target_os = "windows")]
const TAIL_WAKE_NEW_AUDIO_SAMPLES_FOR_TEST: usize = SAMPLE_RATE as usize / 4;

/// Bind ONNX Runtime once per test process through rightkit_onnx, as the engine
/// does at startup; the KWS spotter runs on that process-wide binding.
pub(super) fn bind_test_ort(path: &Path) -> PathBuf {
    static BOUND: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    let bound = BOUND.get_or_init(|| {
        let binding = rightkit_onnx::configure_runtime_with_options(
            [rightkit_onnx::explicit_file_candidate(path)],
            &rightkit_onnx::RuntimeBindingOptions {
                force_replace: true,
                ..Default::default()
            },
        )
        .unwrap_or_else(|error| panic!("bind ONNX Runtime {}: {error}", path.display()));
        rightkit_onnx::init_environment(
            &binding.selection,
            &rightkit_onnx::EnvironmentOptions::default(),
        )
        .unwrap_or_else(|error| panic!("init ONNX Runtime {}: {error}", path.display()));
        binding.selection.path
    });
    let canonical =
        |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    assert_eq!(
        canonical(bound),
        canonical(path),
        "this test process already bound another ONNX Runtime"
    );
    bound.clone()
}

/// The cascade on the bundled model, with ONNX Runtime bound first (`RIGHTKIT_TEST_ORT_DYLIB`,
/// else the runtime staged beside the app resources).
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn load_test_kws(model_dir: &Path) -> Result<Kws, String> {
    #[cfg(target_os = "macos")]
    const ORT_LIBRARY: &str = "libonnxruntime.dylib";
    #[cfg(target_os = "windows")]
    const ORT_LIBRARY: &str = "onnxruntime.dll";
    let ort = std::env::var_os("RIGHTKIT_TEST_ORT_DYLIB")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../src-tauri/resources/runtime")
                .join(ORT_LIBRARY)
        });
    bind_test_ort(&ort);
    Kws::load(model_dir)
}

pub(super) fn read_test_wav(path: &Path) -> Vec<f32> {
    let wav = rightkit_audio::wav::read_wav_mono_f32(path)
        .unwrap_or_else(|error| panic!("open {}: {error}", path.display()));
    assert_eq!(wav.sample_rate, SAMPLE_RATE as u32, "{}", path.display());
    wav.samples
}
