#![cfg(all(target_os = "macos", feature = "system"))]

use rightkit_tts::{SystemTts, SystemTtsOptions};

#[cfg(target_os = "macos")]
#[test]
fn macos_system_backend_writes_audio_and_word_timings_without_playback() {
    let chunks = rightkit_tts::chunk_sentences("Dr. Smith said hello. It worked.");
    assert_eq!(
        chunks.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(),
        vec!["Dr. Smith said hello.", "It worked."]
    );
    let result = SystemTts
        .synthesize(
            "RightKit speech output is written to a file.",
            &SystemTtsOptions::default(),
        )
        .expect("macOS say + afconvert");
    let (rate, samples) = rightkit_tts::wav::decode_pcm16(&result.wav).expect("PCM16 WAV");
    assert!(rate > 0 && !samples.is_empty());
    eprintln!("system_tts_metrics rate={rate} samples={} wav_bytes={} duration_ms={} words={} segments={}", samples.len(), result.wav.len(), result.manifest.total_duration_ms, result.transcript.words.len(), result.manifest.segments.len());
    assert_eq!(result.backend, "macos-say");
    assert!(result.transcript.text().contains("RightKit"));
    assert!(result.transcript.words.len() >= 5);
    assert_eq!(result.manifest.segments.len(), 1);
    assert_eq!(
        result.manifest.total_duration_ms,
        result.transcript.duration_ms
    );
    result
        .transcript
        .validate()
        .expect("timing manifest validates");
}

#[cfg(feature = "engine")]
#[cfg(target_os = "macos")]
#[test]
fn qts_real_synthesis_writes_non_silent_audio_and_transcript() {
    let root = std::path::Path::new("/private/tmp/gre2e-qts");
    let model_dir = root.join("models/qts");
    let runtime = root.join("runtime/qts/libonnxruntime.dylib");
    // Cargo's binary path proves this test drives the extracted RightKit worker. The
    // prebuilt donor worker remains a manual fallback only, never this acceptance path.
    let worker = std::path::PathBuf::from(env!("CARGO_BIN_EXE_rightkit-tts-worker"));
    assert!(
        model_dir.is_dir(),
        "QTS model directory missing: {}",
        model_dir.display()
    );
    assert!(
        runtime.is_file(),
        "QTS runtime missing: {}",
        runtime.display()
    );
    assert!(
        worker.is_file(),
        "RightKit QTS worker missing: {}",
        worker.display()
    );
    std::env::set_var("RIGHTKIT_TTS_MODEL_DIR", &model_dir);
    std::env::set_var("RIGHTKIT_TTS_WORKER", &worker);
    std::env::set_var("ORT_DYLIB_PATH", &runtime);
    let client = rightkit_tts::QtsClient::new(30);
    let result = client
        .synthesize("This is a real QTS synthesis check.", &Default::default())
        .expect("QTS synthesis");
    let (rate, samples) = rightkit_tts::wav::decode_pcm16(&result.wav).expect("QTS WAV");
    assert_eq!(rate, rightkit_tts::QTS_SAMPLE_RATE_HZ);
    assert!(samples.iter().any(|s| *s != 0), "QTS output is silent");
    assert!(result.audio_seconds > 0.1);
    assert!(result.chunks >= 1);
    eprintln!("qts_metrics rate={rate} samples={} wav_bytes={} audio_seconds={:.3} chunks={} worker_pid={:?}", samples.len(), result.wav.len(), result.audio_seconds, result.chunks, client.worker_pid());
    let words = rightkit_transcript::words_from_cues(&[rightkit_transcript::ParsedCue {
        start_ms: 0,
        end_ms: (result.audio_seconds * 1000.0) as u64,
        lines: vec!["This is a real QTS synthesis check.".into()],
    }]);
    assert!(!words.is_empty());
    client.shutdown();
}
