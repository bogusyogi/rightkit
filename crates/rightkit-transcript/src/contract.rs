//! `rightkit.transcript.v1`: word-timed speech for one source.
//!
//! Times are integer milliseconds on the SOURCE timeline (0 = first audio
//! sample). Words must be sorted by `start_ms`, `start_ms < end_ms`, with no
//! overlaps; [`Transcript::validate`] enforces that so cut planning and
//! caption building can rely on it. The CutRight `cutright.transcript.v1`
//! schema (same field names) is accepted on read.

use crate::words::TimedWord;
use serde::{Deserialize, Serialize};

pub const SCHEMA: &str = "rightkit.transcript.v1";
pub const LEGACY_SCHEMA: &str = "cutright.transcript.v1";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Transcript {
    pub schema: String,
    /// BLAKE3 (or other stable id) of the source this transcript describes.
    /// Empty when the producer has no registered source (SRT import, TTS).
    #[serde(default)]
    pub source_blake3: String,
    /// BCP-47 language tag, e.g. "en".
    #[serde(default)]
    pub language: String,
    /// Source audio duration in milliseconds.
    pub duration_ms: u64,
    pub words: Vec<Word>,
    /// Voice-activity segments (speech present), sorted, non-overlapping.
    #[serde(default)]
    pub vad: Vec<Span>,
    #[serde(default)]
    pub model: ModelRef,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Word {
    pub text: String,
    pub start_ms: u64,
    pub end_ms: u64,
    /// 0.0..=1.0 when the recogniser reports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    #[serde(default)]
    pub end_source: EndSource,
}

/// How a word's timing was derived.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndSource {
    #[default]
    Model,
    /// Engine reported only token starts; the end is the next start (approximate).
    NextTokenStart,
    Vad,
    /// Spread proportionally across a cue/segment (SRT/VTT import, TTS manifest).
    Interpolated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub start_ms: u64,
    pub end_ms: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRef {
    /// Shared-store model id, e.g. "parakeet-tdt-0.6b-v3-coreml".
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub revision: String,
    /// "coreml" | "onnx-directml" | "onnx-cpu" | "srt-import" | ...
    #[serde(default)]
    pub backend: String,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TranscriptError {
    #[error("unsupported schema {0:?}")]
    Schema(String),
    #[error("word {index} has start_ms >= end_ms")]
    EmptyWord { index: usize },
    #[error("word {index} overlaps or precedes the previous word")]
    Unsorted { index: usize },
    #[error("{kind} {index} ends after the source duration")]
    PastEnd { kind: &'static str, index: usize },
    #[error("vad span {index} is empty, unsorted or overlapping")]
    BadVad { index: usize },
    #[error("invalid transcript JSON: {0}")]
    Json(String),
}

impl Transcript {
    /// Build from timed words; `duration_ms` is the last word end unless given.
    pub fn from_words(
        words: &[TimedWord],
        language: &str,
        duration_ms: Option<u64>,
        model: ModelRef,
        end_source: EndSource,
    ) -> Self {
        let words: Vec<Word> = crate::segment::normalize_words(words)
            .into_iter()
            .map(|w| Word {
                text: w.text,
                start_ms: w.start_ms,
                end_ms: w.end_ms,
                confidence: None,
                end_source,
            })
            .collect();
        let end = words.last().map(|w| w.end_ms).unwrap_or(0);
        Transcript {
            schema: SCHEMA.into(),
            source_blake3: String::new(),
            language: language.into(),
            duration_ms: duration_ms.unwrap_or(end).max(end),
            words,
            vad: Vec::new(),
            model,
        }
    }

    pub fn timed_words(&self) -> Vec<TimedWord> {
        self.words
            .iter()
            .map(|w| TimedWord::new(w.text.clone(), w.start_ms, w.end_ms))
            .collect()
    }

    /// Plain transcript text (words joined by one space).
    pub fn text(&self) -> String {
        self.words
            .iter()
            .map(|w| w.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }

    pub fn validate(&self) -> Result<(), TranscriptError> {
        if self.schema != SCHEMA && self.schema != LEGACY_SCHEMA {
            return Err(TranscriptError::Schema(self.schema.clone()));
        }
        let mut prev_end = 0;
        for (index, w) in self.words.iter().enumerate() {
            if w.start_ms >= w.end_ms {
                return Err(TranscriptError::EmptyWord { index });
            }
            if w.start_ms < prev_end {
                return Err(TranscriptError::Unsorted { index });
            }
            if w.end_ms > self.duration_ms {
                return Err(TranscriptError::PastEnd {
                    kind: "word",
                    index,
                });
            }
            prev_end = w.end_ms;
        }
        let mut prev_end = 0;
        for (index, s) in self.vad.iter().enumerate() {
            if s.start_ms >= s.end_ms || s.start_ms < prev_end {
                return Err(TranscriptError::BadVad { index });
            }
            if s.end_ms > self.duration_ms {
                return Err(TranscriptError::PastEnd {
                    kind: "vad span",
                    index,
                });
            }
            prev_end = s.end_ms;
        }
        Ok(())
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("transcript serialises")
    }

    /// Parse and validate. Accepts the legacy CutRight schema id.
    pub fn from_json(s: &str) -> Result<Self, TranscriptError> {
        let t: Transcript =
            serde_json::from_str(s).map_err(|e| TranscriptError::Json(e.to_string()))?;
        t.validate()?;
        Ok(t)
    }
}
