//! Typed editing-style schema (#94 provenance, #93 measured style).
//!
//! Every style value carries its provenance: measured from footage (method and
//! BLAKE3 hash of the measured source), labelled by a model (model id), or
//! authored as a constant in this crate. The presets `tight`, `natural` and
//! `punchy` map onto the schema by reading [`CutConfig`]; this module does not
//! change any cut behaviour.

use rightkit_transcript::Transcript;
use serde::{Deserialize, Serialize};

use crate::cuts::config::{CutConfig, Preset};

#[path = "measure.rs"]
mod measure;

pub use measure::{MeasureError, Measurement};

/// Where a style value came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Provenance {
    /// Measured from footage. `source_hash` is the BLAKE3 hex digest of the
    /// measured source bytes.
    Measured { method: String, source_hash: String },
    /// Labelled by a model; `model_id` names the model that produced the label.
    ModelLabelled { model_id: String },
    /// Authored constant in this crate; `source` names where it is defined.
    Authored { source: String },
}

impl Provenance {
    /// Provenance for a value measured from `source` by `method`.
    pub fn measured(method: impl Into<String>, source: &[u8]) -> Self {
        Self::Measured {
            method: method.into(),
            source_hash: blake3::hash(source).to_hex().to_string(),
        }
    }

    /// Provenance for a value labelled by the model `model_id`.
    pub fn model_labelled(model_id: impl Into<String>) -> Self {
        Self::ModelLabelled {
            model_id: model_id.into(),
        }
    }

    /// Provenance for a constant authored in this crate.
    pub fn authored(source: impl Into<String>) -> Self {
        Self::Authored {
            source: source.into(),
        }
    }
}

/// A style value together with its provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StyleValue<T> {
    pub value: T,
    pub provenance: Provenance,
}

/// A time gap in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GapMs(pub i64);

/// Speaking pace in words per minute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Pace(pub u32);

/// How filler words are handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FillerPolicy {
    Keep,
    Remove,
}

/// A typed editing style. Gaps are in milliseconds, filler handling is a
/// [`FillerPolicy`], and pace is optional: no preset carries a pace value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditingStyle {
    pub preset: Preset,
    /// Longest pause kept between words.
    pub max_pause: StyleValue<GapMs>,
    /// Longest pause kept after a sentence-final word.
    pub sentence_pause: StyleValue<GapMs>,
    /// Minimum silence kept after the word before a cut.
    pub release_pad: StyleValue<GapMs>,
    /// Minimum silence kept before the word after a cut.
    pub attack_pad: StyleValue<GapMs>,
    /// Silence kept before the first kept word.
    pub lead_in: StyleValue<GapMs>,
    /// Silence kept after the last kept word.
    pub tail_out: StyleValue<GapMs>,
    /// Silence cuts shorter than this are not made.
    pub min_cut: StyleValue<GapMs>,
    /// Pause on each side that makes a context filler isolated.
    pub filler_isolation: StyleValue<GapMs>,
    pub fillers: StyleValue<FillerPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pace: Option<StyleValue<Pace>>,
}

fn gap(value: i64, source: &str) -> StyleValue<GapMs> {
    StyleValue {
        value: GapMs(value),
        provenance: Provenance::authored(source),
    }
}

impl EditingStyle {
    /// Maps a cut configuration onto the style schema. Every value is tagged
    /// `Authored`, naming the preset it came from.
    pub fn from_config(cfg: &CutConfig) -> Self {
        let name = match cfg.preset {
            Preset::Tight => "tight",
            Preset::Natural => "natural",
            Preset::Punchy => "punchy",
        };
        let source = format!("CutConfig::{name}");
        let src = source.as_str();
        let fillers = StyleValue {
            value: if cfg.remove_fillers {
                FillerPolicy::Remove
            } else {
                FillerPolicy::Keep
            },
            provenance: Provenance::authored(src),
        };
        Self {
            preset: cfg.preset,
            max_pause: gap(cfg.max_pause_ms, src),
            sentence_pause: gap(cfg.sentence_pause_ms, src),
            release_pad: gap(cfg.release_pad_ms, src),
            attack_pad: gap(cfg.attack_pad_ms, src),
            lead_in: gap(cfg.lead_in_ms, src),
            tail_out: gap(cfg.tail_out_ms, src),
            min_cut: gap(cfg.min_cut_ms, src),
            filler_isolation: gap(cfg.filler_isolation_ms, src),
            fillers,
            pace: None,
        }
    }

    /// The style for one of the three presets.
    pub fn from_preset(preset: Preset) -> Self {
        Self::from_config(&CutConfig::for_preset(preset))
    }

    /// Measures a style from a finished reference edit: `transcript` for one
    /// source and `kept_ms`, the reference edit's kept source ranges in ms.
    /// Measured values carry [`Provenance::Measured`] with the transcript's
    /// source BLAKE3. Unmeasured values keep the `base` preset's authored
    /// values. Returns the style and the raw distributions behind it.
    pub fn measure(
        transcript: &Transcript,
        kept_ms: &[(i64, i64)],
        base: Preset,
    ) -> Result<(Self, Measurement), MeasureError> {
        measure::measure(transcript, kept_ms, base)
    }
}
