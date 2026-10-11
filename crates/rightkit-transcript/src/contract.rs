//! `rightkit.transcript.v1`: word-timed speech for one source.
//!
//! Times are integer milliseconds on the SOURCE timeline (0 = first audio
//! sample). Words must be sorted by `start_ms`, `start_ms < end_ms`, with no
//! overlaps; [`Transcript::validate`] enforces that so cut planning and
//! caption building can rely on it.
//!
//! # Schema ids
//!
//! | Id | Constant | Read | Written |
//! |---|---|---|---|
//! | `rightkit.transcript.v1` | [`SCHEMA`] | yes | by default (`from_words`) |
//! | `cutright.transcript.v1` | [`LEGACY_SCHEMA`] | yes | only via [`Transcript::to_json_with_schema_id`] |
//!
//! [`ACCEPTED_SCHEMAS`] lists exactly these ids; any other id is rejected on
//! read. A parsed transcript keeps the id it was read with. [`Transcript::validate`]
//! accepts both ids on purpose, so CutRight files stay readable.
//!
//! [`Transcript::from_json`] is lenient: unknown fields are ignored and absent
//! `source_blake3`, `language`, `vad` or `model` fields take their defaults.
//! [`Transcript::from_json_strict`] matches CutRight: it rejects unknown fields and
//! requires those fields (with a non-empty `source_blake3`), reporting
//! [`TranscriptError::MissingField`] by name.

use crate::words::TimedWord;
use serde::{Deserialize, Serialize};

pub const SCHEMA: &str = "rightkit.transcript.v1";
pub const LEGACY_SCHEMA: &str = "cutright.transcript.v1";

/// Schema ids this crate accepts on read. Also the only ids
/// [`Transcript::to_json_with_schema_id`] will write, so its output always
/// reads back. Keep in step with the table in the module docs.
pub const ACCEPTED_SCHEMAS: &[&str] = &[SCHEMA, LEGACY_SCHEMA];

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
    /// Present since 0.1.0. CutRight has no such variant and never produces it,
    /// so a CutRight reader rejects a transcript that contains it.
    Interpolated,
    /// Clamped to the media boundary by [`Transcript::constrain_duration`]
    /// (e.g. encoder padding past the registered duration). Serialises as
    /// `"source_boundary"`, the tag CutRight uses.
    SourceBoundary,
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
    /// A field CutRight requires is absent, or `source_blake3` is empty. Only
    /// [`Transcript::from_json_strict`] reports this; `from_json` defaults the field.
    #[error("required field {field:?} is missing or empty")]
    MissingField { field: &'static str },
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
        if !ACCEPTED_SCHEMAS.contains(&self.schema.as_str()) {
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

    /// Clamps the transcript to registered media of `duration_ms` without
    /// losing words (ported from CutRight's `constrain_duration`).
    ///
    /// The effective duration is the smaller of `duration_ms` and the current
    /// `duration_ms`, so it never grows. Each word keeps one millisecond per
    /// trailing word before the boundary, so earlier timings stay unchanged
    /// unless they overlap that reserved tail. Words whose timing changed are
    /// marked [`EndSource::SourceBoundary`]. VAD spans are clamped and empty
    /// ones dropped. Fails with `PastEnd { kind: "word", index: 0 }` (leaving
    /// the transcript untouched) when there is less room than words.
    pub fn constrain_duration(&mut self, duration_ms: u64) -> Result<(), TranscriptError> {
        self.validate()?;
        let cap = duration_ms.min(self.duration_ms);
        if cap < self.words.len() as u64 {
            return Err(TranscriptError::PastEnd {
                kind: "word",
                index: 0,
            });
        }
        // Invariant (validated input, cap >= word count): after processing the
        // word at index j, its start is >= j, so `end - 1` never underflows and
        // every word keeps `start < end`.
        let mut next_start = cap;
        for word in self.words.iter_mut().rev() {
            let end = word.end_ms.min(next_start);
            let start = word.start_ms.min(end - 1);
            if (start, end) != (word.start_ms, word.end_ms) {
                word.end_source = EndSource::SourceBoundary;
            }
            word.start_ms = start;
            word.end_ms = end;
            next_start = start;
        }
        for span in &mut self.vad {
            span.end_ms = span.end_ms.min(cap);
        }
        self.vad.retain(|s| s.start_ms < s.end_ms);
        self.duration_ms = cap;
        self.validate()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("transcript serialises")
    }

    /// Serialises with `schema_id` in place of `self.schema`, for consumers
    /// that keep emitting an older id. Only ids in [`ACCEPTED_SCHEMAS`] are
    /// accepted, so the output always reads back; any other id is
    /// `TranscriptError::Schema`. `self` is not modified.
    pub fn to_json_with_schema_id(&self, schema_id: &str) -> Result<String, TranscriptError> {
        if !ACCEPTED_SCHEMAS.contains(&schema_id) {
            return Err(TranscriptError::Schema(schema_id.to_string()));
        }
        let mut out = self.clone();
        out.schema = schema_id.to_string();
        Ok(out.to_json())
    }

    /// Parse and validate. Accepts every id in [`ACCEPTED_SCHEMAS`]. Unknown
    /// fields are ignored; use [`Transcript::from_json_strict`] to reject them.
    pub fn from_json(s: &str) -> Result<Self, TranscriptError> {
        let t: Transcript =
            serde_json::from_str(s).map_err(|e| TranscriptError::Json(e.to_string()))?;
        t.validate()?;
        Ok(t)
    }

    /// Parse and validate, rejecting any unknown field at every level (top
    /// level, words, VAD spans, model). Accepts the same ids and values as
    /// [`Transcript::from_json`].
    pub fn from_json_strict(s: &str) -> Result<Self, TranscriptError> {
        let doc: strict::Doc =
            serde_json::from_str(s).map_err(|e| TranscriptError::Json(e.to_string()))?;
        let t = doc.into_transcript()?;
        t.validate()?;
        Ok(t)
    }
}

/// Reader behind [`Transcript::from_json_strict`]. Mirrors the public types
/// field for field with `deny_unknown_fields` at each level. Fields that
/// CutRight requires are `Option` here, so a missing one is reported by name
/// instead of being defaulted. The `strict_mirror_matches_public_types` test
/// keeps the two in step.
mod strict {
    use super::{EndSource, ModelRef, Span, Transcript, TranscriptError, Word};
    use serde::Deserialize;

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    pub(super) struct Doc {
        schema: String,
        source_blake3: Option<String>,
        language: Option<String>,
        duration_ms: u64,
        words: Vec<WordDoc>,
        vad: Option<Vec<SpanDoc>>,
        model: Option<ModelDoc>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct WordDoc {
        text: String,
        start_ms: u64,
        end_ms: u64,
        #[serde(default)]
        confidence: Option<f32>,
        #[serde(default)]
        end_source: EndSource,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct SpanDoc {
        start_ms: u64,
        end_ms: u64,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ModelDoc {
        id: Option<String>,
        revision: Option<String>,
        backend: Option<String>,
    }

    impl Doc {
        pub(super) fn into_transcript(self) -> Result<Transcript, TranscriptError> {
            let missing = |field: &'static str| TranscriptError::MissingField { field };
            let source_blake3 = self.source_blake3.ok_or(missing("source_blake3"))?;
            if source_blake3.is_empty() {
                return Err(missing("source_blake3"));
            }
            let language = self.language.ok_or(missing("language"))?;
            let vad = self.vad.ok_or(missing("vad"))?;
            let model = self.model.ok_or(missing("model"))?;
            let model = ModelRef {
                id: model.id.ok_or(missing("model.id"))?,
                revision: model.revision.ok_or(missing("model.revision"))?,
                backend: model.backend.ok_or(missing("model.backend"))?,
            };
            Ok(Transcript {
                schema: self.schema,
                source_blake3,
                language,
                duration_ms: self.duration_ms,
                words: self
                    .words
                    .into_iter()
                    .map(|w| Word {
                        text: w.text,
                        start_ms: w.start_ms,
                        end_ms: w.end_ms,
                        confidence: w.confidence,
                        end_source: w.end_source,
                    })
                    .collect(),
                vad: vad
                    .into_iter()
                    .map(|s| Span {
                        start_ms: s.start_ms,
                        end_ms: s.end_ms,
                    })
                    .collect(),
                model,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(words: &[(u64, u64)]) -> Transcript {
        Transcript {
            schema: SCHEMA.into(),
            source_blake3: "0".repeat(64),
            language: "en".into(),
            duration_ms: 10_000,
            words: words
                .iter()
                .map(|&(s, e)| Word {
                    text: "w".into(),
                    start_ms: s,
                    end_ms: e,
                    confidence: None,
                    end_source: EndSource::Model,
                })
                .collect(),
            vad: vec![Span {
                start_ms: 0,
                end_ms: 5_000,
            }],
            model: ModelRef {
                id: "m".into(),
                revision: "r".into(),
                backend: "coreml".into(),
            },
        }
    }

    /// Like `t`, with no VAD and the given duration, for the constrain tests.
    fn tr(words: &[(u64, u64)], duration_ms: u64) -> Transcript {
        let mut x = t(words);
        x.vad.clear();
        x.duration_ms = duration_ms;
        x
    }

    fn spans(x: &Transcript) -> Vec<(u64, u64, EndSource)> {
        x.words
            .iter()
            .map(|w| (w.start_ms, w.end_ms, w.end_source))
            .collect()
    }

    #[test]
    fn accepts_sorted_and_rejects_overlap() {
        assert_eq!(t(&[(0, 100), (100, 200)]).validate(), Ok(()));
        assert_eq!(
            t(&[(0, 100), (90, 200)]).validate(),
            Err(TranscriptError::Unsorted { index: 1 })
        );
        assert_eq!(
            t(&[(5, 5)]).validate(),
            Err(TranscriptError::EmptyWord { index: 0 })
        );
    }

    #[test]
    fn json_round_trips() {
        let x = t(&[(0, 100)]);
        let s = serde_json::to_string(&x).unwrap();
        assert_eq!(serde_json::from_str::<Transcript>(&s).unwrap(), x);
    }

    #[test]
    fn constrain_within_duration_changes_nothing_and_never_grows() {
        let mut x = tr(&[(0, 400), (500, 900)], 2000);
        x.constrain_duration(5000).unwrap();
        assert_eq!(x.duration_ms, 2000);
        assert_eq!(
            spans(&x),
            vec![(0, 400, EndSource::Model), (500, 900, EndSource::Model)]
        );
    }

    #[test]
    fn overhanging_last_word_is_clamped_and_marked() {
        let mut x = tr(&[(0, 400), (500, 1200)], 2000);
        x.constrain_duration(1000).unwrap();
        assert_eq!(x.duration_ms, 1000);
        assert_eq!(
            spans(&x),
            vec![
                (0, 400, EndSource::Model),
                (500, 1000, EndSource::SourceBoundary)
            ]
        );
    }

    #[test]
    fn trailing_words_reserve_one_millisecond_each() {
        let mut x = tr(&[(0, 500), (500, 1000)], 1000);
        x.constrain_duration(500).unwrap();
        assert_eq!(
            spans(&x),
            vec![
                (0, 499, EndSource::SourceBoundary),
                (499, 500, EndSource::SourceBoundary)
            ]
        );
        assert!(x.validate().is_ok());
    }

    #[test]
    fn cap_below_word_count_is_rejected_without_mutation() {
        let mut x = tr(&[(0, 100), (100, 200)], 1000);
        let before = x.clone();
        assert_eq!(
            x.constrain_duration(1),
            Err(TranscriptError::PastEnd {
                kind: "word",
                index: 0
            })
        );
        assert_eq!(x, before);
    }

    #[test]
    fn invalid_input_is_rejected_before_mutation() {
        let mut x = tr(&[(0, 100), (90, 200)], 1000);
        let before = x.clone();
        assert_eq!(
            x.constrain_duration(500),
            Err(TranscriptError::Unsorted { index: 1 })
        );
        assert_eq!(x, before);
    }

    #[test]
    fn vad_spans_are_clamped_and_empty_ones_dropped() {
        let mut x = tr(&[(0, 100)], 1000);
        x.vad = vec![
            Span {
                start_ms: 0,
                end_ms: 300,
            },
            Span {
                start_ms: 700,
                end_ms: 900,
            },
        ];
        let mut kept = x.clone();
        kept.constrain_duration(800).unwrap();
        assert_eq!(
            kept.vad,
            vec![
                Span {
                    start_ms: 0,
                    end_ms: 300
                },
                Span {
                    start_ms: 700,
                    end_ms: 800
                }
            ]
        );
        x.constrain_duration(650).unwrap();
        assert_eq!(
            x.vad,
            vec![Span {
                start_ms: 0,
                end_ms: 300
            }]
        );
    }

    #[test]
    fn empty_transcript_and_zero_cap_are_fine() {
        let mut x = tr(&[], 1000);
        x.constrain_duration(0).unwrap();
        assert_eq!(x.duration_ms, 0);
        assert!(x.words.is_empty());
    }

    #[test]
    fn constrain_is_idempotent() {
        let mut x = tr(&[(0, 500), (500, 1000), (1000, 1500)], 2000);
        x.constrain_duration(1200).unwrap();
        let once = x.clone();
        x.constrain_duration(1200).unwrap();
        assert_eq!(x, once);
    }

    #[test]
    fn every_feasible_cap_keeps_all_words_valid() {
        let base = tr(&[(0, 300), (300, 301), (301, 900), (950, 1000)], 1000);
        for cap in 0..=1000u64 {
            let mut x = base.clone();
            match x.constrain_duration(cap) {
                Ok(()) => {
                    assert_eq!(x.words.len(), 4, "cap {cap} dropped words");
                    assert!(x.validate().is_ok(), "cap {cap} invalid");
                    assert!(x.words.iter().all(|w| w.end_ms <= cap));
                }
                Err(TranscriptError::PastEnd { kind: "word", .. }) => assert!(cap < 4),
                Err(e) => panic!("cap {cap}: unexpected {e:?}"),
            }
        }
    }

    #[test]
    fn source_boundary_serialises_as_cutright_tag() {
        assert_eq!(
            serde_json::to_string(&EndSource::SourceBoundary).unwrap(),
            "\"source_boundary\""
        );
        assert_eq!(
            serde_json::from_str::<EndSource>("\"source_boundary\"").unwrap(),
            EndSource::SourceBoundary
        );
    }
}
