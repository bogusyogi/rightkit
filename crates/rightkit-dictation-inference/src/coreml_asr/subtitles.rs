// SRT/WebVTT rendering for CoreML file transcription word timings. The shared
// implementation (cue grouping and export) lives in `crate::subtitle_export`.
use super::model::TimedTok;

/// Group word-level [`TimedTok`]s into subtitle cues and render SRT + WebVTT.
pub fn render_subtitles(words: &[TimedTok]) -> (String, String) {
    crate::subtitle_export::render_srt_vtt(words.iter().map(|w| (w.text.clone(), w.start, w.end)))
}
