//! Typed Adapt-proposal intake (PL-011).
//!
//! A proposal arriving from Membrane/Adapt is **typed evidence & intent**.
//! These types are the wire/intake shapes only — nothing here grants mutation
//! authority. Authority is a separate host decision
//! ([`super::authority::ApplicationAuthorityGate`]).
//!
//! Shapes follow the Membrane/Adapt H7 handoff contract
//! (`docs/pending/CODERIGHT-MEMBRANE-INTEGRATION.md` §10):
//! `SealedRemediationProposalV1` carries an `intervention_target`, a
//! mutation-eligible `InterventionAttributionV1` (required for mutable
//! instruction surfaces), an `expected_base_digest` binding the candidate to
//! its base artifact, an itemized delta (never a wholesale rewrite), typed
//! evidence references, and a typed approval receipt from the review surface.
//!
//! ## Canonical section format
//!
//! Refinement replaces one delimited section of the owning artifact's content,
//! so the consumer can never be asked to apply a wholesale rewrite. Sections
//! are delimited by the marker
//!
//! ```text
//! <!-- adapt:section:<section_id> -->
//! ```
//!
//! on its own line. Content before the first marker is the preamble and is
//! preserved verbatim. Artifacts without any section marker cannot be refined
//! through this path ([`SectionDeltaError::TargetNotSectioned`]) — the
//! replace-first discipline requires an itemized target, so a whole-artifact
//! rewrite is structurally impossible here.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Adapt contract version this consumer accepts.
pub const ADAPT_CONTRACT_VERSION: &str = "1";

/// Canonical section-marker prefix (a marker line is `PREFIX <id> SUFFIX`).
pub const SECTION_MARKER_PREFIX: &str = "<!-- adapt:section:";
/// Canonical section-marker suffix.
pub const SECTION_MARKER_SUFFIX: &str = "-->";

/// Digest of a guidance artifact's content. This is the `observed_base_digest`
/// value for a refine target and the record digest reported by the store.
pub fn guidance_content_digest(content: &str) -> String {
    rightkit_fs::digest::sha256_bytes(content.as_bytes())
}

/// Deterministic restatement normalization used by the duplicate gate:
/// lowercase, all whitespace runs collapsed to single spaces, trimmed.
/// This is a mechanical detector for verbatim restatement, not semantic
/// equality — semantic novelty is Adapt's layer, deduplication at the host is
/// structural.
pub fn normalize_guidance_text(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// A recognized mutable instruction surface (integration doc §10: "Begin with
/// text artifacts: skills/procedures, system prompts, tool descriptions, and
/// documentation policy").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuidanceKind {
    Skill,
    Procedure,
    SystemPrompt,
    ToolDescription,
    DocumentationPolicy,
}

impl GuidanceKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            GuidanceKind::Skill => "skill",
            GuidanceKind::Procedure => "procedure",
            GuidanceKind::SystemPrompt => "system_prompt",
            GuidanceKind::ToolDescription => "tool_description",
            GuidanceKind::DocumentationPolicy => "documentation_policy",
        }
    }
}

/// Typed mutation kinds (canon PL-011).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationKind {
    /// Modify/refine the narrowest existing owning guidance (replace-first).
    /// The delta is an itemized section replacement against the bound base
    /// artifact — never a wholesale rewrite.
    Refine,
    /// Create a new guidance record. Valid only when no existing owner can
    /// carry the contract without semantic loss; the apply pipeline rejects
    /// appends that overlap an owner.
    Append,
}

impl MutationKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            MutationKind::Refine => "refine",
            MutationKind::Append => "append",
        }
    }
}

/// The itemized delta payload. The variant must match the proposal's
/// [`MutationKind`] (validated at intake).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProposalDelta {
    /// Replace the body of one section of the target artifact.
    RefineSection {
        section_id: String,
        replacement: String,
        rationale: String,
    },
    /// Append a brand-new guidance record (subject must be unowned).
    AppendRecord { content: String, rationale: String },
}

impl ProposalDelta {
    /// The mutation kind this delta implements.
    pub fn mutation_kind(&self) -> MutationKind {
        match self {
            ProposalDelta::RefineSection { .. } => MutationKind::Refine,
            ProposalDelta::AppendRecord { .. } => MutationKind::Append,
        }
    }

    /// Byte size of the payload the authority gate's size bound applies to.
    pub fn payload_bytes(&self) -> usize {
        match self {
            ProposalDelta::RefineSection { replacement, .. } => replacement.len(),
            ProposalDelta::AppendRecord { content, .. } => content.len(),
        }
    }
}

/// Typed evidence classes (integration doc §9: citations, explicit references,
/// follow-up search, refetch, user correction, evaluator outcome).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    UserCorrection,
    EvaluatorOutcome,
    DetectorResult,
    RecurrenceMeasurement,
    ExplicitReference,
}

/// One typed evidence reference carried by the proposal or its attribution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposalEvidenceRef {
    pub kind: EvidenceKind,
    pub reference: String,
}

/// Typed approval receipt from the Adapt review surface (integration doc §11).
/// Necessary evidence — never sufficient authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdaptReviewAction {
    /// Approves applying this remediation to the host guidance surface.
    ApproveRemediation,
    /// Approves dataset admission for a regression case (§9) — does NOT
    /// authorize a guidance mutation.
    ApproveRegressionCase,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposalApproval {
    pub approval_id: String,
    pub action: AdaptReviewAction,
    pub approved_by: String,
    pub approved_at_utc: String,
}

/// Mutation-eligible attribution (integration doc §10 / Adapt canon §6.9): a
/// proposal targeting a mutable instruction surface enters application only
/// with a referenced attribution covering the target subject.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterventionAttributionV1 {
    pub attribution_id: String,
    pub attributed_subject: String,
    pub evidence_refs: Vec<ProposalEvidenceRef>,
}

/// What the proposal intervenes on. For [`MutationKind::Refine`],
/// `base_guidance_id` names the owning record and `expected_base_digest` binds
/// its content digest. For [`MutationKind::Append`] there is no base record —
/// `expected_base_digest` binds the digest of the whole surface (see
/// [`super::guidance::surface_digest`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterventionTargetV1 {
    pub surface: GuidanceKind,
    pub subject: String,
    pub base_guidance_id: Option<String>,
}

/// An approved, sealed Adapt remediation proposal — typed evidence & intent.
/// "Sealed" means it arrived with a typed approval receipt and is immutable
/// from the host's perspective; the host still applies it only under its own
/// authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealedRemediationProposalV1 {
    pub contract_version: String,
    pub proposal_id: String,
    pub intervention_target: InterventionTargetV1,
    /// The base-artifact digest the proposal was authored against
    /// (`base_artifact_digest` in the integration doc). Compared against the
    /// host-observed digest at application; mismatch rejects as stale.
    pub expected_base_digest: String,
    pub mutation_kind: MutationKind,
    pub delta: ProposalDelta,
    pub evidence: Vec<ProposalEvidenceRef>,
    pub approval: ProposalApproval,
    /// Required for every guidance-kind target (all PL-011 surfaces are
    /// mutable instruction surfaces); application rejects without it.
    pub attribution: Option<InterventionAttributionV1>,
    /// Guidance ids this proposal declares it replaces. Only meaningful for
    /// [`MutationKind::Append`], where any non-empty value is REJECTED — an
    /// append that supersedes something is a refine in disguise and violates
    /// replace-first. Refinement retires nothing as a side effect.
    pub supersedes: Vec<String>,
}

/// Structural intake failures. These are evidence-shape problems, detected
/// before any authority or store interaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntakeError {
    ContractVersion {
        found: String,
    },
    EmptyField(&'static str),
    InvalidBaseDigest(String),
    InvalidSubject(String),
    /// Refine requires `base_guidance_id`; Append must not carry one.
    BaseBindingMismatch {
        kind: MutationKind,
    },
    KindDeltaMismatch {
        kind: MutationKind,
        delta: &'static str,
    },
    MarkerInReplacement,
}

impl fmt::Display for IntakeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IntakeError::ContractVersion { found } => write!(
                f,
                "unsupported adapt contract version {found:?} (expected {ADAPT_CONTRACT_VERSION:?})"
            ),
            IntakeError::EmptyField(name) => write!(f, "required field {name:?} is empty"),
            IntakeError::InvalidBaseDigest(d) => write!(
                f,
                "expected_base_digest {d:?} is not a 64-char lowercase sha256 hex digest"
            ),
            IntakeError::InvalidSubject(s) => write!(
                f,
                "subject {s:?} is invalid: must be non-empty dot-path of [a-z0-9.-]"
            ),
            IntakeError::BaseBindingMismatch { kind } => write!(
                f,
                "mutation kind {kind:?} has the wrong base_guidance_id binding (refine requires one, append forbids one)"
            ),
            IntakeError::KindDeltaMismatch { kind, delta } => {
                write!(f, "mutation kind {kind:?} does not match delta {delta:?}")
            }
            IntakeError::MarkerInReplacement => write!(
                f,
                "replacement injects a section marker, which would corrupt the itemized-delta structure"
            ),
        }
    }
}

impl std::error::Error for IntakeError {}

/// Subjects are dot-delimited paths (e.g. `review.errors`); owner coverage and
/// narrowness are defined over this path shape.
pub fn validate_subject(subject: &str) -> Result<(), IntakeError> {
    let ok = !subject.is_empty()
        && subject
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-');
    if ok {
        Ok(())
    } else {
        Err(IntakeError::InvalidSubject(subject.to_string()))
    }
}

fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl SealedRemediationProposalV1 {
    /// Structural intake validation. Ordered so the first failure is the
    /// cheapest and most fundamental one.
    pub fn validate_intake(&self) -> Result<(), IntakeError> {
        if self.contract_version != ADAPT_CONTRACT_VERSION {
            return Err(IntakeError::ContractVersion {
                found: self.contract_version.clone(),
            });
        }
        if self.proposal_id.trim().is_empty() {
            return Err(IntakeError::EmptyField("proposal_id"));
        }
        validate_subject(&self.intervention_target.subject)?;
        if !is_sha256_hex(&self.expected_base_digest) {
            return Err(IntakeError::InvalidBaseDigest(
                self.expected_base_digest.clone(),
            ));
        }
        if self.approval.approval_id.trim().is_empty() {
            return Err(IntakeError::EmptyField("approval.approval_id"));
        }
        if self.approval.approved_by.trim().is_empty() {
            return Err(IntakeError::EmptyField("approval.approved_by"));
        }
        if self.approval.approved_at_utc.trim().is_empty() {
            return Err(IntakeError::EmptyField("approval.approved_at_utc"));
        }
        // Kind/delta coherence.
        if self.mutation_kind != self.delta.mutation_kind() {
            return Err(IntakeError::KindDeltaMismatch {
                kind: self.mutation_kind,
                delta: match self.delta {
                    ProposalDelta::RefineSection { .. } => "refine_section",
                    ProposalDelta::AppendRecord { .. } => "append_record",
                },
            });
        }
        // Base binding coherence.
        match self.mutation_kind {
            MutationKind::Refine => match self.intervention_target.base_guidance_id.as_deref() {
                None => {
                    return Err(IntakeError::BaseBindingMismatch {
                        kind: MutationKind::Refine,
                    })
                }
                Some(id) if id.trim().is_empty() => {
                    return Err(IntakeError::BaseBindingMismatch {
                        kind: MutationKind::Refine,
                    })
                }
                Some(_) => {}
            },
            MutationKind::Append => {
                if self.intervention_target.base_guidance_id.is_some() {
                    return Err(IntakeError::BaseBindingMismatch {
                        kind: MutationKind::Append,
                    });
                }
            }
        }
        // Delta payload sanity.
        match &self.delta {
            ProposalDelta::RefineSection {
                section_id,
                replacement,
                ..
            } => {
                if section_id.trim().is_empty()
                    || section_id.contains(SECTION_MARKER_SUFFIX)
                    || section_id.contains('\n')
                {
                    return Err(IntakeError::EmptyField("delta.refine_section.section_id"));
                }
                if replacement.contains(SECTION_MARKER_PREFIX) {
                    return Err(IntakeError::MarkerInReplacement);
                }
            }
            ProposalDelta::AppendRecord { content, .. } => {
                if content.trim().is_empty() {
                    return Err(IntakeError::EmptyField("delta.append_record.content"));
                }
            }
        }
        // Evidence references must actually reference something.
        for r in &self.evidence {
            if r.reference.trim().is_empty() {
                return Err(IntakeError::EmptyField("evidence.reference"));
            }
        }
        // Attribution, when present, must be well-formed (presence itself is
        // enforced at application time as `MissingAttribution`).
        if let Some(a) = &self.attribution {
            if a.attribution_id.trim().is_empty() {
                return Err(IntakeError::EmptyField("attribution.attribution_id"));
            }
            if a.evidence_refs.is_empty() {
                return Err(IntakeError::EmptyField("attribution.evidence_refs"));
            }
            validate_subject(&a.attributed_subject)?;
        }
        for id in &self.supersedes {
            if id.trim().is_empty() {
                return Err(IntakeError::EmptyField("supersedes"));
            }
        }
        Ok(())
    }
}

/// One delimited section of a guidance artifact's content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub section_id: String,
    /// Section body, trimmed of surrounding blank lines (canonical form).
    pub body: String,
}

/// A parsed guidance artifact: optional preamble text plus its sections.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SectionedContent {
    pub preamble: String,
    pub sections: Vec<Section>,
}

/// Extract the section id from a marker line, if the line is one.
fn section_marker_id(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    let rest = trimmed.strip_prefix(SECTION_MARKER_PREFIX)?;
    let inner = rest.strip_suffix(SECTION_MARKER_SUFFIX)?;
    let id = inner.trim();
    if id.is_empty() {
        None
    } else {
        Some(id)
    }
}

/// Parse content into preamble + sections (canonical trimmed bodies).
pub fn parse_sections(content: &str) -> SectionedContent {
    let mut preamble_lines: Vec<&str> = Vec::new();
    let mut sections: Vec<Section> = Vec::new();
    let mut current: Option<Section> = None;
    for line in content.lines() {
        if let Some(id) = section_marker_id(line) {
            if let Some(open) = current.take() {
                sections.push(Section {
                    section_id: open.section_id,
                    body: open.body.trim().to_string(),
                });
            }
            current = Some(Section {
                section_id: id.to_string(),
                body: String::new(),
            });
        } else if let Some(open) = current.as_mut() {
            if !open.body.is_empty() {
                open.body.push('\n');
            }
            open.body.push_str(line);
        } else {
            preamble_lines.push(line);
        }
    }
    if let Some(open) = current.take() {
        sections.push(Section {
            section_id: open.section_id,
            body: open.body.trim().to_string(),
        });
    }
    SectionedContent {
        preamble: preamble_lines.join("\n").trim().to_string(),
        sections,
    }
}

pub fn render_section_marker(section_id: &str) -> String {
    format!("{SECTION_MARKER_PREFIX}{section_id} {SECTION_MARKER_SUFFIX}")
}

/// Render parsed content back to canonical text.
pub fn render_sections(sc: &SectionedContent) -> String {
    let mut out = String::new();
    if !sc.preamble.is_empty() {
        out.push_str(&sc.preamble);
        out.push_str("\n\n");
    }
    for s in &sc.sections {
        out.push_str(&render_section_marker(&s.section_id));
        out.push('\n');
        if !s.body.is_empty() {
            out.push_str(&s.body);
            out.push('\n');
        }
        out.push('\n');
    }
    out
}

/// Itemized refinement: replace exactly one section's body. Everything else —
/// preamble and sibling sections — is preserved byte-for-byte (canonical form).
/// This is the only refinement path the consumer offers; there is no
/// whole-artifact replace, per the replace-first gate ("never a wholesale
/// rewrite").
pub fn replace_section_body(
    content: &str,
    section_id: &str,
    replacement: &str,
) -> Result<String, SectionDeltaError> {
    if replacement.contains(SECTION_MARKER_PREFIX) {
        return Err(SectionDeltaError::MarkerInjection);
    }
    let mut parsed = parse_sections(content);
    if parsed.sections.is_empty() {
        return Err(SectionDeltaError::TargetNotSectioned);
    }
    let mut found = false;
    for s in parsed.sections.iter_mut() {
        if s.section_id == section_id {
            s.body = replacement.trim().to_string();
            found = true;
            break;
        }
    }
    if !found {
        return Err(SectionDeltaError::UnknownSection {
            section_id: section_id.to_string(),
        });
    }
    Ok(render_sections(&parsed))
}

/// Refinement failures mapped onto typed application rejections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SectionDeltaError {
    /// The artifact has no section markers, so no itemized refinement can be
    /// applied — a wholesale rewrite would be required and is refused.
    TargetNotSectioned,
    /// The delta names a section the artifact does not carry; the proposal
    /// must be rebased.
    UnknownSection { section_id: String },
    /// The replacement would inject a section marker.
    MarkerInjection,
}

impl fmt::Display for SectionDeltaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SectionDeltaError::TargetNotSectioned => {
                write!(f, "target artifact has no section markers")
            }
            SectionDeltaError::UnknownSection { section_id } => {
                write!(f, "target artifact has no section {section_id:?}")
            }
            SectionDeltaError::MarkerInjection => {
                write!(f, "replacement injects a section marker")
            }
        }
    }
}

impl std::error::Error for SectionDeltaError {}

#[cfg(test)]
mod tests {
    use super::*;

    const THREE_SECTIONS: &str = "# Review guidance\n\n\
        <!-- adapt:section:retry -->\nRetry failed checks up to three times.\n\n\
        <!-- adapt:section:errors -->\nOn error, stop.\n\n\
        <!-- adapt:section:scope -->\nApplies to code review.\n";

    fn sample_refine_proposal(base_digest: &str) -> SealedRemediationProposalV1 {
        SealedRemediationProposalV1 {
            contract_version: ADAPT_CONTRACT_VERSION.into(),
            proposal_id: "p-1".into(),
            intervention_target: InterventionTargetV1 {
                surface: GuidanceKind::Skill,
                subject: "review.errors".into(),
                base_guidance_id: Some("skill/review".into()),
            },
            expected_base_digest: base_digest.into(),
            mutation_kind: MutationKind::Refine,
            delta: ProposalDelta::RefineSection {
                section_id: "errors".into(),
                replacement: "On error, stop and report.".into(),
                rationale: "silent stop hides failures".into(),
            },
            evidence: vec![ProposalEvidenceRef {
                kind: EvidenceKind::EvaluatorOutcome,
                reference: "eval-42".into(),
            }],
            approval: ProposalApproval {
                approval_id: "apr-1".into(),
                action: AdaptReviewAction::ApproveRemediation,
                approved_by: "operator".into(),
                approved_at_utc: "2026-08-29T00:00:00Z".into(),
            },
            attribution: Some(InterventionAttributionV1 {
                attribution_id: "att-1".into(),
                attributed_subject: "review.errors".into(),
                evidence_refs: vec![ProposalEvidenceRef {
                    kind: EvidenceKind::RecurrenceMeasurement,
                    reference: "rec-9".into(),
                }],
            }),
            supersedes: vec![],
        }
    }

    #[test]
    fn valid_intake_passes_and_kind_delta_mismatch_fails() {
        let digest = rightkit_fs::digest::sha256_bytes(b"base");
        let mut p = sample_refine_proposal(&digest);
        assert_eq!(p.validate_intake(), Ok(()));

        p.mutation_kind = MutationKind::Append;
        assert!(matches!(
            p.validate_intake(),
            Err(IntakeError::KindDeltaMismatch { .. })
        ));
    }

    #[test]
    fn expected_base_digest_must_be_sha256_hex() {
        let p = sample_refine_proposal("not-a-digest");
        assert!(matches!(
            p.validate_intake(),
            Err(IntakeError::InvalidBaseDigest(_))
        ));
        // Uppercase hex is not canonical.
        let u = sample_refine_proposal(&"A".repeat(64));
        assert!(matches!(
            u.validate_intake(),
            Err(IntakeError::InvalidBaseDigest(_))
        ));
    }

    #[test]
    fn base_binding_is_kind_coherent() {
        let digest = rightkit_fs::digest::sha256_bytes(b"base");
        let mut p = sample_refine_proposal(&digest);
        p.intervention_target.base_guidance_id = None;
        assert!(matches!(
            p.validate_intake(),
            Err(IntakeError::BaseBindingMismatch { .. })
        ));

        let mut a = sample_refine_proposal(&digest);
        a.mutation_kind = MutationKind::Append;
        a.delta = ProposalDelta::AppendRecord {
            content: "New guidance.".into(),
            rationale: "unowned subject".into(),
        };
        // Append must not carry a base record.
        assert!(matches!(
            a.validate_intake(),
            Err(IntakeError::BaseBindingMismatch { .. })
        ));
        a.intervention_target.base_guidance_id = None;
        assert_eq!(a.validate_intake(), Ok(()));
    }

    #[test]
    fn subject_charset_is_validated() {
        assert_eq!(validate_subject("review.errors"), Ok(()));
        assert_eq!(validate_subject("deploy-notes"), Ok(()));
        assert!(validate_subject("Review").is_err());
        assert!(validate_subject("").is_err());
        assert!(validate_subject("has space").is_err());
    }

    #[test]
    fn parse_render_roundtrip_is_stable() {
        let parsed = parse_sections(THREE_SECTIONS);
        assert_eq!(parsed.preamble, "# Review guidance");
        assert_eq!(parsed.sections.len(), 3);
        assert_eq!(parsed.sections[0].section_id, "retry");
        assert_eq!(
            parsed.sections[0].body,
            "Retry failed checks up to three times."
        );

        let rendered = render_sections(&parsed);
        let reparsed = parse_sections(&rendered);
        assert_eq!(parsed, reparsed, "parse→render→parse must be stable");
    }

    #[test]
    fn refine_section_replaces_only_that_section() {
        let refined = replace_section_body(THREE_SECTIONS, "errors", "On error, stop and report.")
            .expect("section exists");
        let parsed = parse_sections(&refined);
        assert_eq!(parsed.preamble, "# Review guidance");
        assert_eq!(
            parsed.sections[0].body,
            "Retry failed checks up to three times."
        );
        assert_eq!(parsed.sections[1].body, "On error, stop and report.");
        assert_eq!(parsed.sections[2].body, "Applies to code review.");
    }

    #[test]
    fn refine_rejects_unknown_section_and_unsectioned_targets() {
        assert!(matches!(
            replace_section_body(THREE_SECTIONS, "missing", "x"),
            Err(SectionDeltaError::UnknownSection { .. })
        ));
        assert_eq!(
            replace_section_body("plain prose without markers", "any", "x"),
            Err(SectionDeltaError::TargetNotSectioned)
        );
    }

    #[test]
    fn replacement_cannot_inject_section_markers() {
        assert_eq!(
            replace_section_body(THREE_SECTIONS, "errors", "<!-- adapt:section:evil -->"),
            Err(SectionDeltaError::MarkerInjection)
        );
        let mut p = sample_refine_proposal(&rightkit_fs::digest::sha256_bytes(b"base"));
        if let ProposalDelta::RefineSection { replacement, .. } = &mut p.delta {
            *replacement = render_section_marker("evil");
        }
        assert!(matches!(
            p.validate_intake(),
            Err(IntakeError::MarkerInReplacement)
        ));
    }

    #[test]
    fn normalization_detects_restatement() {
        assert_eq!(
            normalize_guidance_text("On  error,\n STOP."),
            normalize_guidance_text("on error, stop.")
        );
        assert_ne!(
            normalize_guidance_text("On error, stop."),
            normalize_guidance_text("On error, continue.")
        );
    }

    #[test]
    fn proposal_serde_roundtrip_preserves_typed_intake() {
        let p = sample_refine_proposal(&rightkit_fs::digest::sha256_bytes(b"base"));
        let json = serde_json::to_string(&p).expect("serialize");
        let back: SealedRemediationProposalV1 = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(p, back);
        assert_eq!(
            back.mutation_kind,
            MutationKind::Refine,
            "typed mutation kind must survive the wire"
        );
    }
}
