//! The unchanged cascade policy on the Rust keyword spotter (`rightkit_asr::kws`),
//! and a per-snapshot A/B against the Sherpa C API.
//!
//! Real-audio tests print a `SKIP` line and pass when an input is absent:
//! - `RIGHTKIT_TEST_ORT_DYLIB`: ONNX Runtime library, bound once through rightkit_onnx.
//! - `RIGHTKIT_TEST_KWS_DIR`: KWS model directory (default: the bundled `src-tauri/resources/kws`).
//! - `RIGHTKIT_SHERPA_LIBRARY_PATH` (A/B only): Sherpa C API library. On macOS and Windows the
//!   bundled runtime is used when the variable is unset. Point `RIGHTKIT_TEST_ORT_DYLIB` at the
//!   ONNX Runtime that Sherpa library links, so one runtime serves both backends.
//! - `RIGHTKIT_TEST_KWS_AB_WAVS` (A/B only, optional): semicolon-separated 16 kHz WAVs replayed
//!   after the dictation fixtures, so the A/B also covers clips that fire keywords.

use super::tests::{bind_test_ort, read_test_wav};
use super::*;

/// Dictation clips without a spoken command: the cascade must never latch one.
const NEGATIVE_FIXTURES: [&str; 3] = ["short_9s.wav", "long_38s.wav", "filler_retake.wav"];
const IDLE_WAKE_THRESHOLD: f32 = 0.25;

/// What the Rust backend must equal for the Sherpa library under test.
/// macOS/Windows link the the app-patched build, which IS the production
/// contract. Other platforms have only stock upstream Sherpa (GCC build): no
/// active-path TTL, the 1.5 s trailing-blank reset, and an f64 joiner softmax.
#[cfg(any(target_os = "macos", target_os = "windows"))]
const SHERPA_REFERENCE_TUNING: RustKwsTuning = RustKwsTuning::PRODUCTION;
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const SHERPA_REFERENCE_TUNING: RustKwsTuning = RustKwsTuning {
    active_path_ttl_frames: None,
    trailing_blank_reset: true,
    softmax_f64: true,
    // Onset estimation stays on: the A/B then also proves it never changes a result.
    ..RustKwsTuning::PRODUCTION
};

#[test]
fn backend_switch_parses_and_defaults_per_platform() {
    assert_eq!(KwsBackend::parse("rust"), Some(KwsBackend::Rust));
    assert_eq!(KwsBackend::parse(" Sherpa\n"), Some(KwsBackend::Sherpa));
    assert_eq!(KwsBackend::parse("onnx"), None);
    assert_eq!(KwsBackend::parse(""), None);
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    assert_eq!(KwsBackend::DEFAULT, KwsBackend::Sherpa);
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    assert_eq!(KwsBackend::DEFAULT, KwsBackend::Rust);
}

#[test]
fn rust_backend_reproduces_the_patched_sherpa_contract() {
    assert_eq!(ACTIVE_KEYWORD_TTL_FRAMES, 38);
    assert_eq!(
        RustKwsTuning::PRODUCTION,
        RustKwsTuning {
            active_path_ttl_frames: Some(38),
            trailing_blank_reset: false,
            softmax_f64: false,
            onset: true,
            threshold_tokens: rust_kws::ThresholdTokens::Upstream,
            joiner_batching: rust_kws::JoinerBatching::Beam,
            merge_timestamps: rust_kws::MergeTimestamps::First,
            portable_math: false,
            static_batch: false,
            decoder_cache: false,
            shared_model: false,
        }
    );
    // No switch set: the shipped contract, cutting on the emission timestamp.
    assert_eq!(
        KwsSwitches::default().tuning(RustKwsTuning::PRODUCTION),
        RustKwsTuning::PRODUCTION
    );
    assert_eq!(KwsSwitches::default().onset, Some(KwsOnsetMethod::Emission));
    assert_eq!(KwsSwitches::default().onset_shift_ms, 0);
    let manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../src-tauri/resources/kws/manifest.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        manifest["native_patch"]["active_keyword_ttl_frames"],
        ACTIVE_KEYWORD_TTL_FRAMES
    );
    assert_eq!(manifest["runtime"]["num_threads"], NUM_THREADS);
    assert_eq!(manifest["runtime"]["max_active_paths"], MAX_ACTIVE_PATHS);
    assert_eq!(
        manifest["runtime"]["num_trailing_blanks"],
        NUM_TRAILING_BLANKS
    );
}

#[test]
fn switches_parse_every_value_and_keep_defaults_on_bad_input() {
    let parse = |pairs: &[(&str, &str)]| {
        let map: std::collections::HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        KwsSwitches::from_lookup(|name| map.get(name).cloned())
    };
    let (none, warnings) = parse(&[]);
    assert_eq!(none, KwsSwitches::default());
    assert!(warnings.is_empty());

    let (all, warnings) = parse(&[
        ("RIGHTKIT_KWS_ONSET", " Acoustic"),
        ("RIGHTKIT_KWS_ONSET_SHIFT_MS", "-120"),
        ("RIGHTKIT_KWS_THRESHOLD_TOKENS", "matched"),
        ("RIGHTKIT_KWS_JOINER_BATCHING", "per-hypothesis"),
        ("RIGHTKIT_KWS_MERGE_TIMESTAMPS", "Likelier"),
        ("RIGHTKIT_KWS_PORTABLE_MATH", "1"),
        ("RIGHTKIT_KWS_TTL_FRAMES", "50"),
        ("RIGHTKIT_KWS_STATIC_BATCH", "1"),
        ("RIGHTKIT_KWS_DECODER_CACHE", "on"),
        ("RIGHTKIT_KWS_SHARED_MODEL", "true"),
    ]);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(
        all,
        KwsSwitches {
            onset: Some(KwsOnsetMethod::Acoustic),
            onset_shift_ms: -120,
            threshold_tokens: rust_kws::ThresholdTokens::MatchedKeyword,
            joiner_batching: rust_kws::JoinerBatching::PerHypothesis,
            merge_timestamps: rust_kws::MergeTimestamps::Likelier,
            portable_math: true,
            ttl_frames: Some(50),
            static_batch: true,
            decoder_cache: true,
            shared_model: true,
        }
    );
    let tuning = all.tuning(RustKwsTuning::PRODUCTION);
    assert_eq!(tuning.active_path_ttl_frames, Some(50));
    assert_eq!(tuning.merge_timestamps, rust_kws::MergeTimestamps::Likelier);
    assert!(tuning.onset && tuning.portable_math && !tuning.trailing_blank_reset);
    assert!(tuning.static_batch && tuning.decoder_cache && tuning.shared_model);

    for method in ["emission", "posterior", "aligned", "acoustic"] {
        let (s, _) = parse(&[("RIGHTKIT_KWS_ONSET", method)]);
        assert_eq!(s.onset.map(KwsOnsetMethod::as_str), Some(method));
    }
    let (off, _) = parse(&[
        ("RIGHTKIT_KWS_ONSET", "off"),
        ("RIGHTKIT_KWS_TTL_FRAMES", "OFF"),
    ]);
    assert_eq!((off.onset, off.ttl_frames), (None, None));
    assert!(!off.tuning(RustKwsTuning::PRODUCTION).onset);

    let (bad, warnings) = parse(&[
        ("RIGHTKIT_KWS_ONSET", "onset"),
        ("RIGHTKIT_KWS_ONSET_SHIFT_MS", "2000"),
        ("RIGHTKIT_KWS_THRESHOLD_TOKENS", "first"),
        ("RIGHTKIT_KWS_JOINER_BATCHING", "batch"),
        ("RIGHTKIT_KWS_MERGE_TIMESTAMPS", "last"),
        ("RIGHTKIT_KWS_PORTABLE_MATH", "maybe"),
        ("RIGHTKIT_KWS_TTL_FRAMES", "0"),
        ("RIGHTKIT_KWS_STATIC_BATCH", "2"),
        ("RIGHTKIT_KWS_DECODER_CACHE", "yes"),
        ("RIGHTKIT_KWS_SHARED_MODEL", ""),
    ]);
    assert_eq!(bad, KwsSwitches::default());
    assert_eq!(warnings.len(), 10, "{warnings:?}");
}

#[test]
fn onset_samples_are_exact_multiples_of_the_feature_shift() {
    let estimate = |feature_frame| rust_kws::OnsetEstimate {
        feature_frame,
        seconds: feature_frame as f32 * 0.01,
    };
    assert_eq!(onset_sample(16_000, estimate(275)), 16_000 + 275 * 160);
    assert_eq!(onset_sample(7, estimate(-3)), 7);
    // Emission frame t is feature frame 4t: the same sample the timestamp path gives.
    for t in [0, 1, 73, 131, 999] {
        assert_eq!(
            onset_sample(0, estimate(4 * t)),
            seconds_to_samples(sherpa_json_timestamp(0.04 * t as f32))
        );
    }
}

#[test]
fn rust_timestamps_take_sherpa_json_rounding() {
    // Sherpa prints `%.2f`; frame 3 is 0.120000005 as f32 either way.
    assert_eq!(sherpa_json_timestamp(0.04 * 3.0), 0.12);
    assert_eq!(sherpa_json_timestamp(1.004_999), 1.0);
    assert_eq!(sherpa_json_timestamp(0.0), 0.0);
    assert_eq!(sherpa_json_timestamp(7.6), 7.6);
}

fn test_model_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("RIGHTKIT_TEST_KWS_DIR").map(PathBuf::from) {
        validate_model_dir(&dir).unwrap_or_else(|error| panic!("RIGHTKIT_TEST_KWS_DIR: {error}"));
        return Some(dir);
    }
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../src-tauri/resources/kws");
    if validate_model_dir(&dir).is_ok() {
        Some(dir)
    } else {
        eprintln!(
            "SKIP: no KWS model at {}; set RIGHTKIT_TEST_KWS_DIR",
            dir.display()
        );
        None
    }
}

/// The model directory with ONNX Runtime bound, or `None` after a SKIP line.
fn rust_backend_inputs(test: &str) -> Option<PathBuf> {
    let Some(ort) = std::env::var_os("RIGHTKIT_TEST_ORT_DYLIB").map(PathBuf::from) else {
        eprintln!("SKIP {test}: set RIGHTKIT_TEST_ORT_DYLIB to an ONNX Runtime library");
        return None;
    };
    let model_dir = test_model_dir()?;
    bind_test_ort(&ort);
    Some(model_dir)
}

fn fixture(name: &str) -> Vec<f32> {
    read_test_wav(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../rightkit-dictation-engine/tests/fixtures")
            .join(name),
    )
}

#[derive(Clone, Copy, Debug)]
enum Drive {
    /// Persistent wake + confirmation streams only.
    Persistent,
    /// The worker's fresh-confirmation schedule once a wake arms it.
    FreshConfirmation,
}

/// One cascade call: absolute live cursor, then the outcome. Token times are
/// compared bit for bit.
type TraceStep = (
    usize,
    Result<(String, Vec<(String, u32, u32)>, Option<usize>, bool), String>,
);

/// Replay a clip through `transcribe_scheduled_probe_result_with_wake_start`
/// exactly as the corpus tests drive it (3 s snapshots, 250 ms hop, 50 ms once
/// armed), recording every call. Returns the trace and any latched action.
fn replay_trace(
    kws: &mut SherpaKws,
    audio: &[f32],
    drive: Drive,
) -> (Vec<TraceStep>, Option<String>) {
    kws.reset_stream().expect("reset cascade");
    let mut padded = audio.to_vec();
    padded.resize(audio.len() + CONFIRM_TIMEOUT_SAMPLES, 0.0);
    let mut live = (SAMPLE_RATE as usize / 4).min(padded.len());
    let mut armed = false;
    let mut trace = Vec::new();
    loop {
        let start = live.saturating_sub(SAMPLE_RATE as usize * 3);
        let fresh = matches!(drive, Drive::FreshConfirmation) && armed;
        let step = kws.transcribe_scheduled_probe_result_with_wake_start(
            &padded[start..live],
            start,
            live,
            armed,
            fresh,
        );
        let mut action = None;
        let outcome = step.map(|(result, wake_start, fresh_used)| {
            match result.text.as_str() {
                "zephyr" => armed = true,
                "zephyr stop" | "zephyr send" | "zephyr cancel" => {
                    action = Some(result.text.clone());
                }
                "" => {}
                other => panic!("unexpected cascade output {other}"),
            }
            let tokens = result
                .tokens
                .iter()
                .map(|token| {
                    (
                        token.text.clone(),
                        token.start.to_bits(),
                        token.end.to_bits(),
                    )
                })
                .collect();
            (result.text, tokens, wake_start, fresh_used)
        });
        trace.push((live, outcome));
        if action.is_some() || live == padded.len() {
            return (trace, action);
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

/// One idle-wake event: the clip sample its arrival ended at, then the hit span or error.
type IdleWakeEvent = (usize, Result<(usize, usize), String>);

/// Feed the always-on hey-wake stream in 20 ms arrivals, restarting after each
/// hit or error, as the idle owner does after a hit.
fn idle_wake_trace(kws: &mut SherpaKws, audio: &[f32]) -> Vec<IdleWakeEvent> {
    kws.start_idle_wake(IDLE_WAKE_THRESHOLD)
        .expect("start idle wake");
    let mut events = Vec::new();
    let mut origin = 0usize;
    for (index, chunk) in audio.chunks(KWS_FEED_CHUNK_SAMPLES).enumerate() {
        let start = index * KWS_FEED_CHUNK_SAMPLES;
        let end = start + chunk.len();
        match kws.feed_idle_wake(chunk, start - origin, end - origin) {
            Ok(None) => {}
            Ok(Some(hit)) => {
                events.push((end, Ok(hit)));
                kws.start_idle_wake(IDLE_WAKE_THRESHOLD)
                    .expect("restart idle wake");
                origin = end;
            }
            Err(error) => {
                events.push((end, Err(error)));
                kws.start_idle_wake(IDLE_WAKE_THRESHOLD)
                    .expect("restart idle wake");
                origin = end;
            }
        }
    }
    kws.stop_idle_wake();
    events
}

#[test]
fn rust_backend_loads_and_accepts_silence() {
    let Some(model_dir) = rust_backend_inputs("rust_backend_loads_and_accepts_silence") else {
        return;
    };
    let mut kws =
        SherpaKws::load_with_backend(&model_dir, KwsBackend::Rust).expect("load Rust KWS cascade");
    assert_eq!(kws.backend(), KwsBackend::Rust);
    assert!(
        kws.hey_wake_spotter.is_none(),
        "idle spotter must load lazily"
    );
    let result = kws
        .transcribe_result(&vec![0.0; SAMPLE_RATE as usize / 2])
        .expect("decode silence");
    assert!(result.text.is_empty());
    assert!(result.tokens.is_empty());
    assert!(!kws
        .detect_hey_wake(&vec![0.0; SAMPLE_RATE as usize])
        .unwrap());
    kws.start_idle_wake(IDLE_WAKE_THRESHOLD)
        .expect("start idle wake");
    assert!(kws.hey_wake_spotter.is_some());
    assert_eq!(
        kws.feed_idle_wake(&vec![0.0; 320], 0, 320)
            .expect("feed one arrival"),
        None
    );
    kws.stop_idle_wake();
    assert!(kws.idle_wake_stream.is_none());
    kws.reset_stream().expect("reset Rust KWS stream");
    let replay = SherpaKws::load_for_replay_with_paths_and_backend(&model_dir, 4, KwsBackend::Rust)
        .expect("load Rust replay cascade");
    assert_eq!(replay.backend(), KwsBackend::Rust);
}

#[test]
fn rust_backend_cascade_latches_no_command_on_dictation() {
    let Some(model_dir) =
        rust_backend_inputs("rust_backend_cascade_latches_no_command_on_dictation")
    else {
        return;
    };
    let mut kws =
        SherpaKws::load_with_backend(&model_dir, KwsBackend::Rust).expect("load Rust KWS cascade");
    let mut false_actions = Vec::new();
    for name in NEGATIVE_FIXTURES {
        let audio = fixture(name);
        for drive in [Drive::Persistent, Drive::FreshConfirmation] {
            let (trace, action) = replay_trace(&mut kws, &audio, drive);
            let wakes = trace
                .iter()
                .filter(|(_, step)| matches!(step, Ok((text, ..)) if text == "zephyr"))
                .count();
            let errors: Vec<_> = trace
                .iter()
                .filter_map(|(live, step)| step.as_ref().err().map(|error| (live, error)))
                .collect();
            eprintln!(
                "rust {name} {drive:?}: snapshots={} wake_outputs={wakes} action={action:?} errors={errors:?}",
                trace.len()
            );
            if let Some(action) = action {
                false_actions.push(format!("{name} {drive:?}: {action}"));
            }
        }
    }
    assert!(
        false_actions.is_empty(),
        "Rust backend latched commands on dictation: {false_actions:#?}"
    );
}

/// The dictation fixtures, then any `RIGHTKIT_TEST_KWS_AB_WAVS` clips.
fn ab_clips() -> Vec<(String, Vec<f32>)> {
    let mut clips: Vec<_> = NEGATIVE_FIXTURES
        .iter()
        .map(|name| (name.to_string(), fixture(name)))
        .collect();
    if let Some(extra) = std::env::var_os("RIGHTKIT_TEST_KWS_AB_WAVS") {
        for path in extra
            .to_string_lossy()
            .split(';')
            .filter(|path| !path.trim().is_empty())
        {
            let path = PathBuf::from(path.trim());
            clips.push((path.display().to_string(), read_test_wav(&path)));
        }
    }
    clips
}

fn non_empty_outputs(trace: &[TraceStep]) -> usize {
    trace
        .iter()
        .filter(|(_, step)| !matches!(step, Ok((text, ..)) if text.is_empty()))
        .count()
}

/// Run every clip on both backends and diff the per-snapshot traces.
/// Returns human-readable divergences (first differing call per clip/drive).
fn ab_divergences(model_dir: &Path, tuning: RustKwsTuning) -> (Vec<String>, usize) {
    let mut sherpa = SherpaKws::load_with_paths(
        model_dir,
        MAX_ACTIVE_PATHS,
        KwsBackend::Sherpa,
        RustKwsTuning::PRODUCTION,
    )
    .expect("load Sherpa KWS cascade");
    let mut rust =
        SherpaKws::load_with_paths(model_dir, MAX_ACTIVE_PATHS, KwsBackend::Rust, tuning)
            .expect("load Rust KWS cascade");
    assert_eq!(sherpa.backend(), KwsBackend::Sherpa);
    assert_eq!(rust.backend(), KwsBackend::Rust);
    cascade_divergences(
        ("sherpa", &mut sherpa),
        ("rust", &mut rust),
        &ab_clips(),
        false,
    )
}

/// Replay every clip on two cascades and diff their per-snapshot traces (both
/// drives) and idle-wake hits; with `reports`, also every onset report (token
/// probabilities, scores and onset candidates, compared exactly). Returns the
/// first divergence per clip/drive and the number of calls compared.
fn cascade_divergences(
    (expected_name, expected_kws): (&str, &mut SherpaKws),
    (actual_name, actual_kws): (&str, &mut SherpaKws),
    clips: &[(String, Vec<f32>)],
    reports: bool,
) -> (Vec<String>, usize) {
    let mut divergences = Vec::new();
    let mut compared = 0usize;
    for (name, audio) in clips {
        for drive in [Drive::Persistent, Drive::FreshConfirmation] {
            let (expected, expected_action) = replay_trace(expected_kws, audio, drive);
            let (actual, actual_action) = replay_trace(actual_kws, audio, drive);
            compared += expected.len().min(actual.len());
            eprintln!(
                "A/B {name} {drive:?}: {expected_name} snapshots={} non_empty={} action={expected_action:?}; {actual_name} snapshots={} non_empty={} action={actual_action:?}",
                expected.len(),
                non_empty_outputs(&expected),
                actual.len(),
                non_empty_outputs(&actual)
            );
            if let Some(index) = (0..expected.len().max(actual.len()))
                .find(|&index| expected.get(index) != actual.get(index))
            {
                divergences.push(format!(
                    "{name} {drive:?} call {index}: {expected_name}={:?} {actual_name}={:?}",
                    expected.get(index),
                    actual.get(index)
                ));
            }
            let (expected, actual) = (
                expected_kws.take_onset_reports(),
                actual_kws.take_onset_reports(),
            );
            if reports && expected != actual {
                divergences.push(format!(
                    "{name} {drive:?} onset reports: {expected_name}={expected:#?} {actual_name}={actual:#?}"
                ));
            }
        }
        let expected = idle_wake_trace(expected_kws, audio);
        let actual = idle_wake_trace(actual_kws, audio);
        compared += audio.len().div_ceil(KWS_FEED_CHUNK_SAMPLES);
        eprintln!(
            "A/B {name} idle-wake: {expected_name} hits={expected:?}; {actual_name} hits={actual:?}"
        );
        if expected != actual {
            divergences.push(format!(
                "{name} idle-wake: {expected_name}={expected:?} {actual_name}={actual:?}"
            ));
        }
        let (expected, actual) = (
            expected_kws.take_onset_reports(),
            actual_kws.take_onset_reports(),
        );
        if reports && expected != actual {
            divergences.push(format!(
                "{name} idle-wake onset reports: {expected_name}={expected:#?} {actual_name}={actual:#?}"
            ));
        }
    }
    (divergences, compared)
}

fn sherpa_ab_inputs(test: &str) -> Option<PathBuf> {
    let model_dir = rust_backend_inputs(test)?;
    if resolve_library_path().is_none() {
        eprintln!("SKIP {test}: set RIGHTKIT_SHERPA_LIBRARY_PATH to a Sherpa C API library");
        return None;
    }
    Some(model_dir)
}

/// Both backends produce identical outputs on every cascade snapshot and every
/// idle-wake arrival, with the Rust spotter configured as the Sherpa library
/// under test is built ([`SHERPA_REFERENCE_TUNING`]).
#[test]
fn sherpa_and_rust_backends_match_per_snapshot() {
    let Some(model_dir) = sherpa_ab_inputs("sherpa_and_rust_backends_match_per_snapshot") else {
        return;
    };
    let (divergences, compared) = ab_divergences(&model_dir, SHERPA_REFERENCE_TUNING);
    eprintln!(
        "A/B reference tuning {SHERPA_REFERENCE_TUNING:?}: {compared} calls compared, {} divergences",
        divergences.len()
    );
    assert!(
        divergences.is_empty(),
        "Sherpa and Rust KWS backends diverged: {divergences:#?}"
    );
}

/// Off macOS/Windows the Sherpa library is stock upstream, so the shipped Rust
/// contract (38-frame TTL, no trailing-blank reset, f32 softmax) is reported
/// against it rather than asserted.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
#[test]
fn production_rust_contract_against_upstream_sherpa_report() {
    let Some(model_dir) =
        sherpa_ab_inputs("production_rust_contract_against_upstream_sherpa_report")
    else {
        return;
    };
    let (divergences, compared) = ab_divergences(&model_dir, RustKwsTuning::PRODUCTION);
    eprintln!(
        "A/B production tuning vs upstream Sherpa: {compared} calls compared, {} divergences: {divergences:#?}",
        divergences.len()
    );
}

/// A scratch directory removed on drop.
struct ScratchDir(PathBuf);

impl ScratchDir {
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A model directory whose wake and confirm keywords fire on the dictation
/// fixtures ("and" as the wake word, "and" again as SEND), so the whole
/// cascade latches on bundled audio and every onset path runs.
fn firing_model_dir(test: &str) -> Option<ScratchDir> {
    let model_dir = rust_backend_inputs(test)?;
    let dir = ScratchDir(
        std::env::temp_dir().join(format!("hr-kws-onset-{}-{test}", std::process::id())),
    );
    std::fs::create_dir_all(dir.path()).expect("create scratch model dir");
    for file in [ENCODER, DECODER, JOINER, "tokens.txt"] {
        std::fs::copy(model_dir.join(file), dir.path().join(file)).expect("copy model file");
    }
    std::fs::write(
        dir.path().join("keywords-wake.txt"),
        "AH0 N D :2 #0.05 @ZEPHYR\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("keywords-confirm.txt"),
        "AH0 N D :2 #0.05 @ZEPHYR_SEND\n",
    )
    .unwrap();
    Some(dir)
}

/// The latched action and its cut for one onset choice, plus the reports.
fn latch_with(
    model_dir: &Path,
    audio: &[f32],
    switches: KwsSwitches,
) -> (Option<String>, Option<usize>, Vec<KwsOnsetReport>) {
    let mut kws = SherpaKws::load_with_switches(model_dir, KwsBackend::Rust, switches)
        .expect("load Rust KWS cascade");
    kws.reset_stream().unwrap();
    let mut padded = audio.to_vec();
    padded.resize(audio.len() + CONFIRM_TIMEOUT_SAMPLES, 0.0);
    let mut live = SAMPLE_RATE as usize / 4;
    let mut armed = false;
    loop {
        let start = live.saturating_sub(SAMPLE_RATE as usize * 3);
        let (result, wake_start, _) = kws
            .transcribe_scheduled_probe_result_with_wake_start(
                &padded[start..live],
                start,
                live,
                armed,
                false,
            )
            .expect("cascade snapshot");
        match result.text.as_str() {
            "zephyr" => armed = true,
            "" => {}
            action => {
                return (
                    Some(action.to_string()),
                    wake_start,
                    kws.take_onset_reports(),
                )
            }
        }
        if live == padded.len() {
            return (None, None, kws.take_onset_reports());
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

/// Every onset choice latches the same action on the same snapshot; only the
/// cut moves, to exactly the candidate the wake report logged (or the
/// emission when that candidate is absent), and the default reproduces the
/// emission cut of a cascade with estimation off.
#[test]
fn onset_choice_moves_only_the_cut_and_logs_every_candidate() {
    let Some(dir) = firing_model_dir("onset_choice_moves_only_the_cut_and_logs_every_candidate")
    else {
        return;
    };
    let audio = fixture("short_9s.wav");
    let off = KwsSwitches {
        onset: None,
        ..KwsSwitches::default()
    };
    let (action_off, cut_off, reports_off) = latch_with(dir.path(), &audio, off);
    assert!(reports_off.is_empty(), "estimation off must log nothing");
    let action_off = action_off.expect("the fixture's \"and\" latches SEND");
    assert_eq!(action_off, "zephyr send");
    let cut_off = cut_off.expect("a latched action carries its cut");

    let (action, cut, reports) = latch_with(dir.path(), &audio, KwsSwitches::default());
    assert_eq!(action.as_deref(), Some("zephyr send"));
    assert_eq!(
        cut,
        Some(cut_off),
        "default must cut where estimation-off cuts"
    );
    let wake = reports
        .iter()
        .rev()
        .find(|r| r.stage == "wake")
        .expect("a wake report");
    let confirm = reports.last().expect("a confirm report");
    assert!(matches!(confirm.stage, "confirm" | "fresh_confirm") && confirm.latched);
    assert_eq!(confirm.cut_sample, Some(cut_off));
    assert_eq!(wake.chosen, "emission");
    assert_eq!(wake.chosen_sample, Some(wake.emission_sample));
    assert!(wake.origin_sample.is_some() && wake.aligned_sample.is_some());
    eprintln!("wake report: {wake:#?}\nconfirm report: {confirm:#?}");

    for method in [
        KwsOnsetMethod::Posterior,
        KwsOnsetMethod::Aligned,
        KwsOnsetMethod::Acoustic,
    ] {
        let switches = KwsSwitches {
            onset: Some(method),
            ..KwsSwitches::default()
        };
        let (action, cut, reports) = latch_with(dir.path(), &audio, switches);
        assert_eq!(action.as_deref(), Some("zephyr send"), "{method:?}");
        let wake = reports.iter().rev().find(|r| r.stage == "wake").unwrap();
        let expected = match method {
            KwsOnsetMethod::Posterior => wake.posterior_sample,
            KwsOnsetMethod::Aligned => wake.aligned_sample,
            KwsOnsetMethod::Acoustic => wake.acoustic_sample,
            KwsOnsetMethod::Emission => unreachable!(),
        }
        .unwrap_or(wake.emission_sample);
        assert_eq!(cut, Some(expected), "{method:?}");
        assert_eq!(wake.chosen_sample, Some(expected));
        eprintln!(
            "{method:?}: cut {expected} (emission {}, {:+} ms)",
            wake.emission_sample,
            (expected as i64 - wake.emission_sample as i64) * 1_000 / SAMPLE_RATE as i64
        );
    }

    let shifted = KwsSwitches {
        onset_shift_ms: -100,
        ..KwsSwitches::default()
    };
    let (_, cut, _) = latch_with(dir.path(), &audio, shifted);
    assert_eq!(cut, Some(cut_off - SAMPLE_RATE as usize / 10));
}

/// `RIGHTKIT_KWS_STATIC_BATCH`, `RIGHTKIT_KWS_DECODER_CACHE` and `RIGHTKIT_KWS_SHARED_MODEL` all on.
const SPEED_LEVERS: KwsSwitches = KwsSwitches {
    static_batch: true,
    decoder_cache: true,
    shared_model: true,
    onset: Some(KwsOnsetMethod::Emission),
    onset_shift_ms: 0,
    threshold_tokens: rust_kws::ThresholdTokens::Upstream,
    joiner_batching: rust_kws::JoinerBatching::Beam,
    merge_timestamps: rust_kws::MergeTimestamps::First,
    portable_math: false,
    ttl_frames: Some(ACTIVE_KEYWORD_TTL_FRAMES),
};

/// The Rust spotter's model.
fn rust_model(spotter: &KwsSpotter) -> Arc<rust_kws::KwsModel> {
    match spotter {
        KwsSpotter::Rust(spotter) => Arc::clone(spotter.borrow().model()),
        KwsSpotter::Sherpa { .. } => panic!("not a Rust spotter"),
    }
}

/// The three speed levers change no cascade output: the production Rust
/// cascade with them on equals the one with them off on every snapshot, every
/// idle-wake arrival and every onset report (probabilities and candidates bit
/// for bit), on the dictation fixtures and on a keyword set that fires and
/// latches on them. With `shared_model` the wake, confirm and hey-wake
/// spotters run on one model.
#[test]
fn speed_levers_change_no_rust_cascade_output() {
    let Some(dir) = firing_model_dir("speed_levers_change_no_rust_cascade_output") else {
        return;
    };
    assert_eq!(
        SPEED_LEVERS,
        KwsSwitches {
            static_batch: true,
            decoder_cache: true,
            shared_model: true,
            ..KwsSwitches::default()
        }
    );
    let model_dir = test_model_dir().expect("checked by firing_model_dir");
    std::fs::copy(
        model_dir.join(HEY_WAKE_KEYWORDS_FILE),
        dir.path().join(HEY_WAKE_KEYWORDS_FILE),
    )
    .expect("copy hey-wake keywords");
    let mut divergences = Vec::new();
    let mut compared = 0usize;
    let mut fired = 0usize;
    for (label, dir, clips) in [
        ("bundled", model_dir.as_path(), ab_clips()),
        (
            "firing",
            dir.path(),
            NEGATIVE_FIXTURES
                .iter()
                .map(|name| (name.to_string(), fixture(name)))
                .collect(),
        ),
    ] {
        let mut base = SherpaKws::load_with_switches(dir, KwsBackend::Rust, KwsSwitches::default())
            .expect("load Rust KWS cascade");
        let mut fast = SherpaKws::load_with_switches(dir, KwsBackend::Rust, SPEED_LEVERS)
            .expect("load Rust KWS cascade with the speed levers");
        fast.start_idle_wake(IDLE_WAKE_THRESHOLD)
            .expect("start idle wake");
        base.start_idle_wake(IDLE_WAKE_THRESHOLD)
            .expect("start idle wake");
        let shared = rust_model(&fast.wake_spotter);
        assert!(Arc::ptr_eq(&shared, &rust_model(&fast.confirm_spotter)));
        assert!(Arc::ptr_eq(
            &shared,
            &rust_model(fast.hey_wake_spotter.as_ref().unwrap())
        ));
        assert!(!Arc::ptr_eq(
            &rust_model(&base.wake_spotter),
            &rust_model(&base.confirm_spotter)
        ));
        for (name, audio) in &clips {
            let (trace, _) = replay_trace(&mut base, audio, Drive::Persistent);
            fired += non_empty_outputs(&trace);
            base.take_onset_reports();
            eprintln!(
                "{label} {name}: {} non-empty outputs",
                non_empty_outputs(&trace)
            );
        }
        let (found, calls) = cascade_divergences(
            ("levers_off", &mut base),
            ("levers_on", &mut fast),
            &clips,
            true,
        );
        compared += calls;
        divergences.extend(found.into_iter().map(|d| format!("{label} {d}")));
    }
    eprintln!(
        "speed levers {SPEED_LEVERS:?}: {compared} calls compared, {fired} non-empty outputs, {} divergences",
        divergences.len()
    );
    assert!(
        fired > 0,
        "no clip fired a keyword; the comparison saw no detection"
    );
    assert!(
        divergences.is_empty(),
        "the speed levers changed Rust cascade output: {divergences:#?}"
    );
}

/// The Sherpa A/B with the three speed levers on: still identical on every
/// snapshot and idle-wake arrival.
#[test]
fn sherpa_and_rust_backends_match_per_snapshot_with_speed_levers() {
    let Some(model_dir) =
        sherpa_ab_inputs("sherpa_and_rust_backends_match_per_snapshot_with_speed_levers")
    else {
        return;
    };
    let tuning = RustKwsTuning {
        static_batch: true,
        decoder_cache: true,
        shared_model: true,
        ..SHERPA_REFERENCE_TUNING
    };
    let (divergences, compared) = ab_divergences(&model_dir, tuning);
    eprintln!(
        "A/B reference tuning with speed levers {tuning:?}: {compared} calls compared, {} divergences",
        divergences.len()
    );
    assert!(
        divergences.is_empty(),
        "Sherpa and Rust KWS backends diverged with the speed levers on: {divergences:#?}"
    );
}
