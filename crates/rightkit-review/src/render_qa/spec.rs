//! QA thresholds per preset (data: `data/qa-presets.json`) and what a given
//! render is expected to be (duration, frame rate, captions).

use rightkit_timeline::{Fps, Preset, Ticks, Timeline};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::render_qa::error::QaError;
use rightkit_fs::digest::blake3_bytes;

const PRESETS_JSON: &str = include_str!("../../data/render_qa/qa-presets.json");

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptionRules {
    /// Fail when the render has no captions to check.
    pub required: bool,
    pub max_lines: usize,
    /// Safe-zone width in characters (caption text must fit the burn-in box).
    pub max_chars_per_line: usize,
    /// Reading speed above this is a warning.
    pub max_chars_per_second: f64,
    /// Warn when the last cue ends this long before the render does.
    pub max_tail_gap_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QaSpec {
    /// Preset id (`youtube`, `shorts`, `reels`, `tiktok`).
    #[serde(default)]
    pub preset: String,
    pub width: u32,
    pub height: u32,
    pub video_codecs: Vec<String>,
    pub pixel_formats: Vec<String>,
    pub audio_codecs: Vec<String>,
    pub require_audio: bool,
    pub duration_tolerance_ms: i64,
    pub fps_tolerance: f64,
    /// Black intervals shorter than this are not reported.
    pub black_min_ms: i64,
    /// Longest tolerated black interval outside `QaExpect::allowed_black`.
    pub max_black_ms: i64,
    pub black_pix_threshold: f64,
    pub freeze_min_ms: i64,
    pub max_freeze_ms: i64,
    pub freeze_noise_db: f64,
    pub silence_noise_db: f64,
    pub silence_min_ms: i64,
    pub max_silence_ms: i64,
    pub loudness_target_lufs: f64,
    pub loudness_tolerance_lu: f64,
    pub true_peak_ceiling_dbtp: f64,
    /// Magnitude (0..1) at or above which a sample counts as full scale.
    pub clip_threshold: f64,
    /// Consecutive full-scale samples in one channel that make a clipped run.
    pub clip_min_run: u32,
    pub max_clipped_runs: u64,
    pub max_av_drift_ms: i64,
    pub max_av_start_offset_ms: i64,
    pub captions: CaptionRules,
}

fn merge(base: &mut Value, over: &Value) {
    match (base, over) {
        (Value::Object(b), Value::Object(o)) => {
            for (k, v) in o {
                match b.get_mut(k) {
                    Some(existing) => merge(existing, v),
                    None => {
                        b.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (b, o) => *b = o.clone(),
    }
}

impl QaSpec {
    pub fn preset_names() -> Vec<String> {
        let doc: Value = serde_json::from_str(PRESETS_JSON).expect("embedded presets parse");
        doc["presets"]
            .as_object()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Thresholds for a named preset.
    pub fn preset(name: &str) -> Result<Self, QaError> {
        let doc: Value = serde_json::from_str(PRESETS_JSON).expect("embedded presets parse");
        let over = doc["presets"]
            .get(name)
            .ok_or_else(|| QaError::UnknownPreset(name.into()))?;
        let mut merged = doc["defaults"].clone();
        merge(&mut merged, over);
        let mut spec: QaSpec = serde_json::from_value(merged)
            .map_err(|e| QaError::BadInput(format!("preset {name}: {e}")))?;
        spec.preset = name.into();
        Ok(spec)
    }

    /// Identifier of the QA policy these thresholds belong to: the preset id.
    pub fn policy_id(&self) -> String {
        self.preset.clone()
    }

    /// BLAKE3 of the canonical JSON of every threshold. A receipt records it so a package
    /// verifies against the policy it was qualified under, not whatever the preset is now.
    pub fn policy_blake3(&self) -> String {
        blake3_bytes(&serde_json::to_vec(self).expect("QA spec serialises"))
    }

    /// Thresholds for the timeline crate's render preset (by name; a preset the
    /// QA data does not know is an error rather than a silent default).
    pub fn for_timeline_preset(preset: Preset) -> Result<Self, QaError> {
        Self::preset(preset.name())
    }
}

/// One caption cue on the output timeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QaCue {
    pub start_ms: i64,
    pub end_ms: i64,
    pub lines: Vec<String>,
}

fn srt_time(v: &str) -> Option<i64> {
    let (hms, ms) = v.trim().split_once([',', '.'])?;
    let mut p = hms.split(':');
    let h: i64 = p.next()?.trim().parse().ok()?;
    let m: i64 = p.next()?.trim().parse().ok()?;
    let s: i64 = p.next()?.trim().parse().ok()?;
    let ms: i64 = ms.trim().parse().ok()?;
    Some(((h * 60 + m) * 60 + s) * 1000 + ms)
}

/// Parses SRT text. Cues are returned in file order (QA checks the ordering).
pub fn parse_srt(text: &str) -> Result<Vec<QaCue>, QaError> {
    let text = text.replace("\r\n", "\n");
    let mut cues = Vec::new();
    for block in text.split("\n\n") {
        let mut lines = block.trim_matches('\n').lines().peekable();
        if lines.peek().is_none() {
            continue;
        }
        let mut first = lines.next().unwrap_or_default();
        if !first.contains("-->") {
            first = lines.next().ok_or_else(|| {
                QaError::BadInput(format!("SRT block without a timing line: {block:?}"))
            })?;
        }
        let (a, b) = first.split_once("-->").ok_or_else(|| {
            QaError::BadInput(format!("SRT block without a timing line: {block:?}"))
        })?;
        let bad = || QaError::BadInput(format!("SRT timing line unreadable: {first:?}"));
        cues.push(QaCue {
            start_ms: srt_time(a).ok_or_else(bad)?,
            end_ms: srt_time(b).ok_or_else(bad)?,
            lines: lines.map(str::to_string).collect(),
        });
    }
    Ok(cues)
}

/// What the render under test is supposed to be.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct QaExpect {
    /// Timeline duration in milliseconds.
    pub duration_ms: Option<i64>,
    pub fps: Option<Fps>,
    /// SRT text burned into / shipped with the render, if any.
    pub captions_srt: Option<String>,
    /// Output-time ranges where silence is intended.
    pub allowed_silence: Vec<(i64, i64)>,
    /// Output-time ranges where black is intended (fade in/out, title card).
    pub allowed_black: Vec<(i64, i64)>,
    /// Output-time ranges where a still frame is intended.
    pub allowed_freeze: Vec<(i64, i64)>,
}

impl QaExpect {
    /// Duration and frame rate from the timeline revision the render was made from.
    pub fn from_timeline(tl: &Timeline) -> Self {
        let ticks = tl.fps.frame_to_ticks(tl.total_frames());
        Self {
            duration_ms: Some(ticks_to_ms(ticks)),
            fps: Some(tl.fps),
            ..Self::default()
        }
    }
}

fn ticks_to_ms(t: Ticks) -> i64 {
    t.to_units(1000)
}
