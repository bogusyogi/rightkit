#[cfg(any(target_os = "macos", target_os = "windows"))]
use rightkit_dictation_engine::asr::{AsrEp, AsrRuntime};
#[cfg(any(target_os = "macos", target_os = "windows"))]
use rightkit_dictation_engine::command_classify::{classify_streaming, CommandClassification};
#[cfg(any(target_os = "macos", target_os = "windows"))]
use serde::{Deserialize, Serialize};
#[cfg(any(target_os = "macos", target_os = "windows"))]
use std::path::{Path, PathBuf};
#[cfg(any(target_os = "macos", target_os = "windows"))]
use std::time::Instant;

#[cfg(any(target_os = "macos", target_os = "windows"))]
const SAMPLE_RATE: usize = 16_000;
#[cfg(any(target_os = "macos", target_os = "windows"))]
const MIN_PROBE_SAMPLES: usize = SAMPLE_RATE / 4;
#[cfg(any(target_os = "macos", target_os = "windows"))]
const MAX_COMMAND_SAMPLES: usize = SAMPLE_RATE * 2;

#[cfg(any(target_os = "macos", target_os = "windows"))]
#[derive(Deserialize)]
struct Manifest {
    rows: Vec<ManifestRow>,
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
#[derive(Deserialize)]
struct ManifestRow {
    clip: PathBuf,
    command_start: usize,
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
#[derive(Serialize)]
struct ResultRow {
    clip: PathBuf,
    command_start: usize,
    probes: usize,
    first_prefix_audio_ms: Option<f64>,
    fire_audio_ms: Option<f64>,
    final_probe_ms: Option<f64>,
    total_compute_ms: f64,
    transcript: String,
    classification: String,
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
#[derive(Serialize)]
struct Report {
    cadence_ms: usize,
    clips: usize,
    fired: usize,
    rows: Vec<ResultRow>,
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn read_wav(path: &Path) -> Result<Vec<f32>, String> {
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
fn main() -> Result<(), String> {
    let args = std::env::args().collect::<Vec<_>>();
    if args.len() != 5 {
        return Err(
            "usage: opening_command_cadence_replay <models-root> <manifest.json> <cadence-ms> <output.json>"
                .into(),
        );
    }
    let cadence_ms = args[3]
        .parse::<usize>()
        .map_err(|error| error.to_string())?;
    let cadence_samples = SAMPLE_RATE * cadence_ms / 1_000;
    if cadence_samples == 0 {
        return Err("cadence must produce at least one sample".into());
    }
    let manifest: Manifest =
        serde_json::from_slice(&std::fs::read(&args[2]).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;

    std::env::set_var("RIGHTKIT_ASR_BACKEND", "parakeet-tdt");
    let mut runtime = AsrRuntime::load(Path::new(&args[1]), AsrEp::resolve_default())?;
    let mut rows = Vec::with_capacity(manifest.rows.len());
    for row in manifest.rows {
        let audio = read_wav(&row.clip)?;
        let mut end = row
            .command_start
            .saturating_add(MIN_PROBE_SAMPLES)
            .min(audio.len());
        let max_end = row
            .command_start
            .saturating_add(MAX_COMMAND_SAMPLES)
            .min(audio.len());
        let mut probes = 0usize;
        let mut first_prefix_audio_ms = None;
        let mut fire_audio_ms = None;
        let mut final_probe_ms = None;
        let mut total_compute_ms = 0.0;
        let mut transcript = String::new();
        let mut classification = CommandClassification::None;
        while end <= max_end {
            let started = Instant::now();
            transcript = runtime
                .transcribe_result(&audio[row.command_start..end])?
                .text;
            let probe_ms = started.elapsed().as_secs_f64() * 1_000.0;
            total_compute_ms += probe_ms;
            probes += 1;
            classification = classify_streaming(&transcript, true);
            let audio_ms =
                (end.saturating_sub(row.command_start)) as f64 * 1_000.0 / SAMPLE_RATE as f64;
            if matches!(classification, CommandClassification::Prefix)
                && first_prefix_audio_ms.is_none()
            {
                first_prefix_audio_ms = Some(audio_ms);
            }
            if matches!(
                classification,
                CommandClassification::Complete(_) | CommandClassification::AmbiguousComplete(_)
            ) {
                fire_audio_ms = Some(audio_ms);
                final_probe_ms = Some(probe_ms);
                break;
            }
            if end == max_end {
                break;
            }
            end = end.saturating_add(cadence_samples).min(max_end);
        }
        rows.push(ResultRow {
            clip: row.clip,
            command_start: row.command_start,
            probes,
            first_prefix_audio_ms,
            fire_audio_ms,
            final_probe_ms,
            total_compute_ms,
            transcript,
            classification: format!("{classification:?}"),
        });
    }
    let report = Report {
        cadence_ms,
        clips: rows.len(),
        fired: rows
            .iter()
            .filter(|row| row.fire_audio_ms.is_some())
            .count(),
        rows,
    };
    let encoded = serde_json::to_vec_pretty(&report).map_err(|error| error.to_string())?;
    std::fs::write(&args[4], [encoded, b"\n".to_vec()].concat())
        .map_err(|error| error.to_string())?;
    println!(
        "cadence_ms={} clips={} fired={}",
        report.cadence_ms, report.clips, report.fired
    );
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn main() {
    eprintln!("opening_command_cadence_replay requires macOS or Windows (the ASR runtime)");
}
