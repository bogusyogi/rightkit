//! Incremental replay of the production Sherpa wake-confirm primitive over a
//! corpus. Wake scans stay at 250 ms; only an observed Zephyr candidate switches
//! to the configurable confirm cadence. Prints one JSONL row per clip with its
//! first action and detection time. This is KWS-only cadence evidence, not the
//! full worker-runtime replay gate.
//!
//! Usage: cascade_corpus_replay <model-dir> <row_id> <wav> [<row_id> <wav> ...]

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn main() -> Result<(), String> {
    use std::path::PathBuf;

    use rightkit_text_polish::text_pipeline::parse_control_command;

    const SAMPLE_RATE: usize = 16_000;
    // Offline audio advances in 50 ms blocks after wake; production starts each
    // probe after 40 ms wall time once at least 50 ms fresh audio exists.
    // RIGHTKIT_CONFIRM_STEP_MS=200 reproduces the old effective armed cadence.
    let confirm_step_ms: usize = std::env::var("RIGHTKIT_CONFIRM_STEP_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(
            rightkit_dictation_engine::sherpa_kws::CONFIRM_STEP_SAMPLES * 1000 / SAMPLE_RATE,
        );
    let confirm_step = (SAMPLE_RATE * confirm_step_ms / 1000).max(1);
    let wake_step_ms: usize = std::env::var("RIGHTKIT_WAKE_STEP_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(250);
    let wake_step = (SAMPLE_RATE * wake_step_ms / 1000).max(1);

    let usage = "usage: cascade_corpus_replay <model-dir> <row_id> <wav> [<row_id> <wav> ...]";
    let mut args = std::env::args_os().skip(1);
    let model = args.next().map(PathBuf::from).ok_or(usage)?;
    let rest: Vec<PathBuf> = args.map(PathBuf::from).collect();
    if rest.is_empty() || !rest.len().is_multiple_of(2) {
        return Err(usage.into());
    }

    #[cfg(target_os = "windows")]
    let _ort = unsafe {
        libloading::Library::new(
            model
                .parent()
                .ok_or("model directory has no resource parent")?
                .join("runtime/onnxruntime.dll"),
        )
    }
    .map_err(|e| format!("preload bundled ONNX Runtime: {e}"))?;

    let mut kws = rightkit_dictation_engine::sherpa_kws::SherpaKws::load(&model)
        .map_err(|e| format!("load model: {e}"))?;
    let total = rest.len() / 2;
    for (index, pair) in rest.chunks(2).enumerate() {
        let row_id = pair[0].to_string_lossy().to_string();
        let wav = &pair[1];
        let outcome = replay_clip(&mut kws, wav, wake_step, confirm_step);
        match outcome {
            Ok((detections, detected_at_ms)) => {
                let dets = detections
                    .iter()
                    .map(|d| format!("{d:?}"))
                    .collect::<Vec<_>>()
                    .join(",");
                let detected_at = detected_at_ms.map_or("null".into(), |ms| ms.to_string());
                println!("{{\"row_id\":\"{row_id}\",\"detections\":[{dets}],\"detected_at_ms\":{detected_at},\"error\":null}}");
            }
            Err(e) => {
                let e = e.replace('"', "'");
                println!("{{\"row_id\":\"{row_id}\",\"detections\":[],\"detected_at_ms\":null,\"error\":\"{e}\"}}");
            }
        }
        if (index + 1) % 10 == 0 || index + 1 == total {
            eprintln!("cadence replay progress: checked={}/{total}", index + 1);
        }
    }
    let _ = parse_control_command; // linked via replay_clip
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn replay_clip(
    kws: &mut rightkit_dictation_engine::sherpa_kws::SherpaKws,
    wav: &std::path::Path,
    wake_step: usize,
    confirm_step: usize,
) -> Result<(Vec<String>, Option<usize>), String> {
    let sample_rate = 16_000usize;
    kws.reset_stream()?; // fresh cascade + cleared latch per clip
    use rightkit_text_polish::text_pipeline::parse_control_command;

    let wav_audio = rightkit_audio::wav::read_wav_mono_f32(wav).map_err(|e| e.to_string())?;
    if wav_audio.sample_rate as usize != sample_rate {
        return Err(format!(
            "{}: expected 16k, got {}Hz",
            wav.display(),
            wav_audio.sample_rate
        ));
    }
    let mut audio: Vec<f32> = wav_audio.samples;
    if audio.is_empty() {
        return Err("empty wav".into());
    }
    // Production keeps accepting PCM while candidate confirmation is armed.
    // Add its exact bounded tail so a verb ending just before EOF still gets
    // one final fresh-audio probe instead of becoming a harness-only miss.
    audio.resize(
        audio.len() + rightkit_dictation_engine::sherpa_kws::CONFIRM_TIMEOUT_SAMPLES,
        0.0,
    );

    // Bounded rolling tail window, exactly as the worker (TAIL_WINDOW_SAMPLES=3s):
    // snapshot_start advances so the cascade always scores recent audio with an
    // absolute sample cursor, instead of an ever-growing buffer whose old wake
    // candidates fall out of the trailing window and expire.
    const TAIL_WINDOW_SAMPLES: usize = 16_000 * 3;
    let debug = std::env::var("RIGHTKIT_DEBUG").is_ok();
    let mut detections: Vec<String> = Vec::new();
    let mut detected_at_ms = None;
    let mut armed = false;
    let mut end = 0usize;
    while end < audio.len() {
        let step = if armed { confirm_step } else { wake_step };
        end = (end + step).min(audio.len());
        let start = end.saturating_sub(TAIL_WINDOW_SAMPLES);
        let result = kws.transcribe_cascade_result(&audio[start..end], start, end, armed)?;
        if debug && !result.text.is_empty() {
            eprintln!(
                "  @{:>7}ms armed={armed} text={:?}",
                end * 1000 / sample_rate,
                result.text
            );
        }
        if result.text == "zephyr" {
            armed = true;
            continue;
        }
        if result.text.is_empty() {
            armed = false;
            continue;
        }
        if let Some(cmd) = parse_control_command(&result.text) {
            eprintln!(
                "FIRED {:?} at {}ms (text={:?})",
                cmd.intent,
                end * 1000 / sample_rate,
                result.text
            );
            detections.push(format!("ZEPHYR_{:?}", cmd.intent).to_uppercase());
            detected_at_ms = Some(end * 1000 / sample_rate);
            break; // one-action latch, as in the worker
        }
    }
    Ok((detections, detected_at_ms))
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn main() {
    panic!("macOS and Windows only");
}
