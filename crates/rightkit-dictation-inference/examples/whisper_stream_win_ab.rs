//! Windows Whisper: scheduled Static15 against the deferred baseline.
//!
//! The macOS sweep (`rightkit-dictation-engine/examples/whisper_stream_interval_sweep.rs`)
//! cannot run here — it drives `WhisperCoreMl`, and `rightkit-dictation-engine` does not
//! build on Windows. This is the same two arms against `WhisperWin`, which is
//! what Windows actually ships:
//!
//!   deferred30      decode the whole utterance at key release (WINDOWS TODAY —
//!                   `WhisperWin` falls into `BackgroundDecodeKind::Other`, so
//!                   `backend_supports_scheduled_static15` returns false and no
//!                   segment is ever decoded during hold)
//!   checkpoint:15   the macOS scheduled path: disjoint ~15 s segments cut at the
//!                   quietest point of a trailing 2 240 ms search region, joined
//!                   with the 16-char overlap dedup
//!
//! The quiet-cut and seam-join constants are copied verbatim from the macOS
//! harness so the two platforms measure the same algorithm.
//!
//!   whisper_stream_win_ab <models-dir> <eval-dir> <out-dir> [arm ...]
#[cfg(not(target_os = "windows"))]
fn main() {}

#[cfg(target_os = "windows")]
fn main() {
    use rightkit_dictation_inference::whisper_win::WhisperWin;
    use std::path::{Path, PathBuf};
    use std::time::Instant;

    const SR: usize = 16_000;
    const PADDING_MS: usize = 2_240;
    const SILENCE_MS: usize = 200;
    const HOP_MS: usize = 100;
    const OVERLAP_CHARS: usize = 16;

    fn rms(x: &[f32]) -> f32 {
        rightkit_dictation_core::audio_conditioning::rms(x)
    }

    /// Lowest-RMS 200 ms span in the search region, scanned every 100 ms.
    fn quiet_cut(search: &[f32]) -> Option<usize> {
        let span = SILENCE_MS * SR / 1000;
        let hop = HOP_MS * SR / 1000;
        if search.len() < span {
            return None;
        }
        let mut best: Option<(usize, f32)> = None;
        let mut off = 0usize;
        while off + span <= search.len() {
            let level = rms(&search[off..off + span]);
            if best.map(|(_, b)| level < b).unwrap_or(true) {
                best = Some((off + span / 2, level));
            }
            off += hop;
        }
        best.map(|(i, _)| i)
    }

    fn append_with_overlap(buffer: &mut String, next: &str) {
        let next = next.trim();
        if next.is_empty() {
            return;
        }
        if buffer.is_empty() {
            buffer.push_str(next);
            return;
        }
        let tail: String = buffer
            .chars()
            .rev()
            .take(OVERLAP_CHARS)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let lower_tail = tail.to_lowercase();
        let lower_next = next.to_lowercase();
        let mut overlap = 0usize;
        for k in (1..=lower_tail.chars().count().min(OVERLAP_CHARS)).rev() {
            let suffix: String = lower_tail
                .chars()
                .skip(lower_tail.chars().count() - k)
                .collect();
            if lower_next.starts_with(&suffix) {
                overlap = k;
                break;
            }
        }
        let keep: String = next.chars().skip(overlap).collect();
        let keep = keep.trim_start();
        if keep.is_empty() {
            return;
        }
        if !buffer.ends_with(' ') {
            buffer.push(' ');
        }
        buffer.push_str(keep);
    }

    fn load_wav(path: &Path) -> Vec<f32> {
        let wav = rightkit_audio::wav::read_wav_mono_f32(path)
            .unwrap_or_else(|e| panic!("open {}: {e}", path.display()));
        wav.samples
    }

    let a: Vec<String> = std::env::args().collect();
    if a.len() < 4 {
        eprintln!("usage: whisper_stream_win_ab <models-dir> <eval-dir> <out-dir> [arm ...]");
        std::process::exit(2);
    }
    let arms: Vec<String> = if a.len() > 4 {
        a[4..].to_vec()
    } else {
        vec!["deferred30".into(), "checkpoint:15".into()]
    };
    let eval_dir = PathBuf::from(&a[2]);
    let out_dir = PathBuf::from(&a[3]);
    std::fs::create_dir_all(&out_dir).unwrap();

    let set: Vec<serde_json::Value> =
        serde_json::from_str(&std::fs::read_to_string(eval_dir.join("eval_set.json")).unwrap())
            .unwrap();
    eprintln!("loading whisper-win …");
    let model = WhisperWin::load(Path::new(&a[1])).expect("load WhisperWin");
    let lang = "en";

    for arm in &arms {
        eprintln!("=== arm {arm} ===");
        let started = Instant::now();
        let mut rows: Vec<serde_json::Value> = Vec::new();
        for (i, e) in set.iter().enumerate() {
            let wav = eval_dir.join(e["wav"].as_str().unwrap());
            let audio = load_wav(&wav);
            let (text, bg_ms, tail_ms, tail_s, bg_passes) = if arm == "deferred30" {
                let t0 = Instant::now();
                let text = model.transcribe_lang(&audio, lang).unwrap_or_default();
                (
                    text,
                    0u128,
                    t0.elapsed().as_millis(),
                    audio.len() as f64 / SR as f64,
                    0usize,
                )
            } else {
                let window = 15 * SR;
                let padding = PADDING_MS * SR / 1000;
                let target = window.saturating_sub(padding);
                let mut text = String::new();
                let mut start = 0usize;
                let mut bg = 0u128;
                let mut passes = 0usize;
                while start + window < audio.len() {
                    let t0 = Instant::now();
                    let cut = start
                        + target
                        + quiet_cut(&audio[start + target..start + window]).unwrap_or(padding);
                    let cut = cut.max(start + 1).min(audio.len());
                    let seg = model
                        .transcribe_lang(&audio[start..cut], lang)
                        .unwrap_or_default();
                    append_with_overlap(&mut text, &seg);
                    bg += t0.elapsed().as_millis();
                    passes += 1;
                    start = cut;
                }
                let tail_s = (audio.len() - start) as f64 / SR as f64;
                let t0 = Instant::now();
                let seg = model
                    .transcribe_lang(&audio[start..], lang)
                    .unwrap_or_default();
                append_with_overlap(&mut text, &seg);
                (text, bg, t0.elapsed().as_millis(), tail_s, passes)
            };
            rows.push(serde_json::json!({
                "clip_id": wav.file_stem().unwrap().to_string_lossy(),
                "gt": e["gt"],
                "hyp": text.trim(),
                "dur_s": audio.len() as f64 / SR as f64,
                "elapsed_s": (bg_ms + tail_ms) as f64 / 1000.0,
                "background_ms": bg_ms,
                "tail_ms": tail_ms,
                "tail_s": tail_s,
                "bg_passes": bg_passes,
                "tail_passes": 1,
            }));
            if i.is_multiple_of(10) {
                eprintln!(
                    "  {arm}: {i}/{} ({:.0}s)",
                    set.len(),
                    started.elapsed().as_secs_f64()
                );
            }
        }
        let label = arm.replace(':', "-");
        let p = out_dir.join(format!("{label}.json"));
        std::fs::write(&p, serde_json::to_string_pretty(&rows).unwrap()).unwrap();
        eprintln!(
            "arm {arm} done in {:.1}s -> {}",
            started.elapsed().as_secs_f64(),
            p.display()
        );
    }
}
