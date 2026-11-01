//! Apply pipeline (PL-011): the single entry point that turns an approved,
//! sealed Adapt proposal into a committed guidance mutation (or a typed
//! rejection), under the host app's own authority.
//!
//! See the module doc on [`super`] for the full contract. In short:
//!
//! 1. Structural intake ([`super::proposal::SealedRemediationProposalV1::validate_intake`]).
//! 2. Attribution presence (every guidance-kind target is a mutable
//!    instruction surface; a proposal without [`super::proposal::InterventionAttributionV1`]
//!    is rejected, never applied).
//! 3. Host authority ([`super::authority::ApplicationAuthorityGate`]) — an
//!    approved proposal is evidence, never authority by itself.
//! 4. Replace-first / duplicate / contradiction checks against the current
//!    guidance surface ([`super::guidance::resolve_owners`]).
//! 5. Stale-base compare (`expected_base_digest` vs. the host-observed
//!    digest), then a second CAS at commit time so a race is also rejected as
//!    stale, never silently applied.
//! 6. Derived-projection regeneration, recorded per-projection on the
//!    receipt.
//! 7. A deterministic, replay-dedupable [`ApplicationReceipt`].
//!
//! [`apply_proposal`] is synchronous by design (small text artifacts).

use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use super::authority::{ApplicationAuthorityGate, AuthorityDecision};
use super::guidance::{
    resolve_owners, surface_digest, GuidanceRecord, GuidanceStore, GuidanceStoreError,
    NewGuidanceRecord,
};
use super::projection::{ProjectionOutcome, ProjectionRegistry};
use super::proposal::{
    guidance_content_digest, normalize_guidance_text, replace_section_body, GuidanceKind,
    MutationKind, ProposalDelta, SealedRemediationProposalV1, SectionDeltaError,
};

/// Domain separator for [`derived_receipt_id`] — mixed into the hash so a
/// receipt id can never collide with a digest computed for an unrelated
/// purpose elsewhere in the house digest scheme.
pub const APPLICATION_RECEIPT_DOMAIN: &str = "coderight.adapt_consumer.application_receipt.v1";

/// The two digests a stale-base rejection reports: what the proposal expected
/// and what the host actually observed at check/commit time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DigestBinding {
    pub expected_base_digest: String,
    pub observed_base_digest: String,
}

/// Why an attempted application was rejected. The proposal is never silently
/// dropped nor silently applied; every rejection is one of these typed kinds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RejectionKind {
    /// Structural intake failure (see [`super::proposal::IntakeError`]'s
    /// `Display`); `detail` is that error's rendered message.
    IntakeInvalid { detail: String },
    /// A guidance-kind target arrived without
    /// [`super::proposal::InterventionAttributionV1`].
    MissingAttribution,
    /// the host app's own policy refused application.
    AuthorityDenied { reason: String },
    /// The proposal did not refine the narrowest existing owner (or, for an
    /// append, a subject that already has an owner) — replace-first was
    /// violated.
    ReplaceFirstViolation { detail: String },
    /// The resulting content, normalized, restates existing guidance
    /// verbatim.
    Duplicate,
    /// More than one exact-subject owner already exists on the target
    /// surface, or the append supersedes a record (an append that retires
    /// something is a refine in disguise).
    Contradiction { detail: String },
    /// `expected_base_digest` did not match what the host observed, either at
    /// the pre-commit check or at the commit-time CAS (a race).
    StaleBase { binding: DigestBinding },
    /// The itemized section delta could not be applied to the target
    /// artifact's current content.
    SectionDelta { detail: String },
    /// The guidance store reported a failure unrelated to CAS staleness.
    StoreError { detail: String },
}

/// The result of one application attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ApplicationOutcome {
    Applied {
        guidance_id: String,
        digest: String,
        mutation_kind: MutationKind,
        projections: Vec<ProjectionOutcome>,
    },
    Rejected {
        rejection: RejectionKind,
    },
}

impl ApplicationOutcome {
    pub fn is_applied(&self) -> bool {
        matches!(self, ApplicationOutcome::Applied { .. })
    }
}

/// Every application attempt, applied or rejected, produces one of these.
/// `receipt_id` is derived from the proposal's identity (see
/// [`derived_receipt_id`]) so a replayed proposal dedupes to the same receipt
/// instead of being reprocessed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplicationReceipt {
    pub receipt_id: String,
    pub proposal_id: String,
    pub surface: GuidanceKind,
    pub subject: String,
    pub outcome: ApplicationOutcome,
}

/// Deterministic, replay-dedupable receipt id derived from the proposal's
/// identity — not from the outcome. Two calls to [`apply_proposal`] with a
/// byte-identical proposal always derive the same id, so a receipt log keyed
/// by this id lets a replayed submission short-circuit to the cached receipt
/// instead of re-running the pipeline (which would otherwise fail a second
/// CAS check after the first application already moved the base digest).
pub fn derived_receipt_id(proposal: &SealedRemediationProposalV1) -> String {
    let delta_fingerprint = match &proposal.delta {
        ProposalDelta::RefineSection {
            section_id,
            replacement,
            ..
        } => format!("refine\0{section_id}\0{replacement}"),
        ProposalDelta::AppendRecord { content, .. } => format!("append\0{content}"),
    };
    let canonical = format!(
        "{APPLICATION_RECEIPT_DOMAIN}\0{}\0{}\0{}\0{}\0{}\0{}\0{}",
        proposal.contract_version,
        proposal.proposal_id,
        proposal.intervention_target.surface.as_str(),
        proposal.intervention_target.subject,
        proposal
            .intervention_target
            .base_guidance_id
            .as_deref()
            .unwrap_or(""),
        proposal.expected_base_digest,
        delta_fingerprint,
    );
    rightkit_fs::digest::sha256_bytes(canonical.as_bytes())
}

/// A log of application receipts, keyed by [`ApplicationReceipt::receipt_id`].
/// Lets [`apply_proposal`] treat a replayed proposal idempotently.
pub trait ApplicationReceiptLog: Send + Sync {
    fn get(&self, receipt_id: &str) -> Option<ApplicationReceipt>;
    fn record(&self, receipt: ApplicationReceipt);
}

/// Reference in-memory receipt log used by tests and as an integration
/// template.
#[derive(Debug, Default)]
pub struct InMemoryApplicationReceiptLog {
    receipts: Mutex<HashMap<String, ApplicationReceipt>>,
}

impl InMemoryApplicationReceiptLog {
    pub fn new() -> Self {
        Self::default()
    }
}

impl ApplicationReceiptLog for InMemoryApplicationReceiptLog {
    fn get(&self, receipt_id: &str) -> Option<ApplicationReceipt> {
        self.receipts
            .lock()
            .expect("receipt log mutex poisoned")
            .get(receipt_id)
            .cloned()
    }

    fn record(&self, receipt: ApplicationReceipt) {
        self.receipts
            .lock()
            .expect("receipt log mutex poisoned")
            .insert(receipt.receipt_id.clone(), receipt);
    }
}

fn rejected(
    proposal: &SealedRemediationProposalV1,
    receipt_id: String,
    rejection: RejectionKind,
) -> ApplicationReceipt {
    ApplicationReceipt {
        receipt_id,
        proposal_id: proposal.proposal_id.clone(),
        surface: proposal.intervention_target.surface,
        subject: proposal.intervention_target.subject.clone(),
        outcome: ApplicationOutcome::Rejected { rejection },
    }
}

/// Apply one sealed, approved proposal against `store` under `authority`,
/// regenerating every projection in `projections` that reads the mutated
/// surface, and recording the attempt in `receipt_log`. Synchronous by
/// design. Never panics on a malformed or hostile proposal — every failure
/// mode is a typed [`RejectionKind`] on the returned receipt.
pub fn apply_proposal(
    proposal: &SealedRemediationProposalV1,
    store: &dyn GuidanceStore,
    authority: &dyn ApplicationAuthorityGate,
    projections: &ProjectionRegistry,
    receipt_log: &dyn ApplicationReceiptLog,
) -> ApplicationReceipt {
    let receipt_id = derived_receipt_id(proposal);

    // Replay dedup: an identical proposal already processed returns the
    // cached receipt rather than re-running the pipeline.
    if let Some(cached) = receipt_log.get(&receipt_id) {
        return cached;
    }

    let receipt = apply_proposal_inner(proposal, store, authority, projections, receipt_id);
    receipt_log.record(receipt.clone());
    receipt
}

fn apply_proposal_inner(
    proposal: &SealedRemediationProposalV1,
    store: &dyn GuidanceStore,
    authority: &dyn ApplicationAuthorityGate,
    projections: &ProjectionRegistry,
    receipt_id: String,
) -> ApplicationReceipt {
    // 1. Structural intake.
    if let Err(e) = proposal.validate_intake() {
        return rejected(
            proposal,
            receipt_id,
            RejectionKind::IntakeInvalid {
                detail: e.to_string(),
            },
        );
    }

    // 2. Attribution required — every PL-011 surface is a mutable
    // instruction surface (integration doc §10 / Adapt canon §6.9).
    if proposal.attribution.is_none() {
        return rejected(proposal, receipt_id, RejectionKind::MissingAttribution);
    }

    // 3. Host authority — an approved proposal is evidence, never authority.
    match authority.authorize(proposal) {
        AuthorityDecision::Denied { reason } => {
            return rejected(
                proposal,
                receipt_id,
                RejectionKind::AuthorityDenied { reason },
            );
        }
        AuthorityDecision::Granted { .. } => {}
    }

    let surface = proposal.intervention_target.surface;
    let subject = &proposal.intervention_target.subject;

    let records = match store.list() {
        Ok(records) => records,
        Err(e) => {
            return rejected(
                proposal,
                receipt_id,
                RejectionKind::StoreError {
                    detail: e.to_string(),
                },
            );
        }
    };

    let owners = resolve_owners(&records, surface, subject);

    // 4. Contradiction: two exact-subject owners already compete for this
    // subject on the target surface.
    if owners.exact.len() > 1 {
        let competing: Vec<String> = owners.exact.iter().map(|r| r.guidance_id.clone()).collect();
        return rejected(
            proposal,
            receipt_id,
            RejectionKind::Contradiction {
                detail: format!(
                    "subject {subject:?} already has {} competing exact owners: {}",
                    owners.exact.len(),
                    competing.join(", ")
                ),
            },
        );
    }

    match proposal.mutation_kind {
        MutationKind::Refine => apply_refine(
            proposal,
            store,
            projections,
            &records,
            owners.narrowest,
            receipt_id,
        ),
        MutationKind::Append => apply_append(
            proposal,
            store,
            projections,
            &records,
            owners.narrowest,
            receipt_id,
        ),
    }
}

fn apply_refine(
    proposal: &SealedRemediationProposalV1,
    store: &dyn GuidanceStore,
    projections: &ProjectionRegistry,
    records: &[GuidanceRecord],
    narrowest: Option<&GuidanceRecord>,
    receipt_id: String,
) -> ApplicationReceipt {
    let surface = proposal.intervention_target.surface;
    let subject = proposal.intervention_target.subject.clone();
    // Intake already guarantees Some(base_guidance_id) for Refine.
    let base_guidance_id = proposal
        .intervention_target
        .base_guidance_id
        .as_deref()
        .unwrap_or_default();
    let (section_id, replacement) = match &proposal.delta {
        ProposalDelta::RefineSection {
            section_id,
            replacement,
            ..
        } => (section_id.as_str(), replacement.as_str()),
        ProposalDelta::AppendRecord { .. } => {
            unreachable!("mutation_kind coherence enforced at intake")
        }
    };

    // Replace-first: the proposal must target the narrowest existing owner,
    // not append behind its back and not refine some other record.
    let owner = match narrowest {
        None => {
            return rejected(
                proposal,
                receipt_id,
                RejectionKind::ReplaceFirstViolation {
                    detail: format!(
                        "no existing guidance owns subject {subject:?} on surface {}; a refine target must exist",
                        surface.as_str()
                    ),
                },
            );
        }
        Some(owner) => owner,
    };
    if owner.guidance_id != base_guidance_id {
        return rejected(
            proposal,
            receipt_id,
            RejectionKind::ReplaceFirstViolation {
                detail: format!(
                    "narrowest owner of subject {subject:?} is {:?}, but the proposal targets {base_guidance_id:?}",
                    owner.guidance_id
                ),
            },
        );
    }

    // Stale-base pre-check (a second CAS happens at commit time).
    if owner.digest != proposal.expected_base_digest {
        return rejected(
            proposal,
            receipt_id,
            RejectionKind::StaleBase {
                binding: DigestBinding {
                    expected_base_digest: proposal.expected_base_digest.clone(),
                    observed_base_digest: owner.digest.clone(),
                },
            },
        );
    }

    let new_content = match replace_section_body(&owner.content, section_id, replacement) {
        Ok(content) => content,
        Err(e) => {
            let detail = section_delta_detail(&e);
            return rejected(proposal, receipt_id, RejectionKind::SectionDelta { detail });
        }
    };

    // Duplicate: the itemized change, once applied, restates existing
    // content verbatim (mechanical normalization, not semantic equality).
    if normalize_guidance_text(&new_content) == normalize_guidance_text(&owner.content) {
        return rejected(proposal, receipt_id, RejectionKind::Duplicate);
    }

    let stored = match store.replace_if_unchanged(&owner.guidance_id, &owner.digest, &new_content) {
        Ok(stored) => stored,
        Err(GuidanceStoreError::CasMismatch {
            observed_base_digest,
            ..
        }) => {
            return rejected(
                proposal,
                receipt_id,
                RejectionKind::StaleBase {
                    binding: DigestBinding {
                        expected_base_digest: proposal.expected_base_digest.clone(),
                        observed_base_digest,
                    },
                },
            );
        }
        Err(e) => {
            return rejected(
                proposal,
                receipt_id,
                RejectionKind::StoreError {
                    detail: e.to_string(),
                },
            );
        }
    };

    finish_applied(
        proposal,
        store,
        projections,
        records,
        receipt_id,
        stored.guidance_id.clone(),
        stored.digest.clone(),
        stored,
    )
}

fn apply_append(
    proposal: &SealedRemediationProposalV1,
    store: &dyn GuidanceStore,
    projections: &ProjectionRegistry,
    records: &[GuidanceRecord],
    narrowest: Option<&GuidanceRecord>,
    receipt_id: String,
) -> ApplicationReceipt {
    let surface = proposal.intervention_target.surface;
    let subject = proposal.intervention_target.subject.clone();
    let content = match &proposal.delta {
        ProposalDelta::AppendRecord { content, .. } => content.as_str(),
        ProposalDelta::RefineSection { .. } => {
            unreachable!("mutation_kind coherence enforced at intake")
        }
    };

    // An append that supersedes anything is a refine in disguise.
    if !proposal.supersedes.is_empty() {
        return rejected(
            proposal,
            receipt_id,
            RejectionKind::ReplaceFirstViolation {
                detail: format!(
                    "append declares {} superseded record(s); an append must retire nothing, refine instead",
                    proposal.supersedes.len()
                ),
            },
        );
    }

    // Replace-first: append is only valid when nothing owns the subject.
    if let Some(owner) = narrowest {
        return rejected(
            proposal,
            receipt_id,
            RejectionKind::ReplaceFirstViolation {
                detail: format!(
                    "subject {subject:?} is already owned by {:?} on surface {}; refine that record instead of appending",
                    owner.guidance_id,
                    surface.as_str()
                ),
            },
        );
    }

    // Duplicate: verbatim restatement of any existing record on this surface.
    let normalized_new = normalize_guidance_text(content);
    if records
        .iter()
        .filter(|r| r.kind == surface)
        .any(|r| normalize_guidance_text(&r.content) == normalized_new)
    {
        return rejected(proposal, receipt_id, RejectionKind::Duplicate);
    }

    // Stale-base pre-check against the whole-surface digest.
    let observed_surface_digest = surface_digest(records, surface);
    if observed_surface_digest != proposal.expected_base_digest {
        return rejected(
            proposal,
            receipt_id,
            RejectionKind::StaleBase {
                binding: DigestBinding {
                    expected_base_digest: proposal.expected_base_digest.clone(),
                    observed_base_digest: observed_surface_digest,
                },
            },
        );
    }

    let content_digest = guidance_content_digest(content);
    let guidance_id = format!("{}/{}#{}", surface.as_str(), subject, content_digest);
    let new_record = NewGuidanceRecord {
        guidance_id: guidance_id.clone(),
        kind: surface,
        subject: subject.clone(),
        content: content.to_string(),
    };

    let stored = match store.append_if_surface_unchanged(
        &proposal.expected_base_digest,
        surface,
        new_record,
    ) {
        Ok(stored) => stored,
        Err(GuidanceStoreError::CasMismatch {
            observed_base_digest,
            ..
        }) => {
            return rejected(
                proposal,
                receipt_id,
                RejectionKind::StaleBase {
                    binding: DigestBinding {
                        expected_base_digest: proposal.expected_base_digest.clone(),
                        observed_base_digest,
                    },
                },
            );
        }
        Err(GuidanceStoreError::AlreadyExists { .. }) => {
            return rejected(proposal, receipt_id, RejectionKind::Duplicate);
        }
        Err(e) => {
            return rejected(
                proposal,
                receipt_id,
                RejectionKind::StoreError {
                    detail: e.to_string(),
                },
            );
        }
    };

    finish_applied(
        proposal,
        store,
        projections,
        records,
        receipt_id,
        stored.guidance_id.clone(),
        stored.digest.clone(),
        stored,
    )
}

/// Shared tail: regenerate derived projections against the post-mutation
/// surface and assemble the `Applied` receipt.
fn finish_applied(
    proposal: &SealedRemediationProposalV1,
    store: &dyn GuidanceStore,
    projections: &ProjectionRegistry,
    pre_mutation_records: &[GuidanceRecord],
    receipt_id: String,
    guidance_id: String,
    digest: String,
    _stored: GuidanceRecord,
) -> ApplicationReceipt {
    let surface = proposal.intervention_target.surface;
    // Regenerate from the freshest observable surface; fall back to the
    // pre-mutation snapshot only if the store cannot be re-listed (the store
    // failure itself never hides the successful commit — regeneration
    // failure is recorded per-projection, not raised as an error here).
    let post_records = store
        .list()
        .unwrap_or_else(|_| pre_mutation_records.to_vec());
    let outcomes = projections.regenerate_for(surface, &post_records);

    ApplicationReceipt {
        receipt_id,
        proposal_id: proposal.proposal_id.clone(),
        surface,
        subject: proposal.intervention_target.subject.clone(),
        outcome: ApplicationOutcome::Applied {
            guidance_id,
            digest,
            mutation_kind: proposal.mutation_kind,
            projections: outcomes,
        },
    }
}

fn section_delta_detail(e: &SectionDeltaError) -> String {
    e.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapt_consumer::authority::{DenyAllAuthority, PolicyRule, StaticPolicyAuthority};
    use crate::adapt_consumer::guidance::InMemoryGuidanceStore;
    use crate::adapt_consumer::projection::GuidanceProjection;
    use crate::adapt_consumer::proposal::{
        AdaptReviewAction, EvidenceKind, InterventionAttributionV1, InterventionTargetV1,
        ProposalApproval, ProposalDelta, ProposalEvidenceRef, SealedRemediationProposalV1,
        ADAPT_CONTRACT_VERSION,
    };
    use std::sync::Arc;

    const THREE_SECTIONS: &str = "# Review guidance\n\n\
        <!-- adapt:section:retry -->\nRetry failed checks up to three times.\n\n\
        <!-- adapt:section:errors -->\nOn error, stop.\n\n\
        <!-- adapt:section:scope -->\nApplies to code review.\n";

    fn seeded_store() -> InMemoryGuidanceStore {
        InMemoryGuidanceStore::from_records(vec![super::super::guidance::NewGuidanceRecord {
            guidance_id: "skill/review".into(),
            kind: GuidanceKind::Skill,
            subject: "review".into(),
            content: THREE_SECTIONS.into(),
        }])
    }

    fn permissive_authority() -> StaticPolicyAuthority {
        StaticPolicyAuthority::new(vec![
            PolicyRule {
                kind: GuidanceKind::Skill,
                subject_prefix: "review".into(),
                max_delta_bytes: 4096,
            },
            // The append/replace-first cases below target an unowned subject,
            // which must still clear the authority gate so the pipeline stage
            // under test is the one that decides the outcome.
            PolicyRule {
                kind: GuidanceKind::Skill,
                subject_prefix: "monitoring".into(),
                max_delta_bytes: 4096,
            },
        ])
    }

    fn base_refine_proposal(base_digest: &str, replacement: &str) -> SealedRemediationProposalV1 {
        SealedRemediationProposalV1 {
            contract_version: ADAPT_CONTRACT_VERSION.into(),
            proposal_id: "p-1".into(),
            intervention_target: InterventionTargetV1 {
                surface: GuidanceKind::Skill,
                subject: "review".into(),
                base_guidance_id: Some("skill/review".into()),
            },
            expected_base_digest: base_digest.into(),
            mutation_kind: MutationKind::Refine,
            delta: ProposalDelta::RefineSection {
                section_id: "errors".into(),
                replacement: replacement.into(),
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
                attributed_subject: "review".into(),
                evidence_refs: vec![ProposalEvidenceRef {
                    kind: EvidenceKind::RecurrenceMeasurement,
                    reference: "rec-9".into(),
                }],
            }),
            supersedes: vec![],
        }
    }

    fn base_append_proposal(
        surface_digest: &str,
        subject: &str,
        content: &str,
    ) -> SealedRemediationProposalV1 {
        SealedRemediationProposalV1 {
            contract_version: ADAPT_CONTRACT_VERSION.into(),
            proposal_id: "p-append-1".into(),
            intervention_target: InterventionTargetV1 {
                surface: GuidanceKind::Skill,
                subject: subject.into(),
                base_guidance_id: None,
            },
            expected_base_digest: surface_digest.into(),
            mutation_kind: MutationKind::Append,
            delta: ProposalDelta::AppendRecord {
                content: content.into(),
                rationale: "unowned subject".into(),
            },
            evidence: vec![ProposalEvidenceRef {
                kind: EvidenceKind::UserCorrection,
                reference: "u-1".into(),
            }],
            approval: ProposalApproval {
                approval_id: "apr-append-1".into(),
                action: AdaptReviewAction::ApproveRemediation,
                approved_by: "operator".into(),
                approved_at_utc: "2026-08-29T00:00:00Z".into(),
            },
            attribution: Some(InterventionAttributionV1 {
                attribution_id: "att-append-1".into(),
                attributed_subject: subject.into(),
                evidence_refs: vec![ProposalEvidenceRef {
                    kind: EvidenceKind::ExplicitReference,
                    reference: "ref-1".into(),
                }],
            }),
            supersedes: vec![],
        }
    }

    #[test]
    fn successful_replace_commits_and_regenerates_projections() {
        let store = seeded_store();
        let base_digest = store.list().unwrap()[0].digest.clone();
        let authority = permissive_authority();
        let mut registry = ProjectionRegistry::new();

        struct Counting(std::sync::atomic::AtomicUsize);
        impl GuidanceProjection for Counting {
            fn projection_id(&self) -> &str {
                "counting"
            }
            fn applies_to(&self) -> Option<GuidanceKind> {
                None
            }
            fn regenerate(
                &self,
                _k: GuidanceKind,
                records: &[GuidanceRecord],
            ) -> Result<String, String> {
                self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(rightkit_fs::digest::sha256_bytes(
                    format!("{}", records.len()).as_bytes(),
                ))
            }
        }
        registry.register(Arc::new(Counting(std::sync::atomic::AtomicUsize::new(0))));

        let log = InMemoryApplicationReceiptLog::new();
        let proposal = base_refine_proposal(&base_digest, "On error, stop and report.");
        let receipt = apply_proposal(&proposal, &store, &authority, &registry, &log);

        match &receipt.outcome {
            ApplicationOutcome::Applied {
                guidance_id,
                projections,
                mutation_kind,
                ..
            } => {
                assert_eq!(guidance_id, "skill/review");
                assert_eq!(*mutation_kind, MutationKind::Refine);
                assert_eq!(projections.len(), 1);
                assert!(!projections[0].is_failed());
            }
            other => panic!("expected Applied, got {other:?}"),
        }

        let stored = store.list().unwrap();
        assert!(stored[0].content.contains("On error, stop and report."));
        assert!(!stored[0].content.contains("On error, stop.\n"));
    }

    #[test]
    fn replace_first_violation_when_no_owner_exists() {
        let store = InMemoryGuidanceStore::new();
        let authority = permissive_authority();
        let registry = ProjectionRegistry::new();
        let log = InMemoryApplicationReceiptLog::new();
        let mut proposal = base_refine_proposal(&"0".repeat(64), "New text.");
        proposal.intervention_target.subject = "monitoring".into();
        proposal.intervention_target.base_guidance_id = Some("skill/ghost".into());

        let receipt = apply_proposal(&proposal, &store, &authority, &registry, &log);
        match receipt.outcome {
            ApplicationOutcome::Rejected {
                rejection: RejectionKind::ReplaceFirstViolation { .. },
            } => {}
            other => panic!("expected ReplaceFirstViolation, got {other:?}"),
        }
    }

    #[test]
    fn replace_first_violation_when_target_is_not_the_narrowest_owner() {
        let store = InMemoryGuidanceStore::from_records(vec![
            super::super::guidance::NewGuidanceRecord {
                guidance_id: "skill/broad".into(),
                kind: GuidanceKind::Skill,
                subject: "review".into(),
                content: THREE_SECTIONS.into(),
            },
            super::super::guidance::NewGuidanceRecord {
                guidance_id: "skill/narrow".into(),
                kind: GuidanceKind::Skill,
                subject: "review.errors".into(),
                content: "<!-- adapt:section:core -->\nOn error, stop.\n".into(),
            },
        ]);
        let broad_digest = store
            .list()
            .unwrap()
            .iter()
            .find(|r| r.guidance_id == "skill/broad")
            .unwrap()
            .digest
            .clone();
        let authority = permissive_authority();
        let registry = ProjectionRegistry::new();
        let log = InMemoryApplicationReceiptLog::new();

        let mut proposal = base_refine_proposal(&broad_digest, "On error, stop and report.");
        proposal.intervention_target.subject = "review.errors".into();
        proposal.intervention_target.base_guidance_id = Some("skill/broad".into());

        let receipt = apply_proposal(&proposal, &store, &authority, &registry, &log);
        match receipt.outcome {
            ApplicationOutcome::Rejected {
                rejection: RejectionKind::ReplaceFirstViolation { .. },
            } => {}
            other => panic!(
                "expected ReplaceFirstViolation (must target narrowest owner), got {other:?}"
            ),
        }
    }

    #[test]
    fn duplicate_rejected_when_replacement_restates_existing_section() {
        let store = seeded_store();
        let base_digest = store.list().unwrap()[0].digest.clone();
        let authority = permissive_authority();
        let registry = ProjectionRegistry::new();
        let log = InMemoryApplicationReceiptLog::new();

        // Restates the existing "errors" section body verbatim (mod whitespace/case).
        let proposal = base_refine_proposal(&base_digest, "on error, stop.");
        let receipt = apply_proposal(&proposal, &store, &authority, &registry, &log);
        assert_eq!(
            receipt.outcome,
            ApplicationOutcome::Rejected {
                rejection: RejectionKind::Duplicate
            }
        );
        // Never silently applied.
        assert_eq!(store.list().unwrap()[0].digest, base_digest);
    }

    #[test]
    fn contradiction_rejected_when_two_exact_owners_compete() {
        let store = InMemoryGuidanceStore::from_records(vec![
            super::super::guidance::NewGuidanceRecord {
                guidance_id: "skill/a".into(),
                kind: GuidanceKind::Skill,
                subject: "review".into(),
                content: THREE_SECTIONS.into(),
            },
            super::super::guidance::NewGuidanceRecord {
                guidance_id: "skill/b".into(),
                kind: GuidanceKind::Skill,
                subject: "review".into(),
                content: THREE_SECTIONS.replace("stop.", "halt."),
            },
        ]);
        let digest_a = store
            .list()
            .unwrap()
            .iter()
            .find(|r| r.guidance_id == "skill/a")
            .unwrap()
            .digest
            .clone();
        let authority = permissive_authority();
        let registry = ProjectionRegistry::new();
        let log = InMemoryApplicationReceiptLog::new();

        let proposal = base_refine_proposal(&digest_a, "On error, stop and report.");
        let receipt = apply_proposal(&proposal, &store, &authority, &registry, &log);
        match receipt.outcome {
            ApplicationOutcome::Rejected {
                rejection: RejectionKind::Contradiction { .. },
            } => {}
            other => panic!("expected Contradiction, got {other:?}"),
        }
    }

    #[test]
    fn stale_base_rejected_and_store_left_unchanged() {
        let store = seeded_store();
        let authority = permissive_authority();
        let registry = ProjectionRegistry::new();
        let log = InMemoryApplicationReceiptLog::new();

        let original = store.list().unwrap()[0].clone();
        let proposal = base_refine_proposal(&"0".repeat(64), "On error, stop and report.");
        let receipt = apply_proposal(&proposal, &store, &authority, &registry, &log);
        match receipt.outcome {
            ApplicationOutcome::Rejected {
                rejection: RejectionKind::StaleBase { binding },
            } => {
                assert_eq!(binding.expected_base_digest, "0".repeat(64));
                assert_eq!(binding.observed_base_digest, original.digest);
            }
            other => panic!("expected StaleBase, got {other:?}"),
        }
        assert_eq!(store.list().unwrap()[0], original);
    }

    #[test]
    fn authority_denial_rejects_without_touching_store() {
        let store = seeded_store();
        let original = store.list().unwrap();
        let base_digest = original[0].digest.clone();
        let registry = ProjectionRegistry::new();
        let log = InMemoryApplicationReceiptLog::new();

        let proposal = base_refine_proposal(&base_digest, "On error, stop and report.");
        let receipt = apply_proposal(&proposal, &store, &DenyAllAuthority, &registry, &log);
        match receipt.outcome {
            ApplicationOutcome::Rejected {
                rejection: RejectionKind::AuthorityDenied { .. },
            } => {}
            other => panic!("expected AuthorityDenied, got {other:?}"),
        }
        assert_eq!(store.list().unwrap(), original);
    }

    #[test]
    fn missing_attribution_is_rejected() {
        let store = seeded_store();
        let base_digest = store.list().unwrap()[0].digest.clone();
        let authority = permissive_authority();
        let registry = ProjectionRegistry::new();
        let log = InMemoryApplicationReceiptLog::new();

        let mut proposal = base_refine_proposal(&base_digest, "On error, stop and report.");
        proposal.attribution = None;
        let receipt = apply_proposal(&proposal, &store, &authority, &registry, &log);
        assert_eq!(
            receipt.outcome,
            ApplicationOutcome::Rejected {
                rejection: RejectionKind::MissingAttribution
            }
        );
    }

    #[test]
    fn append_succeeds_when_subject_is_unowned() {
        let store = seeded_store(); // owns "review", not "monitoring"
        let surface_digest = surface_digest(&store.list().unwrap(), GuidanceKind::Skill);
        let authority = permissive_authority();
        let registry = ProjectionRegistry::new();
        let log = InMemoryApplicationReceiptLog::new();

        let proposal =
            base_append_proposal(&surface_digest, "monitoring", "Monitor deploys hourly.");
        let receipt = apply_proposal(&proposal, &store, &authority, &registry, &log);
        match &receipt.outcome {
            ApplicationOutcome::Applied {
                guidance_id,
                mutation_kind,
                ..
            } => {
                assert_eq!(*mutation_kind, MutationKind::Append);
                assert!(store
                    .list()
                    .unwrap()
                    .iter()
                    .any(|r| &r.guidance_id == guidance_id));
            }
            other => panic!("expected Applied, got {other:?}"),
        }
    }

    #[test]
    fn append_rejected_replace_first_when_subject_already_owned() {
        let store = seeded_store();
        let surface_digest = surface_digest(&store.list().unwrap(), GuidanceKind::Skill);
        let authority = permissive_authority();
        let registry = ProjectionRegistry::new();
        let log = InMemoryApplicationReceiptLog::new();

        let proposal = base_append_proposal(
            &surface_digest,
            "review.errors",
            "On error, stop and report.",
        );
        let receipt = apply_proposal(&proposal, &store, &authority, &registry, &log);
        match receipt.outcome {
            ApplicationOutcome::Rejected {
                rejection: RejectionKind::ReplaceFirstViolation { .. },
            } => {}
            other => panic!("expected ReplaceFirstViolation, got {other:?}"),
        }
    }

    #[test]
    fn append_rejected_duplicate_when_content_restates_existing_record() {
        let store = seeded_store();
        let surface_digest = surface_digest(&store.list().unwrap(), GuidanceKind::Skill);
        let authority = permissive_authority();
        let registry = ProjectionRegistry::new();
        let log = InMemoryApplicationReceiptLog::new();

        let proposal = base_append_proposal(&surface_digest, "monitoring", THREE_SECTIONS);
        let receipt = apply_proposal(&proposal, &store, &authority, &registry, &log);
        assert_eq!(
            receipt.outcome,
            ApplicationOutcome::Rejected {
                rejection: RejectionKind::Duplicate
            }
        );
    }

    #[test]
    fn append_declaring_supersedes_is_replace_first_violation() {
        let store = seeded_store();
        let surface_digest = surface_digest(&store.list().unwrap(), GuidanceKind::Skill);
        let authority = permissive_authority();
        let registry = ProjectionRegistry::new();
        let log = InMemoryApplicationReceiptLog::new();

        let mut proposal =
            base_append_proposal(&surface_digest, "monitoring", "Monitor deploys hourly.");
        proposal.supersedes = vec!["skill/review".into()];
        let receipt = apply_proposal(&proposal, &store, &authority, &registry, &log);
        match receipt.outcome {
            ApplicationOutcome::Rejected {
                rejection: RejectionKind::ReplaceFirstViolation { .. },
            } => {}
            other => panic!("expected ReplaceFirstViolation, got {other:?}"),
        }
    }

    #[test]
    fn receipt_id_is_deterministic_and_replay_dedupes() {
        let store = seeded_store();
        let base_digest = store.list().unwrap()[0].digest.clone();
        let authority = permissive_authority();
        let registry = ProjectionRegistry::new();
        let log = InMemoryApplicationReceiptLog::new();

        let proposal = base_refine_proposal(&base_digest, "On error, stop and report.");
        let id_before_apply = derived_receipt_id(&proposal);

        let first = apply_proposal(&proposal, &store, &authority, &registry, &log);
        assert_eq!(first.receipt_id, id_before_apply);
        assert!(first.outcome.is_applied());

        // Replay: the store's base digest has now moved, so a naive re-run
        // would hit StaleBase — but the receipt log short-circuits the
        // identical proposal to the cached (still Applied) receipt.
        let second = apply_proposal(&proposal, &store, &authority, &registry, &log);
        assert_eq!(
            second, first,
            "identical proposal replays to the identical cached receipt"
        );

        // A different proposal (different replacement) derives a different id.
        let other = base_refine_proposal(&base_digest, "On error, stop and notify.");
        assert_ne!(derived_receipt_id(&other), id_before_apply);
    }
}
