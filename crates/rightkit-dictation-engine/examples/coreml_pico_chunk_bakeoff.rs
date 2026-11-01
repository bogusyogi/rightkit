//! Pico eval sweep for the shipped CoreML Parakeet bundle.
//!
//! Modes:
//! - hard: current 15s hard windows inside CoreMlParakeet.
//! - windows: NVIDIA/Windows-agent buffered shape (2.24 / 4.48 / 2.24).
//! - padded_window: 15s window with trailing padding searched for a quiet cut.
//! - combo: Windows buffered shape with quiet cut in right padding.
#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
use rightkit_dictation_engine::coreml_asr::{CoreMlParakeet, TimedPiece};
#[cfg(target_os = "macos")]
use serde::Deserialize;
#[cfg(target_os = "macos")]
use serde_json::json;
#[cfg(target_os = "macos")]
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Instant,
};

#[cfg(target_os = "macos")]
const SR: usize = 16_000;
#[cfg(target_os = "macos")]
const MODEL_WINDOW: usize = 15 * SR;
#[cfg(target_os = "macos")]
const PADDED_WINDOW_PADDING_MS: u64 = 2_240;

#[cfg(target_os = "macos")]
#[derive(Debug, Deserialize)]
struct EvalItem {
    wav: String,
    gt: String,
    corpus: String,
    dur_s: f64,
}

#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug)]
enum Mode {
    Hard,
    Windows,
    PaddedWindow,
    Combo,
}

#[cfg(target_os = "macos")]
impl Mode {
    fn name(self) -> &'static str {
        match self {
            Mode::Hard => "hard",
            Mode::Windows => "windows_2240_4480_2240",
            Mode::PaddedWindow => "padded_window",
            Mode::Combo => "combo_windows_silence_padding",
        }
    }
}

#[cfg(target_os = "macos")]
fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        return Err(
            "usage: coreml_pico_chunk_bakeoff <bundle> <pico_eval_dir> [out.json] [mode,mode]"
                .to_string(),
        );
    }
    let bundle = PathBuf::from(&args[1]);
    let eval_dir = PathBuf::from(&args[2]);
    let out_path = args.get(3).map(PathBuf::from);
    let modes = selected_modes(args.get(4).map(String::as_str))?;

    let items: Vec<EvalItem> = serde_json::from_str(
        &std::fs::read_to_string(eval_dir.join("eval_set.json")).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("parse eval_set.json: {e}"))?;

    let model = CoreMlParakeet::load(&bundle)?;
    if let Some(first) = items.first() {
        let _ = model.transcribe(&load_wav(&eval_dir.join(&first.wav))?);
    }

    let mut report = BTreeMap::new();
    for mode in modes {
        let started = Instant::now();
        let mut refs = Vec::new();
        let mut hyps = Vec::new();
        let mut by_corpus: BTreeMap<String, (Vec<String>, Vec<String>)> = BTreeMap::new();
        let mut rows = Vec::new();
        let mut total_audio_s = 0.0;

        for item in &items {
            let audio = load_wav(&eval_dir.join(&item.wav))?;
            let decode_started = Instant::now();
            let hyp = transcribe_mode(&model, &audio, mode)?;
            let decode_s = decode_started.elapsed().as_secs_f64();
            let gt = norm(&item.gt);
            let hyp_n = norm(&hyp);
            refs.push(gt.clone());
            hyps.push(hyp_n.clone());
            by_corpus
                .entry(item.corpus.clone())
                .or_default()
                .0
                .push(gt.clone());
            by_corpus
                .entry(item.corpus.clone())
                .or_default()
                .1
                .push(hyp_n.clone());
            total_audio_s += item.dur_s;
            rows.push(json!({
                "wav": item.wav,
                "corpus": item.corpus,
                "duration_s": item.dur_s,
                "decode_s": decode_s,
                "wer": wer_one(&gt, &hyp_n),
                "hyp": hyp,
            }));
        }

        let decode_s = started.elapsed().as_secs_f64();
        let mut per_corpus = BTreeMap::new();
        for (corpus, (r, h)) in by_corpus {
            per_corpus.insert(corpus, wer(&r, &h));
        }
        let overall = wer(&refs, &hyps);
        let over15: Vec<_> = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row["duration_s"].as_f64().unwrap_or(0.0) > 15.0)
            .map(|(i, _)| i)
            .collect();
        let over15_wer = if over15.is_empty() {
            0.0
        } else {
            let r: Vec<_> = over15.iter().map(|&i| refs[i].clone()).collect();
            let h: Vec<_> = over15.iter().map(|&i| hyps[i].clone()).collect();
            wer(&r, &h)
        };
        println!(
            "{:<32} WER {:>6.2}%  >15s {:>6.2}%  decode {:>7.2}s  RTF {:.4}",
            mode.name(),
            overall * 100.0,
            over15_wer * 100.0,
            decode_s,
            decode_s / total_audio_s
        );
        report.insert(
            mode.name().to_string(),
            json!({
                "wer": overall,
                "wer_pct": overall * 100.0,
                "over15_wer": over15_wer,
                "over15_wer_pct": over15_wer * 100.0,
                "decode_s": decode_s,
                "rtf": decode_s / total_audio_s,
                "total_audio_s": total_audio_s,
                "per_corpus": per_corpus,
                "rows": rows,
            }),
        );
    }

    if let Some(path) = out_path {
        std::fs::write(&path, serde_json::to_string_pretty(&report).unwrap())
            .map_err(|e| format!("write {}: {e}", path.display()))?;
        println!("wrote {}", path.display());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn selected_modes(raw: Option<&str>) -> Result<Vec<Mode>, String> {
    let Some(raw) = raw else {
        return Ok(vec![
            Mode::Hard,
            Mode::Windows,
            Mode::PaddedWindow,
            Mode::Combo,
        ]);
    };
    let modes: Vec<_> = raw
        .split(',')
        .map(|name| {
            let name = name.trim();
            match name {
                "hard" => Ok(Mode::Hard),
                "windows_2240_4480_2240" => Ok(Mode::Windows),
                "padded_window" => Ok(Mode::PaddedWindow),
                "combo_windows_silence_padding" => Ok(Mode::Combo),
                _ => Err(format!("unknown mode {name:?}")),
            }
        })
        .collect::<Result<_, _>>()?;
    if modes.is_empty() {
        return Err("at least one mode is required".to_string());
    }
    Ok(modes)
}

#[cfg(target_os = "macos")]
fn transcribe_mode(model: &CoreMlParakeet, audio: &[f32], mode: Mode) -> Result<String, String> {
    if matches!(mode, Mode::Hard) || audio.len() <= MODEL_WINDOW {
        return model.transcribe(audio);
    }
    let mut committer = Committer::new(if matches!(mode, Mode::Windows | Mode::Combo) {
        6
    } else {
        0
    });
    match mode {
        Mode::Windows => buffered(
            model,
            audio,
            ms(2_240),
            ms(4_480),
            ms(2_240),
            false,
            &mut committer,
        ),
        Mode::PaddedWindow => padded_window(model, audio, &mut committer),
        Mode::Combo => buffered(
            model,
            audio,
            ms(2_240),
            ms(4_480),
            ms(2_240),
            true,
            &mut committer,
        ),
        Mode::Hard => unreachable!(),
    }?;
    Ok(committer.finish())
}

#[cfg(target_os = "macos")]
fn buffered(
    model: &CoreMlParakeet,
    audio: &[f32],
    left: usize,
    chunk: usize,
    right: usize,
    quiet_cut: bool,
    out: &mut Committer,
) -> Result<(), String> {
    let mut stable_start = 0usize;
    while stable_start < audio.len() {
        let nominal_end = (stable_start + chunk).min(audio.len());
        let window_start = stable_start.saturating_sub(left);
        let window_end = (stable_start + chunk + right).min(audio.len());
        let stable_end = if quiet_cut && nominal_end < audio.len() {
            quiet_cut_in(audio, nominal_end, window_end).unwrap_or(window_end)
        } else {
            nominal_end
        };
        commit_window(
            model,
            audio,
            window_start,
            window_end,
            stable_start,
            stable_end,
            out,
        )?;
        stable_start = stable_end.max(stable_start + 1);
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn padded_window(model: &CoreMlParakeet, audio: &[f32], out: &mut Committer) -> Result<(), String> {
    let padding = ms(PADDED_WINDOW_PADDING_MS);
    let target = MODEL_WINDOW.saturating_sub(padding);
    let mut start = 0usize;
    while start < audio.len() {
        let window_end = (start + MODEL_WINDOW).min(audio.len());
        let target_end = (start + target).min(audio.len());
        let stable_end = if window_end < audio.len() {
            // ponytail: deterministic quietest-RMS cut; swap to VAD if noisy padding needs it.
            quiet_cut_in(audio, target_end, window_end).unwrap_or(window_end)
        } else {
            audio.len()
        };
        commit_window(model, audio, start, window_end, start, stable_end, out)?;
        start = stable_end.max(start + 1);
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn commit_window(
    model: &CoreMlParakeet,
    audio: &[f32],
    window_start: usize,
    window_end: usize,
    stable_start: usize,
    stable_end: usize,
    out: &mut Committer,
) -> Result<(), String> {
    let pieces = model.transcribe_pieces_timed(&audio[window_start..window_end])?;
    let offset = window_start as f32 / SR as f32;
    let lo = stable_start as f32 / SR as f32;
    let hi = stable_end as f32 / SR as f32;
    out.push(&pieces_in_range(&pieces, offset, lo, hi));
    Ok(())
}

#[cfg(target_os = "macos")]
fn pieces_in_range(pieces: &[TimedPiece], offset: f32, lo: f32, hi: f32) -> String {
    pieces
        .iter()
        .filter(|p| {
            let start = offset + p.start;
            start >= lo && start < hi
        })
        .map(|p| p.text.as_str())
        .collect::<String>()
}

#[cfg(target_os = "macos")]
fn quiet_cut_in(audio: &[f32], lo: usize, hi: usize) -> Option<usize> {
    if hi <= lo {
        return None;
    }
    let win = ms(200);
    let hop = ms(100);
    if hi.saturating_sub(lo) < win {
        return None;
    }
    let mut best = None;
    let mut i = lo;
    while i + win <= hi {
        let r = rms(&audio[i..i + win]);
        if best.map(|(_, b)| r < b).unwrap_or(true) {
            best = Some((i + win / 2, r));
        }
        i += hop;
    }
    best.map(|(cut, _)| cut)
}

#[cfg(target_os = "macos")]
#[derive(Debug)]
struct Committer {
    stable: String,
    tail: String,
    hold_words: usize,
}

#[cfg(target_os = "macos")]
impl Committer {
    fn new(hold_words: usize) -> Self {
        Self {
            stable: String::new(),
            tail: String::new(),
            hold_words,
        }
    }

    fn push(&mut self, chunk: &str) {
        append_with_overlap(&mut self.tail, chunk);
        let (stable, tail) = split_tail(&self.tail, self.hold_words);
        append_with_overlap(&mut self.stable, &stable);
        self.tail = tail;
    }

    fn finish(mut self) -> String {
        append_with_overlap(&mut self.stable, &self.tail);
        self.stable.trim().to_string()
    }
}

#[cfg(target_os = "macos")]
fn append_with_overlap(buffer: &mut String, next: &str) {
    if next.is_empty() {
        return;
    }
    let overlap = suffix_prefix_overlap(buffer, next, 16);
    buffer.push_str(&next[overlap..]);
}

#[cfg(target_os = "macos")]
fn suffix_prefix_overlap(a: &str, b: &str, max_chars: usize) -> usize {
    let max = a.len().min(b.len()).min(max_chars);
    let mut best = 0usize;
    for len in 1..=max {
        if !a.is_char_boundary(a.len() - len) || !b.is_char_boundary(len) {
            continue;
        }
        if a[a.len() - len..].eq_ignore_ascii_case(&b[..len]) {
            best = len;
        }
    }
    best
}

#[cfg(target_os = "macos")]
fn split_tail(input: &str, hold_words: usize) -> (String, String) {
    if hold_words == 0 {
        return (input.to_string(), String::new());
    }
    let ranges = word_ranges(input);
    if ranges.len() <= hold_words {
        return (String::new(), input.to_string());
    }
    let split_at = ranges[ranges.len() - hold_words].0;
    (input[..split_at].to_string(), input[split_at..].to_string())
}

#[cfg(target_os = "macos")]
fn word_ranges(input: &str) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut start = None;
    for (idx, ch) in input.char_indices() {
        if ch.is_whitespace() {
            if let Some(s) = start.take() {
                ranges.push((s, idx));
            }
        } else if start.is_none() {
            start = Some(idx);
        }
    }
    if let Some(s) = start {
        ranges.push((s, input.len()));
    }
    ranges
}

#[cfg(target_os = "macos")]
fn load_wav(path: &Path) -> Result<Vec<f32>, String> {
    let wav = rightkit_audio::wav::read_wav_mono_f32(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if wav.sample_rate != SR as u32 {
        return Err(format!(
            "{}: expected {} Hz WAV, got {} Hz",
            path.display(),
            SR as u32,
            wav.sample_rate
        ));
    }
    Ok(wav.samples)
}

#[cfg(target_os = "macos")]
fn norm(text: &str) -> String {
    text.to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '\'' {
                c
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(target_os = "macos")]
fn wer_one(reference: &str, hypothesis: &str) -> f64 {
    wer(&[reference.to_string()], &[hypothesis.to_string()])
}

#[cfg(target_os = "macos")]
fn wer(refs: &[String], hyps: &[String]) -> f64 {
    let r = refs.join(" ");
    let h = hyps.join(" ");
    let rw: Vec<&str> = r.split_whitespace().collect();
    let hw: Vec<&str> = h.split_whitespace().collect();
    if rw.is_empty() {
        return if hw.is_empty() { 0.0 } else { 1.0 };
    }
    let mut prev: Vec<usize> = (0..=hw.len()).collect();
    for (i, rw_i) in rw.iter().enumerate() {
        let mut cur = vec![i + 1; hw.len() + 1];
        for (j, hw_j) in hw.iter().enumerate() {
            cur[j + 1] = (prev[j + 1] + 1)
                .min(cur[j] + 1)
                .min(prev[j] + usize::from(rw_i != hw_j));
        }
        prev = cur;
    }
    prev[hw.len()] as f64 / rw.len() as f64
}

#[cfg(target_os = "macos")]
fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

#[cfg(target_os = "macos")]
fn ms(ms: u64) -> usize {
    (SR as u64 * ms / 1_000) as usize
}
