//! Music ducking under dialogue, two typed forms:
//! * `Sidechain`: `sidechaincompress` keyed on the dialogue bus (follows the audio).
//! * `Spans`: `volume` automation from VAD speech spans with smooth ramps (deterministic,
//!   follows the transcript/VAD; ported and extended from `audio_finish::ducking_filter`,
//!   which used hard `between()` switches).

use serde::{Deserialize, Serialize};

use crate::error::AudioError;
use crate::filter::{db_to_linear, num, Filter};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum Ducking {
    Sidechain {
        threshold_db: f64,
        ratio: f64,
        attack_ms: f64,
        release_ms: f64,
        #[serde(default)]
        makeup_db: f64,
    },
    Spans {
        /// Attenuation under speech, negative dB.
        duck_db: f64,
        /// Ramp length on each side of a span.
        fade_ms: i64,
    },
}

fn range(name: &str, v: f64, lo: f64, hi: f64) -> Result<(), AudioError> {
    if v.is_finite() && v >= lo && v <= hi {
        Ok(())
    } else {
        Err(AudioError::Invalid(format!(
            "{name} = {v} outside {lo}..={hi}"
        )))
    }
}

impl Ducking {
    pub fn validate(&self) -> Result<(), AudioError> {
        match self {
            Ducking::Sidechain {
                threshold_db,
                ratio,
                attack_ms,
                release_ms,
                makeup_db,
            } => {
                range("ducking.threshold_db", *threshold_db, -60.0, 0.0)?;
                range("ducking.ratio", *ratio, 1.0, 20.0)?;
                range("ducking.attack_ms", *attack_ms, 0.01, 2000.0)?;
                range("ducking.release_ms", *release_ms, 0.01, 9000.0)?;
                range("ducking.makeup_db", *makeup_db, 0.0, 36.0)
            }
            Ducking::Spans { duck_db, fade_ms } => {
                range("ducking.duck_db", *duck_db, -60.0, 0.0)?;
                range("ducking.fade_ms", *fade_ms as f64, 1.0, 5000.0)
            }
        }
    }

    /// Fragment that ducks `music_in` and writes `out`.
    /// * `Sidechain`: `key_in` is a dialogue copy (see [`sidechain_split_fragment`]).
    /// * `Spans`: `key_in` is ignored; `speech_spans_ms` are output-timeline `(start,end)`.
    pub fn fragment(
        &self,
        music_in: &str,
        key_in: Option<&str>,
        speech_spans_ms: &[(i64, i64)],
        out: &str,
    ) -> Result<String, AudioError> {
        self.validate()?;
        match self {
            Ducking::Sidechain {
                threshold_db,
                ratio,
                attack_ms,
                release_ms,
                makeup_db,
            } => {
                let key = key_in.ok_or_else(|| {
                    AudioError::Invalid("sidechain ducking needs a dialogue key label".into())
                })?;
                let f = Filter::new("sidechaincompress")
                    .num(
                        "threshold",
                        db_to_linear(*threshold_db).clamp(0.000_976_56, 1.0),
                    )
                    .num("ratio", *ratio)
                    .num("attack", *attack_ms)
                    .num("release", *release_ms)
                    .num("makeup", db_to_linear(*makeup_db).clamp(1.0, 64.0));
                Ok(format!("[{music_in}][{key}]{}[{out}]", f.render()))
            }
            Ducking::Spans { duck_db, fade_ms } => {
                let f = spans_volume_filter(speech_spans_ms, *duck_db, *fade_ms);
                Ok(format!("[{music_in}]{}[{out}]", f.render()))
            }
        }
    }
}

/// `[dialogue_in]asplit=2[main_out][key_out]`: a label can feed only one consumer, so the
/// dialogue bus is split into the programme copy and the sidechain key.
pub fn sidechain_split_fragment(dialogue_in: &str, main_out: &str, key_out: &str) -> String {
    format!("[{dialogue_in}]asplit=2[{main_out}][{key_out}]")
}

/// Merge spans whose ramp zones would overlap so the per-span envelopes are disjoint and
/// can be summed.
fn merge_spans(spans: &[(i64, i64)], fade_ms: i64) -> Vec<(i64, i64)> {
    let mut v: Vec<(i64, i64)> = spans.iter().copied().filter(|(s, e)| e > s).collect();
    v.sort_unstable();
    let mut out: Vec<(i64, i64)> = Vec::new();
    for (s, e) in v {
        match out.last_mut() {
            Some(last) if s <= last.1 + 2 * fade_ms => last.1 = last.1.max(e),
            _ => out.push((s, e)),
        }
    }
    out
}

/// Per-frame `volume` automation: `1 - (1-k) * sum(env_i)` with trapezoid envelopes.
pub fn spans_volume_filter(spans: &[(i64, i64)], duck_db: f64, fade_ms: i64) -> Filter {
    let spans = merge_spans(spans, fade_ms);
    if spans.is_empty() {
        return Filter::new("volume").opt("volume", "1");
    }
    let k = db_to_linear(duck_db);
    let f = fade_ms.max(1) as f64 / 1000.0;
    let envs: Vec<String> = spans
        .iter()
        .map(|(s, e)| {
            let (a, b) = (*s as f64 / 1000.0, *e as f64 / 1000.0);
            format!(
                "clip(min((t-{})/{},({}-t)/{}),0,1)",
                num(a - f),
                num(f),
                num(b + f),
                num(f)
            )
        })
        .collect();
    let expr = format!("1-{}*({})", num(1.0 - k), envs.join("+"));
    Filter::new("volume")
        .quoted("volume", &expr)
        .opt("eval", "frame")
}
