#[allow(unused_imports)]
use super::*;
#[allow(unused_imports)]
use super::{
    capture_recovery::*, probe_decisions::*, unified_probe_lane::*, worker_api::*, worker_clock::*,
    worker_main::*,
};

pub(crate) trait TranscriptText {
    fn transcript_text(&self) -> &str;
    fn empty() -> Self;
}

impl TranscriptText for String {
    fn transcript_text(&self) -> &str {
        self
    }
    fn empty() -> Self {
        String::new()
    }
}

impl TranscriptText for FileTranscript {
    fn transcript_text(&self) -> &str {
        &self.text
    }
    fn empty() -> Self {
        FileTranscript {
            text: String::new(),
            srt: String::new(),
            vtt: String::new(),
            words: Vec::new(),
        }
    }
}

impl TranscriptText for TranscriptionResult {
    fn transcript_text(&self) -> &str {
        &self.text
    }
    fn empty() -> Self {
        TranscriptionResult {
            text: String::new(),
            tokens: Vec::new(),
        }
    }
}

pub(crate) fn chunk_level(chunk: &[f32]) -> f32 {
    if chunk.is_empty() {
        return 0.0;
    }
    let sum_sq: f32 = chunk.iter().map(|s| s * s).sum();
    let rms = (sum_sq / chunk.len() as f32).sqrt();
    (rms.sqrt() * 1.8).clamp(0.0, 1.0)
}
