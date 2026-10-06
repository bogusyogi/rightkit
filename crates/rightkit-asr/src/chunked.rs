//! VAD-chunked decoding for any [`Transcriber`]: decode each speech span on
//! its own and re-time the result onto the source timeline. Span production
//! (thresholds, padding, chunk size) is `rightkit-vad`'s job; this only
//! applies spans to a decoder.
use crate::decoder::{TimedToken, TranscriptionResult};
use crate::error::Result;
use crate::timestamps::TimestampMode;
use crate::transcriber::Transcriber;
use rightkit_vad::Span;

#[derive(Debug, Clone)]
pub struct ChunkResult {
    pub span: Span,
    /// Token times are seconds on the source timeline (span start added).
    pub result: TranscriptionResult,
}

/// Decode `spans` of 16 kHz mono `audio`. Empty spans are skipped.
pub fn transcribe_spans<T: Transcriber>(
    transcriber: &mut T,
    audio: &[f32],
    spans: &[Span],
    mode: Option<TimestampMode>,
) -> Result<Vec<ChunkResult>> {
    let mut out = Vec::with_capacity(spans.len());
    for &span in spans {
        let range = span.sample_range(audio.len());
        if range.is_empty() {
            continue;
        }
        let mut result = transcriber.transcribe_samples(audio[range].to_vec(), 16_000, 1, mode)?;
        let offset = span.start_ms as f32 / 1000.0;
        result.tokens = result
            .tokens
            .into_iter()
            .map(|t| TimedToken { text: t.text, start: t.start + offset, end: t.end + offset })
            .collect();
        out.push(ChunkResult { span, result });
    }
    Ok(out)
}
