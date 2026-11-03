//! Pure delivery checks over caller-produced decode, probe & caption data.
//! No decoder or process runs here. Missing required measurements fail; optional
//! stream/caption applicability is explicit. Invalid policy returns [`ReviewError`].

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::qa::{Check, Severity};
use crate::ReviewError;

/// Version of these checks. Consumers include this in their receipt/check-code
/// binding & bump their own check version when adopting changed semantics.
pub const DELIVERY_CHECK_VERSION: u32 = 1;

pub mod rule_ids {
    pub const INTEGRITY: &str = "file.integrity";
    pub const STREAMS: &str = "media.streams";
    pub const DURATION: &str = "media.duration";
    pub const FRAME_RATE: &str = "video.frame_rate";
    pub const VIDEO_CODEC: &str = "video.codec";
    pub const AUDIO_CODEC: &str = "audio.codec";
    pub const PIXEL_FORMAT: &str = "video.pixel_format";
    pub const AV_SYNC: &str = "av.sync";
    pub const CAPTION_SANITY: &str = "captions.sanity";
    pub const CAPTION_SPEED: &str = "captions.reading_speed";
    pub const CAPTION_COVERAGE: &str = "captions.coverage";
}

/// Evidence from decoding every selected media stream from start through EOF.
/// `completed` must be false for cancellation, timeout, partial scan or sampling.
/// `success` is decoder process success, independent of error-level diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecodeIntegrity {
    pub completed: bool,
    pub success: bool,
    /// Unabridged error-level decoder diagnostics; display evidence is capped.
    pub errors: String,
}

pub fn decode_integrity(measured: Option<&DecodeIntegrity>) -> Check {
    let ok = measured.is_some_and(|m| m.completed && m.success && m.errors.trim().is_empty());
    Check::measured(
        rule_ids::INTEGRITY,
        Severity::Error,
        ok,
        "decodes end to end without errors",
        "decode missing, incomplete or reported errors",
        json!({
            "completed": measured.map(|m| m.completed),
            "success": measured.map(|m| m.success),
            "decoder_errors": measured.map(|m| m.errors.lines().take(8).collect::<Vec<_>>()),
        }),
    )
}

/// Required streams are failures when absent. Stream metadata remains caller-owned.
pub fn required_streams(has_video: bool, has_audio: bool, require_audio: bool) -> Check {
    Check::measured(
        rule_ids::STREAMS,
        Severity::Error,
        has_video && (has_audio || !require_audio),
        "required streams present",
        "required stream missing",
        json!({"video": has_video, "audio": has_audio, "audio_required": require_audio}),
    )
}

fn policy(ok: bool, reason: &str) -> Result<(), ReviewError> {
    if ok {
        Ok(())
    } else {
        Err(ReviewError::InvalidRuleSet(reason.to_string()))
    }
}

/// Inclusive two-sided tolerance. With no expected duration, measured duration
/// must be positive. Missing/negative measurements fail, including absent duration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DurationRule {
    pub expected_ms: Option<i64>,
    pub tolerance_ms: u64,
}

impl DurationRule {
    pub fn check(&self, measured_ms: Option<i64>) -> Result<Check, ReviewError> {
        policy(
            self.expected_ms.is_none_or(|v| v > 0),
            "expected duration must be positive",
        )?;
        let ok = measured_ms.is_some_and(|got| {
            got > 0
                && self.expected_ms.is_none_or(|want| {
                    (got as i128 - want as i128).unsigned_abs() <= self.tolerance_ms as u128
                })
        });
        Ok(Check::measured(
            rule_ids::DURATION,
            Severity::Error,
            ok,
            "duration matches delivery",
            "duration unreadable or outside delivery tolerance",
            json!({
                "expected_ms": self.expected_ms,
                "probed_ms": measured_ms,
                "tolerance_ms": self.tolerance_ms,
            }),
        ))
    }
}

/// Inclusive frame-rate tolerance: max(absolute, expected * relative).
/// Set `relative_tolerance` to 0.0005 for CutRight's timeline comparison.
/// A missing expected rate still requires a finite positive measured rate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrameRateRule {
    pub expected_fps: Option<f64>,
    pub absolute_tolerance: f64,
    pub relative_tolerance: f64,
}

impl FrameRateRule {
    pub fn check(&self, measured_fps: Option<f64>) -> Result<Check, ReviewError> {
        policy(
            self.expected_fps.is_none_or(|v| v.is_finite() && v > 0.0)
                && self.absolute_tolerance.is_finite()
                && self.absolute_tolerance >= 0.0
                && self.relative_tolerance.is_finite()
                && self.relative_tolerance >= 0.0
                && self
                    .expected_fps
                    .is_none_or(|v| (v * self.relative_tolerance).is_finite()),
            "frame-rate policy needs finite positive rate & finite nonnegative tolerances",
        )?;
        let ok = measured_fps.is_some_and(|got| {
            got.is_finite()
                && got > 0.0
                && self.expected_fps.is_none_or(|want| {
                    (got - want).abs()
                        <= self.absolute_tolerance.max(want * self.relative_tolerance)
                })
        });
        Ok(Check::measured(
            rule_ids::FRAME_RATE,
            Severity::Error,
            ok,
            "frame rate matches delivery",
            "frame rate unreadable or outside delivery tolerance",
            json!({
                "expected_fps": self.expected_fps,
                "probed_fps": measured_fps,
                "absolute_tolerance": self.absolute_tolerance,
                "relative_tolerance": self.relative_tolerance,
            }),
        ))
    }
}

/// Exact, case-sensitive allowlist for video/audio codec or pixel format.
/// Missing values fail with chosen severity. Handle optional absent streams with
/// [`Check::not_applicable`] before calling this rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormatRule {
    pub allowed: Vec<String>,
}

impl FormatRule {
    pub fn check(
        &self,
        id: &str,
        severity: Severity,
        measured: Option<&str>,
    ) -> Result<Check, ReviewError> {
        policy(
            !id.trim().is_empty()
                && !self.allowed.is_empty()
                && self.allowed.iter().all(|v| !v.trim().is_empty()),
            "format rule needs an id & nonempty allowed values",
        )?;
        Ok(Check::measured(
            id,
            severity,
            measured.is_some_and(|got| self.allowed.iter().any(|v| v == got)),
            "format accepted",
            "format missing or outside delivery allowlist",
            json!({"measured": measured, "allowed": self.allowed}),
        ))
    }
}

/// Probe seconds preserve sub-millisecond differences until drift is rounded.
/// Negative start timestamps are valid; negative durations & non-finite values fail.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StreamTiming {
    pub start_s: Option<f64>,
    pub duration_s: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamSyncRule {
    pub max_start_offset_ms: u64,
    pub max_duration_drift_ms: u64,
}

impl StreamSyncRule {
    /// Checks whichever comparisons are readable. Neither pair, or either
    /// stream absent, is NotApplicable. Malformed present timestamps fail.
    pub fn check(&self, video: Option<&StreamTiming>, audio: Option<&StreamTiming>) -> Check {
        let (Some(v), Some(a)) = (video, audio) else {
            return Check::not_applicable(rule_ids::AV_SYNC, "needs both video & audio streams");
        };
        let invalid = [v.start_s, a.start_s]
            .into_iter()
            .flatten()
            .any(|t| !t.is_finite())
            || [v.duration_s, a.duration_s]
                .into_iter()
                .flatten()
                .any(|t| !t.is_finite() || t < 0.0);
        if invalid {
            return Check::measured(
                rule_ids::AV_SYNC,
                Severity::Error,
                false,
                "",
                "invalid stream timestamps",
                Value::Null,
            );
        }
        let drift = v
            .duration_s
            .zip(a.duration_s)
            .map(|(v, a)| ((v - a).abs() * 1000.0).round());
        let offset = v
            .start_s
            .zip(a.start_s)
            .map(|(v, a)| ((v - a).abs() * 1000.0).round());
        if drift.is_none() && offset.is_none() {
            return Check::not_applicable(
                rule_ids::AV_SYNC,
                "neither start times nor durations readable on both streams",
            );
        }
        let ok = drift.is_none_or(|d| d.is_finite() && d <= self.max_duration_drift_ms as f64)
            && offset.is_none_or(|o| o.is_finite() && o <= self.max_start_offset_ms as f64);
        Check::measured(
            rule_ids::AV_SYNC,
            Severity::Error,
            ok,
            "audio & video start together & have matching durations",
            "A/V out of sync",
            json!({
                "duration_drift_ms": drift,
                "start_offset_ms": offset,
                "max_drift_ms": self.max_duration_drift_ms,
                "max_start_offset_ms": self.max_start_offset_ms,
            }),
        )
    }
}

/// Parsed caption text. SRT/WebVTT parsing remains with caller.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptionCue {
    pub start_ms: i64,
    pub end_ms: i64,
    pub lines: Vec<String>,
}

/// Text safe zone is measured in nonempty lines & Unicode scalar values per
/// line, matching CutRight's burn-in box contract. This does not measure pixels.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaptionRule {
    pub required: bool,
    pub max_lines: usize,
    pub max_chars_per_line: usize,
    pub max_chars_per_second: f64,
    pub max_tail_gap_ms: u64,
    pub duration_tolerance_ms: u64,
}

impl CaptionRule {
    /// Sanity failures block; reading speed & tail coverage failures warn.
    /// None means captions absent; Some([]) means supplied but empty & fails.
    /// Unknown render duration skips only end-bound & coverage comparisons;
    /// delivery duration should be checked independently with [`DurationRule`].
    pub fn check(
        &self,
        cues: Option<&[CaptionCue]>,
        duration_ms: Option<i64>,
    ) -> Result<Vec<Check>, ReviewError> {
        policy(
            self.max_lines > 0
                && self.max_chars_per_line > 0
                && self.max_chars_per_second.is_finite()
                && self.max_chars_per_second > 0.0,
            "caption policy needs positive line/character limits & finite positive reading speed",
        )?;
        let Some(cues) = cues else {
            return Ok(vec![if self.required {
                Check::measured(
                    rule_ids::CAPTION_SANITY,
                    Severity::Error,
                    false,
                    "",
                    "captions required but absent",
                    Value::Null,
                )
            } else {
                Check::not_applicable(rule_ids::CAPTION_SANITY, "no captions supplied")
            }]);
        };
        let mut problems = Vec::new();
        let mut fast = Vec::new();
        if cues.is_empty() {
            problems.push("no cues".to_string());
        }
        if duration_ms.is_some_and(|v| v < 0) {
            problems.push("invalid render duration".to_string());
        }
        let mut previous_end = None;
        for (i, c) in cues.iter().enumerate() {
            let n = i + 1;
            if c.start_ms < 0 || c.end_ms <= c.start_ms {
                problems.push(format!("cue {n}: bad span {}..{}", c.start_ms, c.end_ms));
            }
            if duration_ms
                .is_some_and(|d| c.end_ms as i128 > d as i128 + self.duration_tolerance_ms as i128)
            {
                problems.push(format!("cue {n}: ends after render"));
            }
            // Maximum earlier end catches overlaps even in unsorted input.
            if i > 0 && c.start_ms < cues[i - 1].start_ms {
                problems.push(format!("cue {n}: out of order"));
            }
            if previous_end.is_some_and(|end| c.start_ms < end) {
                problems.push(format!("cue {n}: overlaps earlier cue"));
            }
            previous_end = Some(previous_end.map_or(c.end_ms, |end: i64| end.max(c.end_ms)));
            let lines: Vec<_> = c.lines.iter().filter(|l| !l.trim().is_empty()).collect();
            if lines.is_empty() {
                problems.push(format!("cue {n}: empty"));
            }
            if lines.len() > self.max_lines {
                problems.push(format!("cue {n}: too many lines for safe zone"));
            }
            if lines
                .iter()
                .any(|l| l.chars().count() > self.max_chars_per_line)
            {
                problems.push(format!("cue {n}: line overflows safe zone"));
            }
            let chars: usize = c.lines.iter().map(|l| l.chars().count()).sum();
            let duration = (c.end_ms as i128 - c.start_ms as i128).max(1) as f64 / 1000.0;
            if chars as f64 / duration > self.max_chars_per_second {
                fast.push(n);
            }
        }
        let mut out = vec![
            Check::measured(
                rule_ids::CAPTION_SANITY,
                Severity::Error,
                problems.is_empty(),
                "captions ordered, inside render & safe zone",
                problems.join("; "),
                json!({
                    "cues": cues.len(),
                    "problems": problems,
                    "max_lines": self.max_lines,
                    "max_chars_per_line": self.max_chars_per_line,
                }),
            ),
            Check::measured(
                rule_ids::CAPTION_SPEED,
                Severity::Warning,
                fast.is_empty(),
                "reading speed comfortable",
                format!("cues {fast:?} exceed reading speed limit"),
                json!({
                    "fast_cues": fast,
                    "max_chars_per_second": self.max_chars_per_second,
                }),
            ),
        ];
        if let (Some(last), Some(dur)) = (
            cues.iter().map(|c| c.end_ms).max(),
            duration_ms.filter(|d| *d >= 0),
        ) {
            let gap = (dur as i128 - last as i128).max(0);
            out.push(Check::measured(
                rule_ids::CAPTION_COVERAGE,
                Severity::Warning,
                gap <= self.max_tail_gap_ms as i128,
                "captions run to render end",
                format!("last cue ends {gap} ms before render ends"),
                json!({"last_cue_end_ms": last, "tail_gap_ms": gap}),
            ));
        }
        Ok(out)
    }
}
