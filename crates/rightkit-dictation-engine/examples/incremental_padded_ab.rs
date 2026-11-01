//! Diagnostic only: compare the shipped post-Stop padded-window decode with
//! the same windows committed incrementally while audio is still arriving.
//!
//! Usage:
//!   incremental_padded_ab <model_dir> <clip.wav> [more clips.wav ...]
//! Optional: RIGHTKIT_INCREMENTAL_TARGET_SECS repeats/truncates each clip to
//! an exact duration for scaling tests.

#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
use rightkit_dictation_engine::coreml_asr::CoreMlParakeet;

#[cfg(target_os = "macos")]
use std::time::Instant;

#[cfg(target_os = "macos")]
const SAMPLE_RATE: usize = 16_000;
#[cfg(target_os = "macos")]
const WINDOW: usize = 15 * SAMPLE_RATE;
#[cfg(target_os = "macos")]
const PADDING: usize = 2_240 * SAMPLE_RATE / 1_000;
#[cfg(target_os = "macos")]
const SILENCE_WINDOW: usize = 200 * SAMPLE_RATE / 1_000;
#[cfg(target_os = "macos")]
const SILENCE_HOP: usize = 100 * SAMPLE_RATE / 1_000;
#[cfg(target_os = "macos")]
const OVERLAP_CHARS: usize = 16;

#[cfg(target_os = "macos")]
#[derive(Debug)]
struct IncrementalResult {
    text: String,
    background_ms: u128,
    tail_ms: u128,
    windows: usize,
    tail_secs: f64,
}

#[cfg(target_os = "macos")]
fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        return Err(
            "usage: incremental_padded_ab <model_dir> <clip.wav> [more clips.wav ...]".into(),
        );
    }

    let model = CoreMlParakeet::load(std::path::Path::new(&args[1]))?;
    let first = load_wav(std::path::Path::new(&args[2]))?;
    model.transcribe_pieces_timed(&first[..first.len().min(3 * SAMPLE_RATE)])?;

    let mut exact = 0usize;
    let mut total_errors = 0usize;
    let mut total_batch_words = 0usize;
    let mut total_batch_ms = 0u128;
    let mut total_background_ms = 0u128;
    let mut total_tail_ms = 0u128;
    let target_samples = std::env::var("RIGHTKIT_INCREMENTAL_TARGET_SECS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|seconds| *seconds > 0)
        .map(|seconds| seconds * SAMPLE_RATE);

    for clip in &args[2..] {
        let source = load_wav(std::path::Path::new(clip))?;
        let raw = target_samples
            .map(|samples| fit_duration(&source, samples))
            .unwrap_or(source);
        let batch_started = Instant::now();
        let batch_text = decode_batch(&model, &raw)?;
        let batch_ms = batch_started.elapsed().as_millis();

        let incremental = decode_incrementally(&model, &raw)?;
        let batch_words = words(&batch_text);
        let incremental_words = words(&incremental.text);
        let errors = edit_distance(&batch_words, &incremental_words);
        let disagreement = if batch_words.is_empty() {
            0.0
        } else {
            errors as f64 * 100.0 / batch_words.len() as f64
        };
        // Hypothesis dump for canonical WER scoring (env-gated, diagnostic only).
        if std::env::var("RIGHTKIT_AB_DUMP").is_ok() {
            let id = std::path::Path::new(clip)
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let esc = |t: &str| t.replace('\\', "\\\\").replace('"', "\\\"");
            eprintln!(
                "DUMP\t{}\t{}\t{}",
                id,
                esc(&batch_text),
                esc(&incremental.text)
            );
        }
        let is_exact = batch_text == incremental.text;
        exact += usize::from(is_exact);
        total_errors += errors;
        total_batch_words += batch_words.len();
        total_batch_ms += batch_ms;
        total_background_ms += incremental.background_ms;
        total_tail_ms += incremental.tail_ms;

        println!(
            "clip={} secs={:.1} windows={} tail={:.1}s batch={}ms background={}ms tail_ms={} saved_after_stop={:.1}% exact={} disagreement={:.2}%",
            std::path::Path::new(clip)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy(),
            raw.len() as f64 / SAMPLE_RATE as f64,
            incremental.windows,
            incremental.tail_secs,
            batch_ms,
            incremental.background_ms,
            incremental.tail_ms,
            saved_pct(batch_ms, incremental.tail_ms),
            is_exact,
            disagreement,
        );
    }

    let clips = args.len() - 2;
    let disagreement = if total_batch_words == 0 {
        0.0
    } else {
        total_errors as f64 * 100.0 / total_batch_words as f64
    };
    println!(
        "SUMMARY clips={} exact={}/{} disagreement={:.2}% batch={}ms background={}ms tail={}ms saved_after_stop={:.1}%",
        clips,
        exact,
        clips,
        disagreement,
        total_batch_ms,
        total_background_ms,
        total_tail_ms,
        saved_pct(total_batch_ms, total_tail_ms),
    );
    Ok(())
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
fn decode_batch(model: &CoreMlParakeet, raw: &[f32]) -> Result<String, String> {
    let audio = rightkit_dictation_core::audio_conditioning::condition_for_asr(
        raw,
        SAMPLE_RATE as u32,
        "default",
    );
    let mut text = String::new();
    let mut start = 0usize;
    while start + WINDOW < audio.len() {
        let cut = start + WINDOW - PADDING
            + quiet_cut(&audio[start + WINDOW - PADDING..start + WINDOW]).unwrap_or(PADDING);
        append_with_overlap(&mut text, &decode(model, &audio[start..cut])?);
        start = cut.max(start + 1);
    }
    if start < audio.len() {
        append_with_overlap(&mut text, &decode(model, &audio[start..])?);
    }
    Ok(text.trim().to_string())
}

#[cfg(target_os = "macos")]
fn decode_incrementally(model: &CoreMlParakeet, raw: &[f32]) -> Result<IncrementalResult, String> {
    let mut text = String::new();
    let mut start = 0usize;
    let mut background_ms = 0u128;
    let mut windows = 0usize;

    while start + WINDOW < raw.len() {
        let started = Instant::now();
        let conditioned = rightkit_dictation_core::audio_conditioning::condition_for_asr(
            &raw[start..start + WINDOW],
            SAMPLE_RATE as u32,
            "default",
        );
        let cut = WINDOW - PADDING + quiet_cut(&conditioned[WINDOW - PADDING..]).unwrap_or(PADDING);
        append_with_overlap(&mut text, &decode(model, &conditioned[..cut])?);
        background_ms += started.elapsed().as_millis();
        start += cut.max(1);
        windows += 1;
    }

    let tail_secs = (raw.len() - start) as f64 / SAMPLE_RATE as f64;
    let tail_started = Instant::now();
    let tail = rightkit_dictation_core::audio_conditioning::condition_for_asr(
        &raw[start..],
        SAMPLE_RATE as u32,
        "default",
    );
    append_with_overlap(&mut text, &decode(model, &tail)?);
    let tail_ms = tail_started.elapsed().as_millis();

    Ok(IncrementalResult {
        text: text.trim().to_string(),
        background_ms,
        tail_ms,
        windows,
        tail_secs,
    })
}

#[cfg(target_os = "macos")]
fn decode(model: &CoreMlParakeet, audio: &[f32]) -> Result<String, String> {
    Ok(model
        .transcribe_pieces_timed(audio)?
        .into_iter()
        .map(|piece| piece.text)
        .collect::<String>()
        .trim()
        .to_string())
}

#[cfg(target_os = "macos")]
fn quiet_cut(search: &[f32]) -> Option<usize> {
    if search.len() < SILENCE_WINDOW {
        return None;
    }
    let mut best: Option<(usize, f32)> = None;
    let mut offset = 0usize;
    while offset + SILENCE_WINDOW <= search.len() {
        let level = rightkit_dictation_core::audio_conditioning::rms(
            &search[offset..offset + SILENCE_WINDOW],
        );
        if best
            .map(|(_, best_level)| level < best_level)
            .unwrap_or(true)
        {
            best = Some((offset + SILENCE_WINDOW / 2, level));
        }
        offset += SILENCE_HOP;
    }
    best.map(|(cut, _)| cut)
}

#[cfg(target_os = "macos")]
fn append_with_overlap(buffer: &mut String, next: &str) {
    if next.is_empty() {
        return;
    }
    if buffer.is_empty() {
        buffer.push_str(next);
        return;
    }
    let overlap = suffix_prefix_overlap(buffer, next, OVERLAP_CHARS);
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

#[cfg(target_os = "macos")]
fn suffix_prefix_overlap(a: &str, b: &str, max_chars: usize) -> usize {
    let max = a.len().min(b.len()).min(max_chars);
    (1..=max)
        .filter(|&len| a.is_char_boundary(a.len() - len) && b.is_char_boundary(len))
        .filter(|&len| a[a.len() - len..].eq_ignore_ascii_case(&b[..len]))
        .max()
        .unwrap_or(0)
}

#[cfg(target_os = "macos")]
fn words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|word| {
            word.trim_matches(|c: char| !c.is_alphanumeric())
                .to_ascii_lowercase()
        })
        .filter(|word| !word.is_empty())
        .collect()
}

#[cfg(target_os = "macos")]
fn edit_distance(a: &[String], b: &[String]) -> usize {
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (a_index, a_word) in a.iter().enumerate() {
        let mut current = vec![a_index + 1; b.len() + 1];
        for (b_index, b_word) in b.iter().enumerate() {
            current[b_index + 1] = if a_word == b_word {
                previous[b_index]
            } else {
                1 + previous[b_index]
                    .min(previous[b_index + 1])
                    .min(current[b_index])
            };
        }
        previous = current;
    }
    previous[b.len()]
}

#[cfg(target_os = "macos")]
fn saved_pct(batch_ms: u128, tail_ms: u128) -> f64 {
    if batch_ms == 0 {
        0.0
    } else {
        (1.0 - tail_ms as f64 / batch_ms as f64) * 100.0
    }
}

#[cfg(target_os = "macos")]
fn load_wav(path: &std::path::Path) -> Result<Vec<f32>, String> {
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
