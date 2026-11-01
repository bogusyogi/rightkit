//! Compare lossless Silero framing with the former per-callback remainder loss.
//!
//! Usage: vad_chunking_ab <silero.onnx> <wav_dir> [limit]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rightkit_dictation_engine::vad::SpeechVad;

const FRAME: usize = 512;
const CALLBACK_PATTERN: [usize; 2] = [3_840, 4_160];

fn wait_ready(vad: &mut SpeechVad) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let _ = vad.observe(&vec![0.0; FRAME]);
        if vad.backend_name().is_some() {
            vad.reset();
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Err("Silero did not become ready".into())
}

fn load_wav(path: &Path) -> Result<Vec<f32>, String> {
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

fn first_detection(
    vad: &mut SpeechVad,
    audio: &[f32],
    discard_callback_remainders: bool,
) -> Option<usize> {
    vad.reset();
    let mut offset = 0;
    let mut callback = 0;
    while offset < audio.len() {
        let take = CALLBACK_PATTERN[callback % CALLBACK_PATTERN.len()].min(audio.len() - offset);
        let chunk = &audio[offset..offset + take];
        let observed = if discard_callback_remainders {
            vad.observe(&chunk[..chunk.len() / FRAME * FRAME])
        } else {
            vad.observe(chunk)
        };
        offset += take;
        callback += 1;
        if observed {
            return Some(offset);
        }
    }
    None
}

fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|sample| sample * sample).sum::<f32>() / samples.len() as f32).sqrt()
}

fn first_detection_with_legacy_energy_gate(vad: &mut SpeechVad, audio: &[f32]) -> Option<usize> {
    vad.reset();
    let (mut offset, mut callback) = (0, 0);
    let (mut raw_ema, mut peak_raw) = (0.0f32, 0.0f32);
    while offset < audio.len() {
        let take = CALLBACK_PATTERN[callback % CALLBACK_PATTERN.len()].min(audio.len() - offset);
        let chunk = &audio[offset..offset + take];
        raw_ema = raw_ema * 0.6 + rms(chunk) * 0.4;
        peak_raw = peak_raw.max(raw_ema);
        let speech_floor = (peak_raw * 0.25).max(0.002);
        let observed = raw_ema > speech_floor && vad.observe(chunk);
        offset += take;
        callback += 1;
        if observed {
            return Some(offset);
        }
    }
    None
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        return Err("usage: vad_chunking_ab <silero.onnx> <wav_dir> [limit]".into());
    }
    let limit = args
        .get(2)
        .and_then(|value| value.parse().ok())
        .unwrap_or(usize::MAX);
    let mut wavs: Vec<PathBuf> = std::fs::read_dir(&args[1])
        .map_err(|error| error.to_string())?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("wav"))
        })
        .collect();
    wavs.sort();
    wavs.truncate(limit);

    let model = Some(PathBuf::from(&args[0]));
    let mut fixed = SpeechVad::with_model_path(model.clone());
    let mut legacy = SpeechVad::with_model_path(model.clone());
    let mut gated = SpeechVad::with_model_path(model);
    wait_ready(&mut fixed)?;
    wait_ready(&mut legacy)?;
    wait_ready(&mut gated)?;

    let (mut both, mut fixed_only, mut legacy_only, mut neither) = (0, 0, 0, 0);
    let mut earlier_samples: i64 = 0;
    let mut compared = 0;
    let (mut gated_detected, mut fixed_detected_gated_missed) = (0, 0);
    let mut audio_samples = 0usize;
    let started = Instant::now();
    for path in wavs {
        let audio = load_wav(&path)?;
        audio_samples += audio.len();
        let fixed_at = first_detection(&mut fixed, &audio, false);
        let legacy_at = first_detection(&mut legacy, &audio, true);
        let gated_at = first_detection_with_legacy_energy_gate(&mut gated, &audio);
        if gated_at.is_some() {
            gated_detected += 1;
        } else if fixed_at.is_some() {
            fixed_detected_gated_missed += 1;
            println!("legacy-gate-miss: {}", path.display());
        }
        match (fixed_at, legacy_at) {
            (Some(a), Some(b)) => {
                both += 1;
                earlier_samples += b as i64 - a as i64;
                compared += 1;
            }
            (Some(_), None) => {
                fixed_only += 1;
                println!("fixed-only: {}", path.display());
            }
            (None, Some(_)) => legacy_only += 1,
            (None, None) => neither += 1,
        }
    }
    let elapsed = started.elapsed();
    let audio_seconds = audio_samples as f64 / 16_000.0;
    println!(
        "VAD chunking A/B: both={both} fixed_only={fixed_only} legacy_only={legacy_only} neither={neither} gated_detected={gated_detected} fixed_detected_gated_missed={fixed_detected_gated_missed} mean_fixed_lead_ms={:.1} audio_s={audio_seconds:.1} process_ms={} realtime_x={:.1}",
        if compared == 0 { 0.0 } else { earlier_samples as f64 * 1_000.0 / 16_000.0 / compared as f64 },
        elapsed.as_millis(),
        audio_seconds / elapsed.as_secs_f64(),
    );
    Ok(())
}
