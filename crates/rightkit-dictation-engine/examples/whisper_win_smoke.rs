// Lane D smoke: exercise `WhisperWin::load` + `transcribe_lang` through the
// real engine code path (not just the raw CLI), since `cargo test` is
// currently blocked crate-wide by a pre-existing, out-of-scope macOS-only
// test-helper gate (see rightkit-dictation-text-pipeline/src/l3_cleanup.rs's test
// helpers). Windows only.
//
// Usage: cargo run --example whisper_win_smoke -- <models_dir> <wav_path> <lang>
#[cfg(target_os = "windows")]
fn main() {
    let mut args = std::env::args().skip(1);
    let models_dir = args.next().expect("usage: <models_dir> <wav_path> <lang>");
    let wav_path = args.next().expect("usage: <models_dir> <wav_path> <lang>");
    let lang = args.next().unwrap_or_else(|| "en".to_string());

    let engine =
        rightkit_dictation_engine::whisper_win::WhisperWin::load(std::path::Path::new(&models_dir))
            .expect("load WhisperWin engine");

    let wav =
        rightkit_audio::wav::read_wav_mono_f32(std::path::Path::new(&wav_path)).expect("open wav");
    println!("wav rate: {} Hz", wav.sample_rate);
    let samples: Vec<f32> = wav.samples;

    let started = std::time::Instant::now();
    let mut locked_lang = None;
    let (text, resolved) = engine
        .transcribe_lang_locked(&samples, &lang, locked_lang)
        .expect("transcribe AUTO");
    locked_lang = resolved;
    println!("latency_ms_first={}", started.elapsed().as_millis());
    println!("text_first={text:?} resolved_lang_id={locked_lang:?}");

    let started = std::time::Instant::now();
    let (locked_text, second_resolved) = engine
        .transcribe_lang_locked(&samples, &lang, locked_lang)
        .expect("transcribe locked second window");
    println!("latency_ms_second={}", started.elapsed().as_millis());
    println!("text_second={locked_text:?} resolved_lang_id_second={second_resolved:?}");
}

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("whisper_win_smoke is Windows-only");
}
