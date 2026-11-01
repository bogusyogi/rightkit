#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("coreml_static15_ab is macOS-only");
}

#[cfg(target_os = "macos")]
mod mac {
    use rightkit_dictation_engine::asr::{AsrEp, AsrRuntime};
    use serde::Deserialize;
    use serde_json::json;
    use std::fs::File;
    use std::io::{BufWriter, Write};
    use std::path::{Path, PathBuf};
    use std::time::Instant;

    const SAMPLE_RATE: usize = 16_000;
    const WINDOW: usize = 15 * SAMPLE_RATE;
    const PADDING: usize = 2_240 * SAMPLE_RATE / 1_000;
    const QUIET_SPAN: usize = 200 * SAMPLE_RATE / 1_000;
    const QUIET_HOP: usize = 100 * SAMPLE_RATE / 1_000;
    const OVERLAP_CHARS: usize = 16;

    #[derive(Deserialize)]
    struct ManifestRow {
        id: String,
        duration_s: f64,
    }

    struct Decode {
        text: String,
        total_compute_s: f64,
        stop_tail_s: f64,
        windows: usize,
        tail_audio_s: f64,
    }

    pub fn run() -> Result<(), String> {
        let args: Vec<String> = std::env::args().collect();
        if args.len() < 5 {
            return Err(
                "usage: coreml_static15_ab <models-root> <manifest> <audio-dir> <out-root> [--scheduled-only] [--opening-probe]".into(),
            );
        }
        let models_root = PathBuf::from(&args[1]);
        let manifest_path = PathBuf::from(&args[2]);
        let audio_dir = PathBuf::from(&args[3]);
        let out_root = PathBuf::from(&args[4]);
        let scheduled_only = args.iter().any(|arg| arg == "--scheduled-only");
        let opening_probe = args.iter().any(|arg| arg == "--opening-probe");
        if args[5..]
            .iter()
            .any(|arg| arg != "--scheduled-only" && arg != "--opening-probe")
        {
            return Err("unsupported coreml_static15_ab option".into());
        }
        let rows: Vec<ManifestRow> = serde_json::from_str(
            &std::fs::read_to_string(&manifest_path).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        std::env::set_var("RIGHTKIT_ASR_BACKEND", "parakeet-tdt");

        for scheduled in [false, true]
            .into_iter()
            .filter(|scheduled| !scheduled_only || *scheduled)
        {
            let arm = if scheduled { "scheduled" } else { "deferred" };
            let arm_dir = out_root.join(arm);
            std::fs::create_dir_all(&arm_dir).map_err(|error| error.to_string())?;
            let mut writer = BufWriter::new(
                File::create(arm_dir.join("hypotheses.jsonl"))
                    .map_err(|error| error.to_string())?,
            );
            let mut model = AsrRuntime::load(&models_root, AsrEp::resolve_default())?;
            let first_audio = load_wav(&audio_dir.join(format!("{}.wav", rows[0].id)))?;
            let _ = model.transcribe_result(&first_audio)?;

            for (index, row) in rows.iter().enumerate() {
                let audio = load_wav(&audio_dir.join(format!("{}.wav", row.id)))?;
                let opening_decode_ms = if opening_probe {
                    let opening_end = audio.len().min(2 * SAMPLE_RATE);
                    let started = Instant::now();
                    let _ = model.transcribe_result(&audio[..opening_end])?;
                    Some(started.elapsed().as_secs_f64() * 1_000.0)
                } else {
                    None
                };
                let decoded = if scheduled {
                    decode_scheduled(&mut model, &audio)?
                } else {
                    let started = Instant::now();
                    let text = model.transcribe_result(&audio)?.text;
                    let elapsed = started.elapsed().as_secs_f64();
                    Decode {
                        text,
                        total_compute_s: elapsed,
                        stop_tail_s: elapsed,
                        windows: 0,
                        tail_audio_s: audio.len() as f64 / SAMPLE_RATE as f64,
                    }
                };
                writeln!(
                    writer,
                    "{}",
                    json!({
                        "id": row.id,
                        "hypothesis": decoded.text,
                        "status": "ok",
                        "duration_s": row.duration_s,
                        "decode_s": decoded.total_compute_s,
                        "simulated_stop_tail_ms": decoded.stop_tail_s * 1_000.0,
                        "total_compute_ms": decoded.total_compute_s * 1_000.0,
                        "scheduled_background_windows": decoded.windows,
                        "tail_audio_s": decoded.tail_audio_s,
                        "opening_decode_ms": opening_decode_ms,
                    })
                )
                .map_err(|error| error.to_string())?;
                writer.flush().map_err(|error| error.to_string())?;
                println!(
                    "{arm} {}/{} {} compute={:.1}ms stop={:.1}ms",
                    index + 1,
                    rows.len(),
                    row.id,
                    decoded.total_compute_s * 1_000.0,
                    decoded.stop_tail_s * 1_000.0
                );
            }
        }
        Ok(())
    }

    fn decode_scheduled(model: &mut AsrRuntime, audio: &[f32]) -> Result<Decode, String> {
        let duration_s = audio.len() as f64 / SAMPLE_RATE as f64;
        let mut text = String::new();
        let mut start = 0usize;
        let mut schedule_finish_s = 0.0f64;
        let mut total_compute_s = 0.0f64;
        let mut windows = 0usize;

        while start + WINDOW < audio.len() {
            let window_end = start + WINDOW;
            let target_end = window_end - PADDING;
            let cut = quiet_cut(audio, target_end, window_end).unwrap_or(window_end);
            let started = Instant::now();
            let result = model.transcribe_result(&audio[start..cut])?;
            let elapsed = started.elapsed().as_secs_f64();
            append_with_overlap(&mut text, &result.text);
            schedule_finish_s =
                schedule_finish_s.max(window_end as f64 / SAMPLE_RATE as f64) + elapsed;
            total_compute_s += elapsed;
            windows += 1;
            start = cut.max(start + 1);
        }

        let tail_audio_s = (audio.len() - start) as f64 / SAMPLE_RATE as f64;
        let started = Instant::now();
        let result = model.transcribe_result(&audio[start..])?;
        let elapsed = started.elapsed().as_secs_f64();
        append_with_overlap(&mut text, &result.text);
        schedule_finish_s = schedule_finish_s.max(duration_s) + elapsed;
        total_compute_s += elapsed;
        Ok(Decode {
            text: text.trim().to_string(),
            total_compute_s,
            stop_tail_s: (schedule_finish_s - duration_s).max(0.0),
            windows,
            tail_audio_s,
        })
    }

    fn quiet_cut(audio: &[f32], lo: usize, hi: usize) -> Option<usize> {
        if hi <= lo || hi - lo < QUIET_SPAN {
            return None;
        }
        let mut best: Option<(usize, f32)> = None;
        let mut start = lo;
        while start + QUIET_SPAN <= hi {
            let sum_sq: f32 = audio[start..start + QUIET_SPAN]
                .iter()
                .map(|sample| sample * sample)
                .sum();
            let rms = (sum_sq / QUIET_SPAN as f32).sqrt();
            if best.map(|(_, value)| rms < value).unwrap_or(true) {
                best = Some((start + QUIET_SPAN / 2, rms));
            }
            start += QUIET_HOP;
        }
        best.map(|(cut, _)| cut)
    }

    fn append_with_overlap(buffer: &mut String, next: &str) {
        if next.is_empty() {
            return;
        }
        if buffer.is_empty() {
            buffer.push_str(next);
            return;
        }
        let max = buffer.len().min(next.len()).min(OVERLAP_CHARS);
        let mut overlap = 0usize;
        for len in 1..=max {
            if buffer.is_char_boundary(buffer.len() - len)
                && next.is_char_boundary(len)
                && buffer[buffer.len() - len..].eq_ignore_ascii_case(&next[..len])
            {
                overlap = len;
            }
        }
        let remainder = &next[overlap..];
        if remainder.is_empty() {
            return;
        }
        if overlap == 0
            && !buffer.ends_with(char::is_whitespace)
            && !remainder.starts_with(char::is_whitespace)
        {
            buffer.push(' ');
        }
        buffer.push_str(remainder);
    }

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
}

#[cfg(target_os = "macos")]
fn main() -> Result<(), String> {
    mac::run()
}
