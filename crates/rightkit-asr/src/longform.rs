//! The one long-form Parakeet route: padded-window schedule, quiet cut, seam
//! stitching, timed-piece bookkeeping, word grouping and SRT/VTT/JSON export.
//! Every Parakeet backend (CoreML on macOS, ORT elsewhere) feeds its
//! single-window decoder into [`transcribe_padded_window`]; nothing else
//! stitches windows.
//!
//! Contract: HeardRight `docs/ASR_DECODE_CONTRACT.md` (LOCKED 2026-07-19):
//! disjoint windows cut at the quietest 200 ms span (100 ms hop) in the
//! trailing 2.24 s of the 15 s window, decode exactly `[start, cut)`, commit
//! every piece, advance `start = max(cut, start + 1)`, join seams with a
//! 16-character overlap rule that inserts a separator when overlap is zero.
//! Donors: HeardRight `asr/windowing.rs` + `asr/runtime.rs` (schedule, seam,
//! terminal flush) and ScrapeRight `speech/src/asr_window.rs` (cancellation,
//! progress, cues, SRT/VTT/JSON exports).
//!
//! Live-dictation-only HeardRight pieces (ScheduledStatic15, the terminal
//! command quiet-cut, control alignment) stay in the HeardRight adapter.

use crate::error::{Error, Result};
use crate::parakeet_core::{TimedToken, TranscriptionResult};
use std::borrow::Cow;
use std::sync::atomic::{AtomicBool, Ordering};

pub const SAMPLE_RATE: usize = 16_000;
pub const PADDED_WINDOW_MS: u64 = 15_000;
pub const PADDED_WINDOW_PADDING_MS: u64 = 2_240;
pub const PADDED_WINDOW_SILENCE_MS: u64 = 200;
pub const PADDED_WINDOW_SILENCE_HOP_MS: u64 = 100;
pub const PADDED_WINDOW_OVERLAP_CHARS: usize = 16;

// Terminal-silence flush (ONNX/DirectML measurements; see `LongFormOptions`).
const FINAL_TAIL_SILENCE_MS: u64 = 480;
const MEL_HOP_SAMPLES: usize = 160;
const FINAL_TAIL_PROBE_MS: u64 = 160;
const FINAL_TAIL_SPEECH_FLOOR: f32 = 0.004;
const FINAL_TAIL_SPEECH_RATIO: f32 = 0.3;

/// How window results are joined at a seam.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SeamPolicy {
    /// HeardRight / ScrapeRight: de-duplicate up to 16 characters of
    /// suffix/prefix overlap and insert a separator when there is none. Built
    /// for transcript prose.
    #[default]
    DedupOverlap,
    /// CutRight: windows are disjoint (cut at the quietest point, no audio
    /// overlap), so a repeated word across a seam is a word the speaker really
    /// said; the overlap eraser would silently delete it and its timing.
    /// Pieces are committed exactly as decoded.
    Exact,
}

/// Long-form schedule options. Defaults reproduce each platform's locked
/// production policy: windows are 15 s; only Windows (ONNX beam) appends a
/// terminal-silence flush to a final segment that still ends in speech. macOS
/// is release-locked without it, and every other target keeps that behaviour.
#[derive(Debug, Clone)]
pub struct LongFormOptions {
    /// Model window in samples (`pipeline.json window_sec * 16000`).
    pub window_samples: usize,
    /// Append up to 480 ms of digital silence to a final segment that still
    /// ends in speech (never past one mel hop below the window).
    pub terminal_silence: bool,
    pub seam: SeamPolicy,
}

impl Default for LongFormOptions {
    fn default() -> Self {
        Self {
            window_samples: padded_window_samples(),
            terminal_silence: cfg!(target_os = "windows"),
            seam: SeamPolicy::DedupOverlap,
        }
    }
}

/// Progress after each committed window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LongFormProgress {
    pub windows_done: usize,
    /// Estimate: `ceil((len - padding) / (window - padding))`, at least 1.
    pub windows_estimated: usize,
    pub samples_done: usize,
    pub samples_total: usize,
}

/// A word (one or more pieces joined) with its timing.
#[derive(Debug, Clone, PartialEq)]
pub struct TimedWord {
    pub text: String,
    pub start: f32,
    pub end: f32,
}

pub fn ms_to_samples(ms: u64) -> usize {
    (SAMPLE_RATE as u64).saturating_mul(ms).saturating_div(1_000) as usize
}

pub fn padded_window_samples() -> usize {
    ms_to_samples(PADDED_WINDOW_MS)
}

fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

/// Quietest 200 ms / 100 ms-hop span inside `[lo, hi)`; returns its midpoint.
/// Strict argmin: ties keep the EARLIEST span.
pub fn quiet_cut_in(samples: &[f32], lo: usize, hi: usize) -> Option<usize> {
    let win = ms_to_samples(PADDED_WINDOW_SILENCE_MS);
    let hop = ms_to_samples(PADDED_WINDOW_SILENCE_HOP_MS);
    if hi <= lo || hi.saturating_sub(lo) < win {
        return None;
    }
    let mut best: Option<(usize, f32)> = None;
    let mut i = lo;
    while i + win <= hi {
        let v = rms(&samples[i..i + win]);
        if best.map(|(_, b)| v < b).unwrap_or(true) {
            best = Some((i + win / 2, v));
        }
        i += hop;
    }
    best.map(|(c, _)| c)
}

/// Join the next window's text onto `buffer`, de-duplicating the seam. With
/// disjoint windows the zero-overlap case is the common one and a separator
/// is mandatory: a raw `push_str` welds words across the seam ("AndersonCounty").
pub fn append_with_overlap(buffer: &mut String, next: &str) {
    if next.is_empty() {
        return;
    }
    if buffer.is_empty() {
        buffer.push_str(next);
        return;
    }
    let overlap = suffix_prefix_overlap(buffer, next, PADDED_WINDOW_OVERLAP_CHARS);
    let remainder = &next[overlap..];
    if remainder.is_empty() {
        return;
    }
    let needs_space = overlap == 0
        && !buffer.ends_with(char::is_whitespace)
        && !remainder.starts_with(char::is_whitespace);
    if needs_space {
        buffer.push(' ');
    }
    buffer.push_str(remainder);
}

pub fn suffix_prefix_overlap(a: &str, b: &str, max_chars: usize) -> usize {
    let max = a.len().min(b.len()).min(max_chars);
    let mut best = 0usize;
    for len in 1..=max {
        if !a.is_char_boundary(a.len() - len) || !b.is_char_boundary(len) {
            continue;
        }
        // Only consume a complete prefix at word edges ("that" + " Turtles"
        // must not consume " T" and render "thaturtles").
        let ends_inside_word = b[..len].chars().last().is_some_and(char::is_alphanumeric)
            && b[len..].chars().next().is_some_and(char::is_alphanumeric);
        if ends_inside_word && len < 3 {
            continue;
        }
        if a[a.len() - len..].eq_ignore_ascii_case(&b[..len]) {
            best = len;
        }
    }
    best
}

fn ends_in_speech(segment: &[f32]) -> bool {
    let probe = ms_to_samples(FINAL_TAIL_PROBE_MS).min(segment.len());
    if probe == 0 {
        return false;
    }
    let tail = rms(&segment[segment.len() - probe..]);
    tail > FINAL_TAIL_SPEECH_FLOOR && tail > rms(segment) * FINAL_TAIL_SPEECH_RATIO
}

/// Platform policy for the last segment of a recording: when enabled, append
/// up to 480 ms of silence to a segment that still ends in speech (a recording
/// stopped mid-word), leaving one mel hop of headroom below the window. Timing
/// is unaffected (silence goes after every real sample).
fn final_segment<'a>(segment: &'a [f32], opts: &LongFormOptions) -> Cow<'a, [f32]> {
    if !opts.terminal_silence {
        return Cow::Borrowed(segment);
    }
    let room = opts
        .window_samples
        .saturating_sub(MEL_HOP_SAMPLES)
        .saturating_sub(segment.len());
    let tail = ms_to_samples(FINAL_TAIL_SILENCE_MS).min(room);
    if tail == 0 || segment.is_empty() || !ends_in_speech(segment) {
        return Cow::Borrowed(segment);
    }
    let mut padded = Vec::with_capacity(segment.len() + tail);
    padded.extend_from_slice(segment);
    padded.resize(segment.len() + tail, 0.0);
    Cow::Owned(padded)
}

/// Keep the timing stream at the same seam as rendered text. An overlap is
/// consumed from both; an unprovable piece rendering drops timing only.
pub fn append_timed_result(
    text: &mut String,
    pieces: &mut Vec<TimedToken>,
    result: TranscriptionResult,
    offset: f32,
) -> (usize, bool) {
    let overlap = if text.is_empty() {
        0
    } else {
        suffix_prefix_overlap(text, &result.text, PADDED_WINDOW_OVERLAP_CHARS)
    };
    let remainder = &result.text[overlap..];
    let separator = !text.is_empty()
        && !remainder.is_empty()
        && overlap == 0
        && !text.ends_with(char::is_whitespace)
        && !remainder.starts_with(char::is_whitespace);
    append_with_overlap(text, &result.text);
    let rendered = result.tokens.iter().map(|t| t.text.as_str()).collect::<String>();
    if rendered.trim() != result.text.trim() {
        pieces.clear();
        return (overlap, separator);
    }
    let mut skip = if overlap == 0 {
        0
    } else {
        rendered.len() - rendered.trim_start().len() + overlap
    };
    let mut first = true;
    for mut piece in result.tokens {
        if skip >= piece.text.len() {
            skip -= piece.text.len();
            continue;
        }
        if skip > 0 {
            if !piece.text.is_char_boundary(skip) {
                pieces.clear();
                return (overlap, separator);
            }
            piece.text = piece.text[skip..].to_string();
            skip = 0;
        }
        if first && separator && !piece.text.starts_with(char::is_whitespace) {
            piece.text.insert(0, ' ');
        }
        first = false;
        piece.start += offset;
        piece.end += offset;
        pieces.push(piece);
    }
    (overlap, separator)
}

/// Commit a window exactly as decoded (no seam de-duplication).
pub fn append_exact_result(
    text: &mut String,
    pieces: &mut Vec<TimedToken>,
    result: TranscriptionResult,
    offset: f32,
) {
    for mut piece in result.tokens {
        text.push_str(&piece.text);
        piece.start += offset;
        piece.end += offset;
        pieces.push(piece);
    }
}

/// Resumable form of the padded-window loop: each [`step`](Self::step) performs
/// at most ONE decode so a caller can interleave other work (live dictation,
/// UI) between windows. [`transcribe_padded_window`] drives this, so there is a
/// single production algorithm.
#[derive(Debug, Default)]
pub struct PaddedWindowCursor {
    start: usize,
    text: String,
    pieces: Vec<TimedToken>,
    finished: bool,
}

impl PaddedWindowCursor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Samples consumed so far (monotonic). Equals `total` once finished.
    pub fn done_samples(&self, total: usize) -> usize {
        if self.finished {
            total
        } else {
            self.start.min(total)
        }
    }

    pub fn is_finished(&self) -> bool {
        self.finished
    }

    fn commit(&mut self, opts: &LongFormOptions, result: TranscriptionResult, start: usize) {
        let offset = start as f32 / SAMPLE_RATE as f32;
        match opts.seam {
            SeamPolicy::DedupOverlap => {
                append_timed_result(&mut self.text, &mut self.pieces, result, offset);
            }
            SeamPolicy::Exact => {
                append_exact_result(&mut self.text, &mut self.pieces, result, offset);
            }
        }
    }

    /// Decode one window. `Ok(None)` means more windows remain; `Ok(Some(_))`
    /// is the final transcript (the cursor is then finished).
    pub fn step<F>(
        &mut self,
        samples: &[f32],
        opts: &LongFormOptions,
        mut decode: F,
    ) -> Result<Option<TranscriptionResult>>
    where
        F: FnMut(&[f32]) -> Result<TranscriptionResult>,
    {
        if self.finished {
            return Err(Error::Config("padded window cursor already finished".into()));
        }
        let window = opts.window_samples;
        if samples.len() <= window {
            self.finished = true;
            return decode(&final_segment(samples, opts)).map(Some);
        }
        if self.start < samples.len() && self.start + window < samples.len() {
            let padding = ms_to_samples(PADDED_WINDOW_PADDING_MS);
            let target = window.saturating_sub(padding);
            let start = self.start;
            let window_end = (start + window).min(samples.len());
            let target_end = (start + target).min(samples.len());
            let stable_end = quiet_cut_in(samples, target_end, window_end).unwrap_or(window_end);
            let segment: &[f32] = if start < stable_end {
                &samples[start..stable_end]
            } else {
                &[]
            };
            let result = match decode(segment) {
                Ok(r) => r,
                Err(e) => {
                    self.finished = true;
                    return Err(e);
                }
            };
            self.commit(opts, result, start);
            self.start = stable_end.max(start + 1);
            return Ok(None);
        }
        // Tail after the final quiet cut.
        self.finished = true;
        if self.start < samples.len() {
            let result = decode(&final_segment(&samples[self.start..], opts))?;
            self.commit(opts, result, self.start);
        }
        Ok(Some(TranscriptionResult {
            text: self.text.trim().to_string(),
            tokens: std::mem::take(&mut self.pieces),
        }))
    }
}

/// Decode `samples` as DISJOINT windows cut at the quietest point of the
/// trailing 2.24 s, each window committing everything it decodes. Applies to
/// every length: a clip that fits in one window is decoded once.
/// `cancel` is checked before every window; `progress` fires after each commit.
pub fn transcribe_padded_window<F>(
    samples: &[f32],
    opts: &LongFormOptions,
    cancel: Option<&AtomicBool>,
    progress: &dyn Fn(&LongFormProgress),
    mut decode: F,
) -> Result<TranscriptionResult>
where
    F: FnMut(&[f32]) -> Result<TranscriptionResult>,
{
    let padding = ms_to_samples(PADDED_WINDOW_PADDING_MS);
    let target = opts.window_samples.saturating_sub(padding).max(1);
    let estimated = samples.len().saturating_sub(padding).div_ceil(target).max(1);
    let mut cursor = PaddedWindowCursor::new();
    let mut done = 0usize;
    loop {
        if cancel.is_some_and(|c| c.load(Ordering::SeqCst)) {
            return Err(Error::Cancelled);
        }
        let step = cursor.step(samples, opts, &mut decode)?;
        done += 1;
        progress(&LongFormProgress {
            windows_done: done,
            windows_estimated: estimated.max(done),
            samples_done: cursor.done_samples(samples.len()),
            samples_total: samples.len(),
        });
        if let Some(result) = step {
            return Ok(result);
        }
    }
}

// ---------------------------------------------------------------------------
// Words, cues and exports
// ---------------------------------------------------------------------------

/// Join sub-word pieces into words. Word ends come from the model's own TDT
/// durations with two clamps only: never shorter than one 80 ms frame, and
/// never past the next word's start. Real pauses are preserved.
pub fn timed_pieces_to_words(pieces: &[TimedToken]) -> Vec<TimedWord> {
    let mut words: Vec<TimedWord> = Vec::new();
    let mut pending_boundary = false;
    for piece in pieces {
        let starts_word =
            pending_boundary || piece.text.chars().next().is_some_and(char::is_whitespace);
        let clean = piece.text.trim();
        if clean.is_empty() {
            pending_boundary = starts_word;
            continue;
        }
        pending_boundary = false;
        if starts_word || words.is_empty() {
            words.push(TimedWord {
                text: clean.to_string(),
                start: piece.start,
                end: piece.end,
            });
        } else if let Some(last) = words.last_mut() {
            last.text.push_str(clean);
            last.end = piece.end;
        }
    }
    for i in 0..words.len() {
        let floor = words[i].start + 0.08;
        let mut end = words[i].end.max(floor);
        if let Some(next) = words.get(i + 1) {
            end = end.min(next.start.max(floor));
        }
        words[i].end = end;
    }
    words
}

/// Group words into subtitle cues. A cue closes on sentence-final punctuation
/// (after >= 3 words), after 12 words, after 7 s, or on a > 1 s gap.
pub fn group_cues(words: &[TimedWord]) -> Vec<TimedWord> {
    let mut cues = Vec::new();
    let mut current: Option<TimedWord> = None;
    let mut count = 0usize;
    for word in words {
        if let Some(cue) = current.as_mut() {
            let gap = word.start - cue.end;
            if gap > 1.0 || count >= 12 || cue.end - cue.start >= 7.0 {
                cues.push(cue.clone());
                current = Some(word.clone());
                count = 1;
            } else {
                cue.text.push(' ');
                cue.text.push_str(&word.text);
                cue.end = word.end;
                count += 1;
            }
        } else {
            current = Some(word.clone());
            count = 1;
        }
        if current
            .as_ref()
            .is_some_and(|c| c.text.ends_with(['.', '?', '!']) && count >= 3)
        {
            cues.push(current.take().expect("cue present"));
            count = 0;
        }
    }
    if let Some(cue) = current {
        cues.push(cue);
    }
    cues
}

fn subtitle_timestamp(seconds: f32, separator: char) -> String {
    let total_ms = (seconds.max(0.0) * 1_000.0).round() as u64;
    format!(
        "{:02}:{:02}:{:02}{}{:03}",
        total_ms / 3_600_000,
        (total_ms % 3_600_000) / 60_000,
        (total_ms % 60_000) / 1_000,
        separator,
        total_ms % 1_000
    )
}

/// `(srt, vtt)` text for the cues. Zero-width cues are widened to one frame.
pub fn render_subtitles(cues: &[TimedWord]) -> (String, String) {
    let mut srt = String::new();
    let mut vtt = String::from("WEBVTT\n\n");
    for (index, cue) in cues.iter().enumerate() {
        let end = cue.end.max(cue.start + 0.08);
        srt.push_str(&format!(
            "{}\n{} --> {}\n{}\n\n",
            index + 1,
            subtitle_timestamp(cue.start, ','),
            subtitle_timestamp(end, ','),
            cue.text.trim()
        ));
        vtt.push_str(&format!(
            "{} --> {}\n{}\n\n",
            subtitle_timestamp(cue.start, '.'),
            subtitle_timestamp(end, '.'),
            cue.text.trim()
        ));
    }
    (srt, vtt)
}

/// Timing JSON shared by every consumer: `segments` are the subtitle cues,
/// `words` the word-level timings (for seeking / playback highlight).
pub fn render_timing_json(cues: &[TimedWord], words: &[TimedWord]) -> Option<String> {
    let values = |items: &[TimedWord]| -> Vec<serde_json::Value> {
        items
            .iter()
            .map(|w| serde_json::json!({ "text": w.text, "start": w.start, "end": w.end }))
            .collect()
    };
    serde_json::to_string(&serde_json::json!({ "segments": values(cues), "words": values(words) }))
        .ok()
}

/// SRT, VTT and timing JSON from a decoded transcript. All `None` when there
/// are no timed pieces (never synthesizes timings).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TimedExports {
    pub srt: Option<String>,
    pub vtt: Option<String>,
    pub json: Option<String>,
}

pub fn timed_exports(pieces: &[TimedToken]) -> TimedExports {
    let words = timed_pieces_to_words(pieces);
    if words.is_empty() {
        return TimedExports::default();
    }
    let cues = group_cues(&words);
    let (srt, vtt) = render_subtitles(&cues);
    let nonempty = |v: String| (!v.trim().is_empty() && v.trim() != "WEBVTT").then_some(v);
    TimedExports {
        srt: nonempty(srt),
        vtt: nonempty(vtt),
        json: render_timing_json(&cues, &words),
    }
}
