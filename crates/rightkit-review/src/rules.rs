//! Typed deterministic rules. Each rule reads measurements from a `Subject`
//! and returns one verdict. No rule calls a model, and none reads pixels.

use serde::{Deserialize, Serialize};

use crate::subject::{ArtifactKind, FrameStat, Subject, WindowKind};
use crate::verdict::{Finding, Verdict};
use crate::ReviewError;

/// Stable rule ids. Consumers and tests refer to rules by these names.
pub mod rule_ids {
    pub const BLACK_FRAME: &str = "det.black_frame";
    pub const FROZEN_FRAME: &str = "det.frozen_frame";
    pub const TEXT_OVERFLOW: &str = "det.text_overflow";
    pub const CONTRAST: &str = "det.contrast";
    pub const FLASH_RATE: &str = "det.flash_rate";
    pub const DETERMINISM: &str = "det.determinism";
    pub const LICENCE: &str = "det.licence";
    pub const DURATION: &str = "det.duration";
    pub const RESOLUTION: &str = "det.resolution";
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RuleKind {
    /// Black frames must sit inside a declared hold or silence window.
    NoUnexpectedBlackFrames,
    /// Identical consecutive frames must not run for `min_run_ms` outside a declared hold or static window.
    NoUnexpectedFrozenRun { min_run_ms: u64 },
    /// No text region may overflow its box.
    TextWithinBounds,
    /// Every measured text region must reach `min_ratio` contrast.
    TextContrast { min_ratio: f32 },
    /// Measured flashes per second must not exceed `max_per_second`.
    FlashRate { max_per_second: f32 },
    /// Repeated renders of one input must produce identical output hashes.
    RenderDeterminism,
    /// The artifact licence must be in the brief's allowed licence scope.
    LicenceAllowed,
    /// The duration must not exceed the brief's maximum.
    DurationWithinDelivery,
    /// The dimensions must equal the brief's target dimensions.
    ResolutionMatchesDelivery,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rule {
    pub id: String,
    #[serde(default = "required_by_default")]
    pub required: bool,
    pub kind: RuleKind,
}

fn required_by_default() -> bool {
    true
}

/// A versioned, ordered set of rules. Bump `version` when a rule's meaning changes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleSet {
    pub id: String,
    pub version: u32,
    pub rules: Vec<Rule>,
}

impl RuleSet {
    /// Builds a rule set after checking ids and thresholds.
    pub fn new(id: &str, version: u32, rules: Vec<Rule>) -> Result<Self, ReviewError> {
        if id.trim().is_empty() {
            return Err(ReviewError::InvalidRuleSet(
                "rule set id is empty".to_string(),
            ));
        }
        if version == 0 {
            return Err(ReviewError::InvalidRuleSet(
                "rule set version starts at 1".to_string(),
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        for rule in &rules {
            if rule.id.trim().is_empty() {
                return Err(ReviewError::InvalidRuleSet("rule id is empty".to_string()));
            }
            if !seen.insert(rule.id.as_str()) {
                return Err(ReviewError::InvalidRuleSet(format!(
                    "duplicate rule id `{}`",
                    rule.id
                )));
            }
            let valid = match &rule.kind {
                RuleKind::NoUnexpectedFrozenRun { min_run_ms } => *min_run_ms > 0,
                RuleKind::TextContrast { min_ratio } => min_ratio.is_finite() && *min_ratio > 0.0,
                RuleKind::FlashRate { max_per_second } => {
                    max_per_second.is_finite() && *max_per_second > 0.0
                }
                _ => true,
            };
            if !valid {
                return Err(ReviewError::InvalidRuleSet(format!(
                    "rule `{}` has a threshold outside its domain",
                    rule.id
                )));
            }
        }
        Ok(RuleSet {
            id: id.to_string(),
            version,
            rules,
        })
    }

    /// The shared standard set, covering the deterministic checks named in
    /// the G6 and P3 plans. Every rule is required.
    pub fn standard() -> Self {
        let rule = |id: &str, kind: RuleKind| Rule {
            id: id.to_string(),
            required: true,
            kind,
        };
        RuleSet {
            id: "rightkit.review.standard".to_string(),
            version: 1,
            rules: vec![
                rule(rule_ids::BLACK_FRAME, RuleKind::NoUnexpectedBlackFrames),
                rule(
                    rule_ids::FROZEN_FRAME,
                    RuleKind::NoUnexpectedFrozenRun { min_run_ms: 1000 },
                ),
                rule(rule_ids::TEXT_OVERFLOW, RuleKind::TextWithinBounds),
                rule(
                    rule_ids::CONTRAST,
                    RuleKind::TextContrast { min_ratio: 4.5 },
                ),
                rule(
                    rule_ids::FLASH_RATE,
                    RuleKind::FlashRate {
                        max_per_second: 3.0,
                    },
                ),
                rule(rule_ids::DETERMINISM, RuleKind::RenderDeterminism),
                rule(rule_ids::LICENCE, RuleKind::LicenceAllowed),
                rule(rule_ids::DURATION, RuleKind::DurationWithinDelivery),
                rule(rule_ids::RESOLUTION, RuleKind::ResolutionMatchesDelivery),
            ],
        }
    }

    /// Validates the subject, then runs every rule in order.
    pub fn evaluate(&self, subject: &Subject) -> Result<Vec<Finding>, ReviewError> {
        subject.validate()?;
        Ok(self
            .rules
            .iter()
            .map(|rule| evaluate_rule(rule, subject))
            .collect())
    }
}

fn evaluate_rule(rule: &Rule, subject: &Subject) -> Finding {
    Finding {
        rule_id: rule.id.clone(),
        required: rule.required,
        verdict: check(&rule.kind, subject),
    }
}

fn check(kind: &RuleKind, subject: &Subject) -> Verdict {
    let artifact = &subject.artifact;
    let video_only = matches!(
        kind,
        RuleKind::NoUnexpectedBlackFrames
            | RuleKind::NoUnexpectedFrozenRun { .. }
            | RuleKind::FlashRate { .. }
            | RuleKind::DurationWithinDelivery
    );
    if video_only && artifact.kind != ArtifactKind::Video {
        return Verdict::NotApplicable {
            reason: "rule applies to video artifacts only".to_string(),
        };
    }
    match kind {
        RuleKind::NoUnexpectedBlackFrames => black_frames(subject),
        RuleKind::NoUnexpectedFrozenRun { min_run_ms } => frozen_runs(subject, *min_run_ms),
        RuleKind::TextWithinBounds => text_bounds(subject),
        RuleKind::TextContrast { min_ratio } => text_contrast(subject, *min_ratio),
        RuleKind::FlashRate { max_per_second } => flash_rate(subject, *max_per_second),
        RuleKind::RenderDeterminism => determinism(subject),
        RuleKind::LicenceAllowed => licence(subject),
        RuleKind::DurationWithinDelivery => duration(subject),
        RuleKind::ResolutionMatchesDelivery => resolution(subject),
    }
}

/// True when the instant `t` lies inside a declared window of one of `kinds`.
fn point_declared(subject: &Subject, kinds: &[WindowKind], t: u64) -> bool {
    subject
        .declarations
        .windows
        .iter()
        .any(|window| kinds.contains(&window.kind) && window.start_ms <= t && t < window.end_ms)
}

/// True when the whole span `start..=end` lies inside one declared window of one of `kinds`.
fn span_declared(subject: &Subject, kinds: &[WindowKind], start: u64, end: u64) -> bool {
    subject.declarations.windows.iter().any(|window| {
        kinds.contains(&window.kind) && window.start_ms <= start && end < window.end_ms
    })
}

fn ambiguous_frames(frames: &[FrameStat]) -> Option<String> {
    let reasons: Vec<String> = frames
        .iter()
        .filter_map(|frame| {
            frame
                .ambiguous
                .as_ref()
                .map(|reason| format!("{} ms: {}", frame.time_ms, reason))
        })
        .collect();
    if reasons.is_empty() {
        None
    } else {
        Some(format!(
            "ambiguous frame measurement(s): {}",
            reasons.join("; ")
        ))
    }
}

fn black_frames(subject: &Subject) -> Verdict {
    let Some(frames) = &subject.artifact.frames else {
        return Verdict::Unverified {
            reason: "no frame measurements".to_string(),
        };
    };
    let offenders: Vec<u64> = frames
        .iter()
        .filter(|frame| {
            frame.black
                && frame.ambiguous.is_none()
                && !point_declared(
                    subject,
                    &[WindowKind::Hold, WindowKind::Silence],
                    frame.time_ms,
                )
        })
        .map(|frame| frame.time_ms)
        .collect();
    if !offenders.is_empty() {
        return Verdict::Failed {
            reason: format!(
                "black frame(s) outside a declared hold or silence at {offenders:?} ms"
            ),
        };
    }
    if let Some(reason) = ambiguous_frames(frames) {
        return Verdict::Held { reason };
    }
    Verdict::Verified
}

fn frozen_runs(subject: &Subject, min_run_ms: u64) -> Verdict {
    let Some(frames) = &subject.artifact.frames else {
        return Verdict::Unverified {
            reason: "no frame measurements".to_string(),
        };
    };
    if frames.iter().any(|frame| frame.frame_hash.is_none()) {
        return Verdict::Inconclusive {
            reason: "frame hashes are missing, so frozen runs cannot be detected".to_string(),
        };
    }
    let mut offenders = Vec::new();
    let mut index = 0;
    while index < frames.len() {
        let hash = frames[index].frame_hash.as_ref();
        let mut last = index;
        while last + 1 < frames.len() && frames[last + 1].frame_hash.as_ref() == hash {
            last += 1;
        }
        let start = frames[index].time_ms;
        let end = frames[last].time_ms;
        if end - start >= min_run_ms
            && !span_declared(subject, &[WindowKind::Hold, WindowKind::Static], start, end)
        {
            offenders.push(format!("{start}..{end} ms"));
        }
        index = last + 1;
    }
    if !offenders.is_empty() {
        return Verdict::Failed {
            reason: format!(
                "frozen run of {min_run_ms} ms or more outside a declared hold or static window: {}",
                offenders.join(", ")
            ),
        };
    }
    if let Some(reason) = ambiguous_frames(frames) {
        return Verdict::Held { reason };
    }
    Verdict::Verified
}

fn text_bounds(subject: &Subject) -> Verdict {
    let Some(regions) = &subject.artifact.text_regions else {
        return Verdict::Unverified {
            reason: "text regions were not measured".to_string(),
        };
    };
    let overflowing: Vec<&str> = regions
        .iter()
        .filter(|region| region.overflow == Some(true))
        .map(|region| region.id.as_str())
        .collect();
    if !overflowing.is_empty() {
        return Verdict::Failed {
            reason: format!("text overflows its box: {}", overflowing.join(", ")),
        };
    }
    let unmeasured: Vec<&str> = regions
        .iter()
        .filter(|region| region.overflow.is_none())
        .map(|region| region.id.as_str())
        .collect();
    if !unmeasured.is_empty() {
        return Verdict::Unverified {
            reason: format!("overflow not measured for: {}", unmeasured.join(", ")),
        };
    }
    Verdict::Verified
}

fn text_contrast(subject: &Subject, min_ratio: f32) -> Verdict {
    let Some(regions) = &subject.artifact.text_regions else {
        return Verdict::Unverified {
            reason: "text regions were not measured".to_string(),
        };
    };
    let below: Vec<String> = regions
        .iter()
        .filter_map(|region| match region.contrast_ratio {
            Some(ratio) if ratio < min_ratio => Some(format!("{} at {ratio:.2}:1", region.id)),
            _ => None,
        })
        .collect();
    if !below.is_empty() {
        return Verdict::Failed {
            reason: format!("contrast below {min_ratio:.1}:1: {}", below.join(", ")),
        };
    }
    let unmeasured: Vec<&str> = regions
        .iter()
        .filter(|region| region.contrast_ratio.is_none())
        .map(|region| region.id.as_str())
        .collect();
    if !unmeasured.is_empty() {
        return Verdict::Unverified {
            reason: format!("contrast not measured for: {}", unmeasured.join(", ")),
        };
    }
    Verdict::Verified
}

fn flash_rate(subject: &Subject, max_per_second: f32) -> Verdict {
    match subject.artifact.max_flashes_per_second {
        None => Verdict::Unverified {
            reason: "flash rate was not measured".to_string(),
        },
        Some(measured) if measured > max_per_second => Verdict::Failed {
            reason: format!("{measured:.2} flashes per second exceeds {max_per_second:.2}"),
        },
        Some(_) => Verdict::Verified,
    }
}

fn determinism(subject: &Subject) -> Verdict {
    let Some(probe) = &subject.artifact.determinism else {
        return Verdict::Inconclusive {
            reason: "no determinism probe was recorded".to_string(),
        };
    };
    if probe.input_hash.trim().is_empty() {
        return Verdict::Inconclusive {
            reason: "determinism probe has no input hash".to_string(),
        };
    }
    if probe.output_hashes.len() < 2 {
        return Verdict::Inconclusive {
            reason: "determinism needs at least two renders".to_string(),
        };
    }
    let first = &probe.output_hashes[0];
    match probe.output_hashes.iter().position(|hash| hash != first) {
        Some(index) => Verdict::Failed {
            reason: format!("render {index} differs from render 0 for the same input"),
        },
        None => Verdict::Verified,
    }
}

fn licence(subject: &Subject) -> Verdict {
    let allowed = &subject.declarations.allowed_licences;
    if allowed.is_empty() {
        return Verdict::Inconclusive {
            reason: "the brief declares no licence scope".to_string(),
        };
    }
    match &subject.artifact.licence {
        None => Verdict::Unverified {
            reason: "artifact licence was not recorded".to_string(),
        },
        Some(licence) if allowed.contains(licence) => Verdict::Verified,
        Some(licence) => Verdict::Failed {
            reason: format!("licence `{licence}` is outside the declared scope"),
        },
    }
}

fn duration(subject: &Subject) -> Verdict {
    let Some(max) = subject.declarations.max_duration_ms else {
        return Verdict::Inconclusive {
            reason: "the delivery declares no maximum duration".to_string(),
        };
    };
    match subject.artifact.duration_ms {
        None => Verdict::Unverified {
            reason: "duration was not measured".to_string(),
        },
        Some(duration) if duration > max => Verdict::Failed {
            reason: format!("duration {duration} ms exceeds the {max} ms delivery maximum"),
        },
        Some(_) => Verdict::Verified,
    }
}

fn resolution(subject: &Subject) -> Verdict {
    let (Some(target_w), Some(target_h)) = (
        subject.declarations.target_width,
        subject.declarations.target_height,
    ) else {
        return Verdict::Inconclusive {
            reason: "the delivery declares no target dimensions".to_string(),
        };
    };
    let (Some(width), Some(height)) = (subject.artifact.width, subject.artifact.height) else {
        return Verdict::Unverified {
            reason: "artifact dimensions were not measured".to_string(),
        };
    };
    if (width, height) == (target_w, target_h) {
        Verdict::Verified
    } else {
        Verdict::Failed {
            reason: format!(
                "artifact is {width}x{height}, delivery requires {target_w}x{target_h}"
            ),
        }
    }
}
