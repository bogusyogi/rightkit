//! The description a review runs over. The crate never decodes media: frame,
//! text, and probe measurements are produced by a deterministic process
//! elsewhere and handed in as data. Absent measurements are `None`, which a
//! rule reports as unverified rather than as a pass. A text body is handed in
//! as data too; text rules read it and never fetch it.

use serde::{Deserialize, Serialize};

use crate::ReviewError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    Video,
    Still,
    Carousel,
    Text,
}

/// One measured frame. `frame_hash` is compared between neighbouring frames to
/// find frozen runs. `ambiguous` carries a reason when the measurement cannot
/// be trusted, which makes the frame-level rules hold for a human.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameStat {
    pub time_ms: u64,
    pub black: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ambiguous: Option<String>,
}

/// A measured text region. `None` means the region was not measured.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextRegion {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overflow: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contrast_ratio: Option<f32>,
}

/// Two or more independent renders of one input. Identical output hashes are
/// the evidence that rendering is deterministic.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeterminismProbe {
    pub input_hash: String,
    pub output_hashes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactDescription {
    pub artifact_id: String,
    pub kind: ArtifactKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frames: Option<Vec<FrameStat>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_flashes_per_second: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_regions: Option<Vec<TextRegion>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub determinism: Option<DeterminismProbe>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub licence: Option<String>,
    /// The text body for `ArtifactKind::Text`. `None` means not supplied,
    /// which text rules report as inconclusive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<TextBody>,
}

/// The text a text rule reads, with the claim annotations bound to it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextBody {
    /// The exact text that will be queued. Producers supply it in NFC.
    pub body: String,
    /// Claim annotations for `RuleKind::ClaimProof`. Empty when the text makes no claims.
    #[serde(default)]
    pub claims: Vec<ClaimAnnotation>,
}

/// One number or named fact in the text, bound to the value a proof must state.
/// `value` is a decimal string. `date` is `YYYY-MM-DD`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaimAnnotation {
    pub value: String,
    pub unit: String,
    pub subject: String,
    pub date: String,
}

/// What the brief declared before the artifact was made. A declared window is
/// the only way a deliberate hold, silence, or static region passes a check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowKind {
    /// A deliberate hold on one picture. Exempts black and frozen checks.
    Hold,
    /// A deliberate silent passage. Exempts black-frame checks.
    Silence,
    /// A deliberately static region, such as a still title card. Exempts frozen checks.
    Static,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeclaredWindow {
    pub kind: WindowKind,
    pub start_ms: u64,
    pub end_ms: u64,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Declarations {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_duration_ms: Option<u64>,
    #[serde(default)]
    pub allowed_licences: Vec<String>,
    #[serde(default)]
    pub windows: Vec<DeclaredWindow>,
}

/// One artifact together with the brief declarations it is checked against.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Subject {
    pub artifact: ArtifactDescription,
    #[serde(default)]
    pub declarations: Declarations,
}

impl Subject {
    /// Rejects descriptions the rules cannot interpret. Run before any rule.
    pub fn validate(&self) -> Result<(), ReviewError> {
        let invalid = |message: String| Err(ReviewError::InvalidSubject(message));
        let artifact = &self.artifact;
        if artifact.artifact_id.trim().is_empty() {
            return invalid("artifact_id is empty".to_string());
        }
        if let Some(frames) = &artifact.frames {
            for pair in frames.windows(2) {
                if pair[1].time_ms <= pair[0].time_ms {
                    return invalid(format!(
                        "frame times must strictly increase; {} ms follows {} ms",
                        pair[1].time_ms, pair[0].time_ms
                    ));
                }
            }
        }
        if let Some(value) = artifact.max_flashes_per_second {
            if !value.is_finite() || value < 0.0 {
                return invalid(
                    "max_flashes_per_second must be finite and non-negative".to_string(),
                );
            }
        }
        if let Some(regions) = &artifact.text_regions {
            for region in regions {
                if let Some(ratio) = region.contrast_ratio {
                    if !ratio.is_finite() || ratio < 0.0 {
                        return invalid(format!(
                            "contrast ratio for `{}` is not a valid number",
                            region.id
                        ));
                    }
                }
            }
        }
        for window in &self.declarations.windows {
            if window.start_ms >= window.end_ms {
                return invalid(format!(
                    "declared window {}..{} ms is empty or reversed",
                    window.start_ms, window.end_ms
                ));
            }
            if window.reason.trim().is_empty() {
                return invalid("declared window needs a reason".to_string());
            }
        }
        Ok(())
    }
}
