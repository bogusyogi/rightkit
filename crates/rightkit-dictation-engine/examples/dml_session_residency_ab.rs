//! Diagnostic only: isolate DirectML session-residency cost without concurrent Run calls.
//!
//! Usage:
//!   dml_session_residency_ab <models_dir> <clip.wav>
//!     <single|dual_idle|dual_saturated|dual_saturated_drop|dual_saturated_rewarm>
//!     [probe_count]

#[cfg(not(target_os = "windows"))]
fn main() {}

#[cfg(target_os = "windows")]
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

#[cfg(target_os = "windows")]
fn timed_text(
    runtime: &mut rightkit_dictation_engine::asr::AsrRuntime,
    audio: &[f32],
) -> Result<(u128, String), String> {
    let started = std::time::Instant::now();
    let text = runtime.transcribe(audio)?;
    Ok((started.elapsed().as_millis(), text))
}

#[cfg(target_os = "windows")]
fn timed_probe(
    runtime: &mut rightkit_dictation_engine::asr::AsrRuntime,
    audio: &[f32],
) -> Result<u128, String> {
    let started = std::time::Instant::now();
    runtime.transcribe_result(audio)?;
    Ok(started.elapsed().as_millis())
}

#[cfg(target_os = "windows")]
fn percentile(values: &[u128], quantile: f64) -> u128 {
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    sorted[((sorted.len() - 1) as f64 * quantile).floor() as usize]
}

#[cfg(target_os = "windows")]
fn main() -> Result<(), String> {
    use rightkit_dictation_engine::asr::{AsrEp, AsrRuntime};
    use std::hash::{DefaultHasher, Hash, Hasher};
    use std::time::{Duration, Instant};

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        return Err("usage: dml_session_residency_ab <models_dir> <clip.wav> \
             <single|dual_idle|dual_saturated|dual_saturated_drop|dual_saturated_rewarm> \
             [probe_count]"
            .into());
    }
    let mode = args[3].as_str();
    if !matches!(
        mode,
        "single" | "dual_idle" | "dual_saturated" | "dual_saturated_drop" | "dual_saturated_rewarm"
    ) {
        return Err(format!("unsupported mode: {mode}"));
    }
    let probe_count = args
        .get(4)
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(180);
    if probe_count == 0 {
        return Err("probe_count must be positive".into());
    }

    let raw = load_wav(std::path::Path::new(&args[2]))?;
    if raw.len() < 3 * 16_000 {
        return Err("clip must contain at least three seconds".into());
    }
    let policy =
        std::env::var("RIGHTKIT_AUDIO_CONDITIONING_POLICY").unwrap_or_else(|_| "default".into());
    let full =
        rightkit_dictation_core::audio_conditioning::condition_for_asr(&raw, 16_000, &policy);
    let tail_raw = &raw[raw.len() - 3 * 16_000..];
    let tail =
        rightkit_dictation_core::audio_conditioning::condition_for_asr(tail_raw, 16_000, &policy);

    let models_dir = std::path::Path::new(&args[1]);
    let ep = AsrEp::resolve_default();
    let mut main_runtime = AsrRuntime::load(models_dir, ep)?;

    timed_probe(&mut main_runtime, &tail)?;
    timed_text(&mut main_runtime, &full)?;
    let (baseline_ms, baseline_text) = timed_text(&mut main_runtime, &full)?;

    let mut probe_runtime = if mode == "single" {
        None
    } else {
        let mut runtime = AsrRuntime::load(models_dir, ep)?;
        timed_probe(&mut runtime, &tail)?;
        Some(runtime)
    };

    let run_probes = mode != "dual_idle";
    let mut probes = Vec::with_capacity(if run_probes { probe_count } else { 0 });
    if run_probes {
        let period_started = Instant::now();
        for probe_index in 0..probe_count {
            let runtime = probe_runtime.as_mut().unwrap_or(&mut main_runtime);
            probes.push(timed_probe(runtime, &tail)?);
            let next_start = period_started + Duration::from_millis(250 * (probe_index + 1) as u64);
            if let Some(remaining) = next_start.checked_duration_since(Instant::now()) {
                std::thread::sleep(remaining);
            }
        }
    }
    let probe_runtime_dropped = mode == "dual_saturated_drop";
    if probe_runtime_dropped {
        drop(probe_runtime.take());
    }
    let main_rewarm_ms = if mode == "dual_saturated_rewarm" {
        Some(timed_probe(&mut main_runtime, &tail)?)
    } else {
        None
    };

    let (post_first_ms, post_first_text) = timed_text(&mut main_runtime, &full)?;
    let (post_second_ms, post_second_text) = timed_text(&mut main_runtime, &full)?;
    if baseline_text != post_first_text || baseline_text != post_second_text {
        return Err("transcript changed between baseline and post-probe final runs".into());
    }

    let mut hasher = DefaultHasher::new();
    baseline_text.hash(&mut hasher);
    let transcript_hash = hasher.finish();
    let probe_mean_ms = if probes.is_empty() {
        0.0
    } else {
        probes.iter().sum::<u128>() as f64 / probes.len() as f64
    };
    let probe_p50_ms = if probes.is_empty() {
        0
    } else {
        percentile(&probes, 0.50)
    };
    let probe_p95_ms = if probes.is_empty() {
        0
    } else {
        percentile(&probes, 0.95)
    };

    println!(
        "{{\"mode\":\"{mode}\",\"audio_samples\":{},\"audio_secs\":{:.3},\
         \"probe_count\":{},\"probe_mean_ms\":{probe_mean_ms:.1},\
         \"probe_p50_ms\":{probe_p50_ms},\"probe_p95_ms\":{probe_p95_ms},\
         \"probe_runtime_dropped\":{probe_runtime_dropped},\
         \"main_rewarm_ms\":{},\
         \"baseline_ms\":{baseline_ms},\"post_first_ms\":{post_first_ms},\
         \"post_second_ms\":{post_second_ms},\"transcript_hash\":\"{transcript_hash:016x}\"}}",
        full.len(),
        full.len() as f64 / 16_000.0,
        probes.len(),
        main_rewarm_ms
            .map(|value| value.to_string())
            .unwrap_or_else(|| "null".into()),
    );
    Ok(())
}
