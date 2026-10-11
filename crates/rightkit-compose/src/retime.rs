//! Retime: a piecewise map from layer-local output time to source time.
//!
//! A video layer's `retime` segments are laid end to end from layer-local
//! output time zero. Each segment's output duration follows from its kind:
//!
//! - `Play` and `Reverse` cover `source_out - source_in` source ticks at
//!   `speed` source ticks per output tick, so they last `ceil(span / speed)`.
//! - `Hold` freezes `source_at` for its own `duration`.
//!
//! Output-to-source rounding is floor-based and deterministic. A `Play` at
//! output offset `o` reads `source_in + floor(o * speed)`, and a `Reverse`
//! reads `source_out - floor(o * speed)`.

use serde::{Deserialize, Serialize};

use crate::ticks::Ticks;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RetimeSegment {
    /// Forward playback of `source_in..source_out` at `speed` (> 0).
    Play {
        #[serde(rename = "source_in_ticks")]
        source_in: Ticks,
        #[serde(rename = "source_out_ticks")]
        source_out: Ticks,
        speed: f64,
    },
    /// Holds the frame at `source_at` for `duration`.
    Hold {
        #[serde(rename = "source_at_ticks")]
        source_at: Ticks,
        #[serde(rename = "duration_ticks")]
        duration: Ticks,
    },
    /// Backward playback from `source_out` down to `source_in` at `speed` (> 0).
    Reverse {
        #[serde(rename = "source_in_ticks")]
        source_in: Ticks,
        #[serde(rename = "source_out_ticks")]
        source_out: Ticks,
        speed: f64,
    },
}

impl RetimeSegment {
    /// Forward playback from source zero, open-ended in the source. Used for
    /// constant-speed layers: the layer's own timing bounds the output. A
    /// speed of zero or less fails validation.
    pub fn constant(speed: f64) -> RetimeSegment {
        RetimeSegment::Play {
            source_in: Ticks::ZERO,
            source_out: Ticks::MAX,
            speed,
        }
    }

    /// Output duration in ticks. `Play` and `Reverse` return
    /// `ceil(span / speed)`, saturating at `Ticks::MAX`; `Hold` returns its
    /// `duration`. Not meaningful for invalid speeds; validation rejects those.
    pub fn output_duration(&self) -> Ticks {
        match *self {
            RetimeSegment::Play {
                source_in,
                source_out,
                speed,
            }
            | RetimeSegment::Reverse {
                source_in,
                source_out,
                speed,
            } => {
                let span = source_out.0.saturating_sub(source_in.0) as f64;
                Ticks((span / speed).ceil() as u64)
            }
            RetimeSegment::Hold { duration, .. } => duration,
        }
    }

    /// Source time at `offset` ticks into this segment's output range. Only
    /// meaningful for `offset < output_duration()`.
    fn source_at_offset(&self, offset: Ticks) -> Ticks {
        let step = (offset.0 as f64 * self.speed_or_one()).floor() as u64;
        match *self {
            RetimeSegment::Play {
                source_in,
                source_out,
                ..
            } => Ticks(source_in.0.saturating_add(step).min(source_out.0)),
            RetimeSegment::Reverse {
                source_in,
                source_out,
                ..
            } => Ticks(source_out.0.saturating_sub(step).max(source_in.0)),
            RetimeSegment::Hold { source_at, .. } => source_at,
        }
    }

    fn speed_or_one(&self) -> f64 {
        match *self {
            RetimeSegment::Play { speed, .. } | RetimeSegment::Reverse { speed, .. } => speed,
            RetimeSegment::Hold { .. } => 0.0,
        }
    }
}

/// Source time at layer-local output time `output`, or `None` when `output`
/// lies past the end of the segments or no segment covers it. An empty slice
/// is handled by the evaluator as identity, not here.
pub fn source_time_at(segments: &[RetimeSegment], output: Ticks) -> Option<Ticks> {
    let mut start = Ticks::ZERO;
    for segment in segments {
        let length = segment.output_duration();
        if length.0 > 0 && output.0 >= start.0 && output.0 - start.0 < length.0 {
            return Some(segment.source_at_offset(Ticks(output.0 - start.0)));
        }
        start = start.saturating_add(length);
    }
    None
}

/// Total output duration of the segments, saturating at `Ticks::MAX`.
pub fn output_span(segments: &[RetimeSegment]) -> Ticks {
    segments.iter().fold(Ticks::ZERO, |total, s| {
        total.saturating_add(s.output_duration())
    })
}

/// Structural checks for a retime list against a layer of `layer_span`.
/// Returns a reason on failure.
pub(crate) fn check_retime(segments: &[RetimeSegment], layer_span: Ticks) -> Result<(), String> {
    for (index, segment) in segments.iter().enumerate() {
        match *segment {
            RetimeSegment::Play {
                source_in,
                source_out,
                speed,
            }
            | RetimeSegment::Reverse {
                source_in,
                source_out,
                speed,
            } => {
                if !speed.is_finite() || speed <= 0.0 {
                    return Err(format!("retime segment {index} needs a finite speed > 0"));
                }
                if source_in >= source_out {
                    return Err(format!(
                        "retime segment {index} is empty: source_in must be below source_out"
                    ));
                }
            }
            RetimeSegment::Hold { duration, .. } => {
                if duration == Ticks::ZERO {
                    return Err(format!("retime segment {index} is an empty hold"));
                }
            }
        }
        if segment.output_duration() == Ticks::ZERO {
            return Err(format!("retime segment {index} has no output duration"));
        }
    }
    // An empty list is identity (the evaluator's `video_source`), so there is no span to cover.
    if !segments.is_empty() && output_span(segments) < layer_span {
        return Err(format!(
            "retime covers less output time than the layer's {layer_span}"
        ));
    }
    Ok(())
}
