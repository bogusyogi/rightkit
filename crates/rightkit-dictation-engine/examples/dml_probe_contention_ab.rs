//! Cross-platform diagnostic: measure final-decode contention from one concurrent
//! command/trigger probe on an independent ASR runtime.
//!
//! Usage: dml_probe_contention_ab <models_dir> <clip.wav> [iterations] [target_secs] [probe_ms]

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn main() {}

#[cfg(any(target_os = "windows", target_os = "macos"))]
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

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn fit_duration(source: &[f32], samples: usize) -> Vec<f32> {
    source.iter().copied().cycle().take(samples).collect()
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn timed(
    runtime: &mut rightkit_dictation_engine::asr::AsrRuntime,
    audio: &[f32],
) -> Result<u128, String> {
    let policy =
        std::env::var("RIGHTKIT_AUDIO_CONDITIONING_POLICY").unwrap_or_else(|_| "default".into());
    let conditioned =
        rightkit_dictation_core::audio_conditioning::condition_for_asr(audio, 16_000, &policy);
    let started = std::time::Instant::now();
    runtime.transcribe(&conditioned)?;
    Ok(started.elapsed().as_millis())
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn main() -> Result<(), String> {
    use rightkit_dictation_engine::asr::{AsrEp, AsrRuntime};
    use std::sync::{Arc, Barrier};

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        return Err(
            "usage: dml_probe_contention_ab <models_dir> <clip.wav> [iterations] [target_secs] [probe_ms]"
                .into(),
        );
    }
    let iterations = args
        .get(3)
        .and_then(|value| value.parse().ok())
        .unwrap_or(5usize);
    let target_secs = args
        .get(4)
        .and_then(|value| value.parse().ok())
        .unwrap_or(15usize);
    let probe_ms = args
        .get(5)
        .and_then(|value| value.parse().ok())
        .unwrap_or(3_000usize);
    let source = load_wav(std::path::Path::new(&args[2]))?;
    if source.is_empty() || iterations == 0 || target_secs == 0 || probe_ms == 0 {
        return Err(
            "clip, iteration count, target seconds, and probe milliseconds must be non-empty"
                .into(),
        );
    }
    let final_audio = fit_duration(&source, target_secs * 16_000);
    let probe_samples = (probe_ms * 16_000 / 1_000).min(final_audio.len());
    let probe_audio = final_audio[final_audio.len() - probe_samples..].to_vec();
    let models_dir = std::path::Path::new(&args[1]);
    let ep = AsrEp::resolve_default();
    let mut main_runtime = AsrRuntime::load(models_dir, ep)?;
    let mut probe_runtime = AsrRuntime::load(models_dir, ep)?;

    timed(&mut main_runtime, &final_audio)?;
    timed(&mut probe_runtime, &probe_audio)?;

    let mut baseline = Vec::with_capacity(iterations);
    let mut contended = Vec::with_capacity(iterations);
    let mut probes = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        baseline.push(timed(&mut main_runtime, &final_audio)?);
        let gate = Arc::new(Barrier::new(2));
        let probe_gate = Arc::clone(&gate);
        let (final_ms, probe_ms) = std::thread::scope(|scope| {
            let probe = scope.spawn(|| {
                probe_gate.wait();
                timed(&mut probe_runtime, &probe_audio)
            });
            gate.wait();
            let final_ms = timed(&mut main_runtime, &final_audio);
            let probe_ms = probe
                .join()
                .map_err(|_| "probe thread panicked".to_string())?;
            Ok::<_, String>((final_ms?, probe_ms?))
        })?;
        contended.push(final_ms);
        probes.push(probe_ms);
    }

    let avg = |values: &[u128]| values.iter().sum::<u128>() as f64 / values.len() as f64;
    let baseline_avg = avg(&baseline);
    let contended_avg = avg(&contended);
    println!(
        "probe contention: target={target_secs}s probe={probe_ms}ms iterations={iterations} baseline_avg={baseline_avg:.1}ms \
         contended_final_avg={contended_avg:.1}ms change={:+.1}% probe_avg={:.1}ms \
         baseline={baseline:?} contended={contended:?} probes={probes:?}",
        (contended_avg / baseline_avg - 1.0) * 100.0,
        avg(&probes),
    );
    Ok(())
}
