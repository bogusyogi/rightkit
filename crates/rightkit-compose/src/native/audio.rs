//! Audio gain/fade descriptors derived from the render plan.
//!
//! The compositor is video-only; audio mixing stays with the engine's audio
//! path. These descriptors are the frame-synchronized mix contract a native
//! audio stage would consume — sample-exact under the shared rounding rule.

use serde::{Deserialize, Serialize};

use rightkit_timeline::plan::{Attenuation, LayerRole, RenderPlan};
use rightkit_timeline::{MilliDb, SourceId, Ticks};

/// Per-layer attenuation window, in output samples at the plan's mix rate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttenuationWindow {
    pub start_sample: i64,
    pub end_sample: i64,
    /// `None` = mute; `Some` = duck by this many millidecibels.
    pub duck: Option<MilliDb>,
}

/// One placed audio layer in output sample space.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioSegment {
    pub id: String,
    pub role: LayerRole,
    pub source: SourceId,
    /// Source trim start, in ticks (decoder-side, not resampled).
    pub source_in: Ticks,
    /// Placement in the output mix.
    pub start_sample: i64,
    pub duration_samples: i64,
    /// Linear gain factor (from millidecibels).
    pub gain_linear: f32,
    pub gain_db: MilliDb,
    pub windows: Vec<AttenuationWindow>,
}

/// The full audio mix contract for a snapshot: a silent base of `total_samples`
/// plus every layer. Frames and samples share the tick base, so video frame `f`
/// and samples `[f * sr / fps, (f+1) * sr / fps)` describe the same interval.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioMixDescriptor {
    pub sample_rate: i64,
    pub total_samples: i64,
    pub layers: Vec<AudioSegment>,
}

/// Derives the audio mix descriptor from a compiled plan.
pub fn audio_descriptor(plan: &RenderPlan) -> AudioMixDescriptor {
    let rate = plan.audio.sample_rate;
    let to_samples = |t: Ticks| t.to_units(rate);
    AudioMixDescriptor {
        sample_rate: rate,
        total_samples: to_samples(plan.audio.total),
        layers: plan
            .audio
            .layers
            .iter()
            .map(|layer| AudioSegment {
                id: layer.id.clone(),
                role: layer.role,
                source: plan.inputs[layer.input].source.clone(),
                source_in: layer.source_in,
                start_sample: to_samples(layer.start),
                duration_samples: to_samples(layer.duration),
                gain_linear: 10f32.powf(layer.gain.0 as f32 / 20_000.0),
                gain_db: layer.gain,
                windows: layer
                    .windows
                    .iter()
                    .map(|w| AttenuationWindow {
                        start_sample: to_samples(w.start),
                        end_sample: to_samples(w.end),
                        duck: match w.attenuation {
                            Attenuation::Duck { amount } => Some(amount),
                            Attenuation::Mute => None,
                        },
                    })
                    .collect(),
            })
            .collect(),
    }
}
