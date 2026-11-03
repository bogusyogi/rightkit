//! SRT and WebVTT rendering for file transcription, shared by macOS (CoreML)
//! and Windows (ONNX). Cue grouping and export come from `rightkit-transcript`:
//! `GroupRules::default()` closes a cue at 12 words, 7 s span, a >1 s gap, or
//! sentence-final punctuation after 3 words, with an 80 ms cue floor.

use rightkit_transcript::{group_words, to_srt, to_vtt, GroupRules, Newline, TimedWord};

/// Renders `(text, start_seconds, end_seconds)` words to `(srt, vtt)`.
pub fn render_srt_vtt(words: impl IntoIterator<Item = (String, f32, f32)>) -> (String, String) {
    let timed: Vec<TimedWord> = words
        .into_iter()
        .map(|(text, start, end)| TimedWord::from_seconds(text, start, end))
        .collect();
    let cues = group_words(&timed, &GroupRules::default());
    (to_srt(&cues, Newline::Lf), to_vtt(&cues, Newline::Lf))
}
