//! Diagnostic only: measure whether sustained three-second command probes slow
//! the immediately following full transcription.
//!
//! Usage:
//!   probe_contention_ab <model_dir> <clip.wav> [target_secs] [probe_count] [interval_ms]

#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
fn load_wav(path: &std::path::Path) -> Result<Vec<f32>, String> {
    let wav = rightkit_audio::wav::read_wav_mono_f32(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if wav.sample_rate != 16_000_u32 {
        return Err(format!(
            "{}: expected {} Hz WAV, got {} Hz",
            path.display(),
            16_000,
            wav.sample_rate
        ));
    }
    Ok(wav.samples)
}

#[cfg(target_os = "macos")]
fn fit_duration(source: &[f32], target_samples: usize) -> Vec<f32> {
    source
        .iter()
        .copied()
        .cycle()
        .take(target_samples)
        .collect()
}

#[cfg(target_os = "macos")]
fn timed_transcribe(
    runtime: &mut rightkit_dictation_engine::asr::AsrRuntime,
    audio: &[f32],
) -> Result<u128, String> {
    let started = std::time::Instant::now();
    let conditioned =
        rightkit_dictation_core::audio_conditioning::condition_for_asr(audio, 16_000, "default");
    runtime.transcribe(&conditioned)?;
    Ok(started.elapsed().as_millis())
}

#[cfg(target_os = "macos")]
fn main() -> Result<(), String> {
    use std::time::{Duration, Instant};

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        return Err("usage: probe_contention_ab <model_dir> <clip.wav> [target_secs] [probe_count] [interval_ms]".into());
    }
    let target_secs = args.get(3).and_then(|v| v.parse().ok()).unwrap_or(60usize);
    let probe_count = args.get(4).and_then(|v| v.parse().ok()).unwrap_or(100usize);
    let interval_ms = args.get(5).and_then(|v| v.parse().ok()).unwrap_or(300u64);
    if target_secs == 0 || probe_count == 0 {
        return Err("target_secs and probe_count must be positive".into());
    }

    let source = load_wav(std::path::Path::new(&args[2]))?;
    if source.is_empty() {
        return Err("input WAV is empty".into());
    }
    let audio = fit_duration(&source, target_secs * 16_000);
    let tail = &audio[audio.len().saturating_sub(3 * 16_000)..];
    let model = rightkit_dictation_engine::coreml_asr::CoreMlParakeet::load(std::path::Path::new(
        &args[1],
    ))?;
    let mut runtime = rightkit_dictation_engine::asr::AsrRuntime::CoreMlParakeet(model);

    // Compile/warm both the short-probe and full-transcription paths.
    timed_transcribe(&mut runtime, tail)?;
    timed_transcribe(&mut runtime, &audio)?;
    std::thread::sleep(Duration::from_secs(2));

    let baseline_ms = timed_transcribe(&mut runtime, &audio)?;
    let probe_period_started = Instant::now();
    let mut probe_total_ms = 0u128;
    for probe_index in 0..probe_count {
        probe_total_ms += timed_transcribe(&mut runtime, tail)?;
        let next_start = probe_period_started
            + Duration::from_millis(interval_ms.saturating_mul((probe_index + 1) as u64));
        if let Some(remaining) = next_start.checked_duration_since(Instant::now()) {
            std::thread::sleep(remaining);
        }
    }
    let loaded_ms = timed_transcribe(&mut runtime, &audio)?;
    std::thread::sleep(Duration::from_secs(2));
    let recovered_ms = timed_transcribe(&mut runtime, &audio)?;

    let change_pct = (loaded_ms as f64 / baseline_ms as f64 - 1.0) * 100.0;
    println!(
        "AB target={}s probes={} cadence={}ms probe_avg={:.1}ms baseline={}ms loaded={}ms change={:+.1}% recovered={}ms",
        target_secs,
        probe_count,
        interval_ms,
        probe_total_ms as f64 / probe_count as f64,
        baseline_ms,
        loaded_ms,
        change_pct,
        recovered_ms,
    );
    Ok(())
}
