//! Retime: a piecewise map from layer-local output time to source time.
//!
//! A video layer's `retime` segments are laid end to end from layer-local
//! output time zero. Each segment's output duration follows from its kind:
//!
//! - `Play` and `Reverse` cover `source_out - source_in` source ticks at
//!   `speed` source ticks per output tick, so they last `ceil(span / speed)`.
//! - `Hold` freezes `source_at` for its own `duration`.
//!
//! `FramePlay` uses frame-relative half-down rounding; signed holds preserve
//! pre-roll. Legacy output-to-source rounding stays floor-based. A `Play` at
//! output offset `o` reads `source_in + floor(o * speed)`, and a `Reverse`
//! reads `source_out - floor(o * speed)`.

use serde::{Deserialize, Serialize};

use crate::ticks::{Rational, SignedTicks, Ticks};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RetimeSegment {
    /// Frame-quantised unit-speed play. `start_frame` is this segment's
    /// absolute output frame, including any preceding segments. At frame `f`
    /// reads `source_in + fps.frame_to_ticks(f - start_frame)` using half-down
    /// rounding. Tick instants between starts hold that frame's source time.
    FramePlay {
        #[serde(rename = "source_in_ticks")]
        source_in: SignedTicks,
        fps: Rational,
        start_frame: i64,
        frames: i64,
    },
    /// Freeze a signed source time, including pre-roll before source zero.
    SignedHold {
        #[serde(rename = "source_at_ticks")]
        source_at: SignedTicks,
        #[serde(rename = "duration_ticks")]
        duration: Ticks,
    },
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
            RetimeSegment::FramePlay {
                fps,
                start_frame,
                frames,
                ..
            } => frame_span(fps, start_frame, frames).unwrap_or(Ticks::ZERO),
            RetimeSegment::SignedHold { duration, .. } => duration,
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
    fn source_at_offset(&self, offset: Ticks) -> Option<SignedTicks> {
        let step = (offset.0 as f64 * self.speed_or_one()).floor() as u64;
        Some(match *self {
            RetimeSegment::FramePlay {
                source_in,
                fps,
                start_frame,
                ..
            } => {
                let absolute = fps
                    .frame_to_ticks(start_frame)?
                    .0
                    .checked_add(i128::from(offset.0))?;
                let relative = fps
                    .frame_at_ticks(SignedTicks(absolute))?
                    .checked_sub(start_frame)?;
                SignedTicks(source_in.0.checked_add(fps.frame_to_ticks(relative)?.0)?)
            }
            RetimeSegment::SignedHold { source_at, .. } => source_at,
            RetimeSegment::Play {
                source_in,
                source_out,
                ..
            } => Ticks(source_in.0.saturating_add(step).min(source_out.0)).into(),
            RetimeSegment::Reverse {
                source_in,
                source_out,
                ..
            } => Ticks(source_out.0.saturating_sub(step).max(source_in.0)).into(),
            RetimeSegment::Hold { source_at, .. } => source_at.into(),
        })
    }

    fn speed_or_one(&self) -> f64 {
        match *self {
            RetimeSegment::Play { speed, .. } | RetimeSegment::Reverse { speed, .. } => speed,
            RetimeSegment::Hold { .. }
            | RetimeSegment::SignedHold { .. }
            | RetimeSegment::FramePlay { .. } => 0.0,
        }
    }
}

/// Source time at layer-local output time `output`, or `None` when `output`
/// lies past the end of the segments or no segment covers it. An empty slice
/// is handled by the evaluator as identity, not here. Negative signed source
/// values return `None`; use [`source_time_at_signed`] to preserve them.
pub fn source_time_at(segments: &[RetimeSegment], output: Ticks) -> Option<Ticks> {
    source_time_at_signed(segments, output).and_then(|t| Ticks::try_from(t).ok())
}

/// Signed source time at layer-local output time. Preserves negative pre-roll
/// & all unsigned legacy values. `None` outside coverage or on arithmetic
/// overflow. Empty lists are identity only in `evaluate`.
pub fn source_time_at_signed(segments: &[RetimeSegment], output: Ticks) -> Option<SignedTicks> {
    let mut start = Ticks::ZERO;
    for segment in segments {
        let length = segment.output_duration();
        if length.0 > 0 && output.0 >= start.0 && output.0 - start.0 < length.0 {
            return segment.source_at_offset(Ticks(output.0 - start.0));
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
pub(crate) fn check_retime(
    segments: &[RetimeSegment],
    layer_span: Ticks,
    layer_start: Ticks,
    output_fps: Rational,
) -> Result<(), String> {
    let mut output_start = layer_start;
    for (index, segment) in segments.iter().enumerate() {
        match *segment {
            RetimeSegment::FramePlay {
                source_in,
                fps,
                start_frame,
                frames,
            } => {
                if fps != output_fps
                    || fps.frame_to_ticks(start_frame) != Some(output_start.into())
                    || frame_span(fps, start_frame, frames).is_none()
                    || fps.frame_at_ticks(SignedTicks(0)).is_none()
                    || fps
                        .frame_to_ticks(frames - 1)
                        .and_then(|last| source_in.0.checked_add(last.0))
                        .is_none()
                {
                    return Err(format!(
                        "retime segment {index} has invalid frame timing or source overflow"
                    ));
                }
            }
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
            RetimeSegment::Hold { duration, .. } | RetimeSegment::SignedHold { duration, .. } => {
                if duration == Ticks::ZERO {
                    return Err(format!("retime segment {index} is an empty hold"));
                }
            }
        }
        output_start = output_start.saturating_add(segment.output_duration());
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

fn frame_span(fps: Rational, start: i64, frames: i64) -> Option<Ticks> {
    if frames <= 0 {
        return None;
    }
    let end = fps.frame_to_ticks(start.checked_add(frames)?)?.0;
    let start = fps.frame_to_ticks(start)?.0;
    let span = Ticks::try_from(SignedTicks(end - start)).ok()?;
    (span > Ticks::ZERO).then_some(span)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_whole_tick_play_uses_relative_frame_rounding() {
        let fps = Rational::new(11, 1);
        let segment = RetimeSegment::FramePlay {
            source_in: SignedTicks(-705_600_000),
            fps,
            start_frame: 1,
            frames: 4,
        };
        let origin = fps.frame_to_ticks(1).unwrap().0;
        let local =
            Ticks::try_from(SignedTicks(fps.frame_to_ticks(2).unwrap().0 - origin)).unwrap();
        // ft(2) - ft(1) is 64,145,454; ft(1) is 64,145,455.
        assert_eq!(local, Ticks(64_145_454));
        assert_eq!(
            source_time_at_signed(&[segment], local),
            Some(SignedTicks(-641_454_545))
        );
        assert_eq!(source_time_at(&[segment], local), None);
        assert_eq!(
            source_time_at_signed(&[segment], segment.output_duration()),
            None
        );
        assert_eq!(
            source_time_at_signed(&[segment], Ticks(local.0 - 1)),
            Some(SignedTicks(-705_600_000))
        );
        assert_eq!(
            source_time_at_signed(&[segment], Ticks(local.0 + 1)),
            Some(SignedTicks(-641_454_545))
        );
    }

    #[test]
    fn signed_hold_and_legacy_u64_max_are_lossless() {
        let signed = RetimeSegment::SignedHold {
            source_at: SignedTicks(-17),
            duration: Ticks(4),
        };
        assert_eq!(
            source_time_at_signed(&[signed], Ticks(3)),
            Some(SignedTicks(-17))
        );
        assert_eq!(source_time_at_signed(&[signed], Ticks(4)), None);
        let unsigned = RetimeSegment::Hold {
            source_at: Ticks::MAX,
            duration: Ticks(4),
        };
        assert_eq!(
            source_time_at_signed(&[unsigned], Ticks(3)),
            Some(Ticks::MAX.into())
        );
        assert_eq!(source_time_at(&[unsigned], Ticks(3)), Some(Ticks::MAX));
    }
}
