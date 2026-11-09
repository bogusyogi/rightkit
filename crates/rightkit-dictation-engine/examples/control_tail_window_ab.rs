//! Compare rolling control-tail window lengths through the shipped ASR runtime.
//!
//! Usage: control_tail_window_ab <models_dir> <wav> [<wav>...]

#[cfg(any(target_os = "macos", target_os = "windows"))]
use std::path::Path;
#[cfg(any(target_os = "macos", target_os = "windows"))]
use std::time::Instant;

#[cfg(any(target_os = "macos", target_os = "windows"))]
use rightkit_dictation_engine::asr::{AsrEp, AsrRuntime};

#[cfg(any(target_os = "macos", target_os = "windows"))]
const SAMPLE_RATE: usize = 16_000;

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn load_wav(path: &Path) -> Result<Vec<f32>, String> {
    let wav = rightkit_audio::wav::read_wav_mono_f32(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if wav.sample_rate != SAMPLE_RATE as u32 {
        return Err(format!(
            "{}: expected {} Hz WAV, got {} Hz",
            path.display(),
            SAMPLE_RATE as u32,
            wav.sample_rate
        ));
    }
    Ok(wav.samples)
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn transcribe_window(
    runtime: &mut AsrRuntime,
    audio: &[f32],
    window_ms: usize,
) -> Result<(String, u128), String> {
    let window_samples = window_ms * SAMPLE_RATE / 1_000;
    let start = audio.len().saturating_sub(window_samples);
    let conditioned = rightkit_dictation_core::audio_conditioning::condition_for_asr(
        &audio[start..],
        SAMPLE_RATE as u32,
        "default",
    );
    let started = Instant::now();
    let text = runtime.transcribe(&conditioned)?;
    Ok((text, started.elapsed().as_millis()))
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        return Err("usage: control_tail_window_ab <models_dir> <wav> [<wav>...]".into());
    }
    let mut runtime = AsrRuntime::load(Path::new(&args[0]), AsrEp::resolve_default())?;
    let _ = runtime.transcribe(&vec![0.0; SAMPLE_RATE]);

    for wav in &args[1..] {
        let audio = load_wav(Path::new(wav))?;
        let (short, short_ms) = transcribe_window(&mut runtime, &audio, 1_750)?;
        let (full, full_ms) = transcribe_window(&mut runtime, &audio, 3_000)?;
        println!(
            "{}\n  1750ms {:>4}ms: {:?}\n  3000ms {:>4}ms: {:?}",
            wav, short_ms, short, full_ms, full
        );
    }
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn main() {
    eprintln!("control_tail_window_ab requires macOS or Windows (the ASR runtime)");
}
